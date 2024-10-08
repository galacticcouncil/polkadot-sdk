// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! [`PeerStore`] manages peer reputations and provides connection candidates to
//! [`crate::protocol_controller::ProtocolController`].

use crate::service::{metrics::PeerStoreMetrics, traits::PeerStore as PeerStoreT};

use libp2p::PeerId;
use log::trace;
use parking_lot::Mutex;
use partial_sort::PartialSort;
use prometheus_endpoint::Registry;
use sc_network_common::{role::ObservedRole, types::ReputationChange};
use std::{
	cmp::{Ord, Ordering, PartialOrd},
	collections::{hash_map::Entry, HashMap, HashSet},
	fmt::Debug,
	sync::Arc,
	time::{Duration, Instant},
};
use wasm_timer::Delay;

/// Log target for this file.
pub const LOG_TARGET: &str = "peerset";

/// We don't accept nodes whose reputation is under this value.
pub const BANNED_THRESHOLD: i32 = 71 * (i32::MIN / 100);
/// Reputation change for a node when we get disconnected from it.
const DISCONNECT_REPUTATION_CHANGE: i32 = -256;
/// Relative decrement of a reputation value that is applied every second. I.e., for inverse
/// decrement of 200 we decrease absolute value of the reputation by 1/200.
///
/// This corresponds to a factor of `k = 0.955`, where k = 1 - 1 / INVERSE_DECREMENT.
///
/// It takes ~ `ln(0.5) / ln(k)` seconds to reduce the reputation by half, or 138.63 seconds for the
/// values above.
///
/// In this setup:
/// - `i32::MAX` becomes 0 in exactly 3544 seconds, or approximately 59 minutes
/// - `i32::MIN` becomes 0 in exactly 3544 seconds, or approximately 59 minutes
/// - `i32::MIN` escapes the banned threshold in 69 seconds
const INVERSE_DECREMENT: i32 = 200;
/// Amount of time between the moment we last updated the [`PeerStore`] entry and the moment we
/// remove it, once the reputation value reaches 0.
const FORGET_AFTER: Duration = Duration::from_secs(3600);

/// Trait describing the required functionality from a `Peerset` handle.
pub trait ProtocolHandle: Debug + Send + Sync {
	/// Disconnect peer.
	fn disconnect_peer(&self, peer_id: sc_network_types::PeerId);
}

/// Trait providing peer reputation management and connection candidates.
pub trait PeerStoreProvider: Debug + Send + Sync {
	/// Check whether the peer is banned.
	fn is_banned(&self, peer_id: &sc_network_types::PeerId) -> bool;

	/// Register a protocol handle to disconnect peers whose reputation drops below the threshold.
	fn register_protocol(&self, protocol_handle: Arc<dyn ProtocolHandle>);

	/// Report peer disconnection for reputation adjustment.
	fn report_disconnect(&self, peer_id: sc_network_types::PeerId);

	/// Adjust peer reputation.
	fn report_peer(&self, peer_id: sc_network_types::PeerId, change: ReputationChange);

	/// Set peer role.
	fn set_peer_role(&self, peer_id: &sc_network_types::PeerId, role: ObservedRole);

	/// Get peer reputation.
	fn peer_reputation(&self, peer_id: &sc_network_types::PeerId) -> i32;

	/// Get peer role, if available.
	fn peer_role(&self, peer_id: &sc_network_types::PeerId) -> Option<ObservedRole>;

	/// Get candidates with highest reputations for initiating outgoing connections.
	fn outgoing_candidates(
		&self,
		count: usize,
		ignored: HashSet<sc_network_types::PeerId>,
	) -> Vec<sc_network_types::PeerId>;

	/// Add known peer.
	fn add_known_peer(&self, peer_id: sc_network_types::PeerId);
}

/// Actual implementation of peer reputations and connection candidates provider.
#[derive(Debug, Clone)]
pub struct PeerStoreHandle {
	inner: Arc<Mutex<PeerStoreInner>>,
}

impl PeerStoreProvider for PeerStoreHandle {
	fn is_banned(&self, peer_id: &sc_network_types::PeerId) -> bool {
		self.inner.lock().is_banned(&peer_id.into())
	}

	fn register_protocol(&self, protocol_handle: Arc<dyn ProtocolHandle>) {
		self.inner.lock().register_protocol(protocol_handle);
	}

	fn report_disconnect(&self, peer_id: sc_network_types::PeerId) {
		let mut inner = self.inner.lock();
		inner.report_disconnect(peer_id.into())
	}

	fn report_peer(&self, peer_id: sc_network_types::PeerId, change: ReputationChange) {
		let mut inner = self.inner.lock();
		inner.report_peer(peer_id.into(), change)
	}

	fn set_peer_role(&self, peer_id: &sc_network_types::PeerId, role: ObservedRole) {
		let mut inner = self.inner.lock();
		inner.set_peer_role(&peer_id.into(), role)
	}

	fn peer_reputation(&self, peer_id: &sc_network_types::PeerId) -> i32 {
		self.inner.lock().peer_reputation(&peer_id.into())
	}

	fn peer_role(&self, peer_id: &sc_network_types::PeerId) -> Option<ObservedRole> {
		self.inner.lock().peer_role(&peer_id.into())
	}

	fn outgoing_candidates(
		&self,
		count: usize,
		ignored: HashSet<sc_network_types::PeerId>,
	) -> Vec<sc_network_types::PeerId> {
		self.inner
			.lock()
			.outgoing_candidates(count, ignored.iter().map(|peer_id| (*peer_id).into()).collect())
			.iter()
			.map(|peer_id| peer_id.into())
			.collect()
	}

	fn add_known_peer(&self, peer_id: sc_network_types::PeerId) {
		self.inner.lock().add_known_peer(peer_id.into());
	}
}

#[derive(Debug, Clone, Copy)]
struct PeerInfo {
	/// Reputation of the peer.
	reputation: i32,

	/// Instant when the peer was last updated.
	last_updated: Instant,

	/// Role of the peer, if known.
	role: Option<ObservedRole>,
}

impl Default for PeerInfo {
	fn default() -> Self {
		Self { reputation: 0, last_updated: Instant::now(), role: None }
	}
}

impl PartialEq for PeerInfo {
	fn eq(&self, other: &Self) -> bool {
		self.reputation == other.reputation
	}
}

impl Eq for PeerInfo {}

impl Ord for PeerInfo {
	// We define reverse order by reputation values.
	fn cmp(&self, other: &Self) -> Ordering {
		self.reputation.cmp(&other.reputation).reverse()
	}
}

impl PartialOrd for PeerInfo {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl PeerInfo {
	fn is_banned(&self) -> bool {
		self.reputation < BANNED_THRESHOLD
	}

	fn add_reputation(&mut self, increment: i32) {
		self.reputation = self.reputation.saturating_add(increment);
		self.bump_last_updated();
	}

	fn decay_reputation(&mut self, seconds_passed: u64) {
		// Note that decaying the reputation value happens "on its own",
		// so we don't do `bump_last_updated()`.
		for _ in 0..seconds_passed {
			let mut diff = self.reputation / INVERSE_DECREMENT;
			if diff == 0 && self.reputation < 0 {
				diff = -1;
			} else if diff == 0 && self.reputation > 0 {
				diff = 1;
			}

			self.reputation = self.reputation.saturating_sub(diff);

			if self.reputation == 0 {
				break
			}
		}
	}

	fn bump_last_updated(&mut self) {
		self.last_updated = Instant::now();
	}
}

#[derive(Debug)]
struct PeerStoreInner {
	peers: HashMap<PeerId, PeerInfo>,
	protocols: Vec<Arc<dyn ProtocolHandle>>,
	metrics: Option<PeerStoreMetrics>,
}

impl PeerStoreInner {
	fn is_banned(&self, peer_id: &PeerId) -> bool {
		self.peers.get(peer_id).map_or(false, |info| info.is_banned())
	}

	fn register_protocol(&mut self, protocol_handle: Arc<dyn ProtocolHandle>) {
		self.protocols.push(protocol_handle);
	}

	fn report_disconnect(&mut self, peer_id: PeerId) {
		let peer_info = self.peers.entry(peer_id).or_default();
		peer_info.add_reputation(DISCONNECT_REPUTATION_CHANGE);

		log::trace!(
			target: LOG_TARGET,
			"Peer {} disconnected, reputation: {:+} to {}",
			peer_id,
			DISCONNECT_REPUTATION_CHANGE,
			peer_info.reputation,
		);
	}

	fn report_peer(&mut self, peer_id: PeerId, change: ReputationChange) {
		let peer_info = self.peers.entry(peer_id).or_default();
		let was_banned = peer_info.is_banned();
		peer_info.add_reputation(change.value);

		log::trace!(
			target: LOG_TARGET,
			"Report {}: {:+} to {}. Reason: {}.",
			peer_id,
			change.value,
			peer_info.reputation,
			change.reason,
		);

		if !peer_info.is_banned() {
			if was_banned {
				log::info!(
					target: LOG_TARGET,
					"Peer {} is now unbanned: {:+} to {}. Reason: {}.",
					peer_id,
					change.value,
					peer_info.reputation,
					change.reason,
				);
			}
			return;
		}

		// Peer is currently banned, disconnect it from all protocols.
		self.protocols.iter().for_each(|handle| handle.disconnect_peer(peer_id.into()));

		// The peer is banned for the first time.
		if !was_banned {
			log::warn!(
				target: LOG_TARGET,
				"Report {}: {:+} to {}. Reason: {}. Banned, disconnecting.",
				peer_id,
				change.value,
				peer_info.reputation,
				change.reason,
			);
			return;
		}

		// The peer was already banned and it got another negative report.
		// This may happen during a batch report.
		if change.value < 0 {
			log::debug!(
				target: LOG_TARGET,
				"Report {}: {:+} to {}. Reason: {}. Misbehaved during the ban threshold.",
				peer_id,
				change.value,
				peer_info.reputation,
				change.reason,
			);
		}
	}

	fn set_peer_role(&mut self, peer_id: &PeerId, role: ObservedRole) {
		log::trace!(target: LOG_TARGET, "Set {peer_id} role to {role:?}");

		match self.peers.entry(*peer_id) {
			Entry::Occupied(mut entry) => {
				entry.get_mut().role = Some(role);
			},
			Entry::Vacant(entry) => {
				entry.insert(PeerInfo { role: Some(role), ..Default::default() });
			},
		}
	}

	fn peer_reputation(&self, peer_id: &PeerId) -> i32 {
		self.peers.get(peer_id).map_or(0, |info| info.reputation)
	}

	fn peer_role(&self, peer_id: &PeerId) -> Option<ObservedRole> {
		self.peers.get(peer_id).map_or(None, |info| info.role)
	}

	fn outgoing_candidates(&self, count: usize, ignored: HashSet<PeerId>) -> Vec<PeerId> {
		let mut candidates = self
			.peers
			.iter()
			.filter_map(|(peer_id, info)| {
				(!info.is_banned() && !ignored.contains(peer_id)).then_some((*peer_id, *info))
			})
			.collect::<Vec<_>>();
		let count = std::cmp::min(count, candidates.len());
		candidates.partial_sort(count, |(_, info1), (_, info2)| info1.cmp(info2));
		candidates.iter().take(count).map(|(peer_id, _)| *peer_id).collect()

		// TODO: keep the peers sorted (in a "bi-multi-map"?) to not repeat sorting every time.
	}

	fn progress_time(&mut self, seconds_passed: u64) {
		if seconds_passed == 0 {
			return
		}

		// Drive reputation values towards 0.
		self.peers
			.iter_mut()
			.for_each(|(_, info)| info.decay_reputation(seconds_passed));

		// Retain only entries with non-zero reputation values or not expired ones.
		let now = Instant::now();
		let mut num_banned_peers: u64 = 0;
		self.peers.retain(|_, info| {
			if info.is_banned() {
				num_banned_peers += 1;
			}

			info.reputation != 0 || info.last_updated + FORGET_AFTER > now
		});

		if let Some(metrics) = &self.metrics {
			metrics.num_discovered.set(self.peers.len() as u64);
			metrics.num_banned_peers.set(num_banned_peers);
		}
	}

	fn add_known_peer(&mut self, peer_id: PeerId) {
		match self.peers.entry(peer_id) {
			Entry::Occupied(mut e) => {
				trace!(
					target: LOG_TARGET,
					"Trying to add an already known peer {peer_id}, bumping `last_updated`.",
				);
				e.get_mut().bump_last_updated();
			},
			Entry::Vacant(e) => {
				trace!(target: LOG_TARGET, "Adding a new known peer {peer_id}.");
				e.insert(PeerInfo::default());
			},
		}
	}
}

/// Worker part of [`PeerStoreHandle`]
#[derive(Debug)]
pub struct PeerStore {
	inner: Arc<Mutex<PeerStoreInner>>,
}

impl PeerStore {
	/// Create a new peer store from the list of bootnodes.
	pub fn new(bootnodes: Vec<PeerId>, metrics_registry: Option<Registry>) -> Self {
		let metrics = if let Some(registry) = &metrics_registry {
			PeerStoreMetrics::register(registry)
				.map_err(|err| {
					log::error!(target: LOG_TARGET, "Failed to register peer set metrics: {}", err);
					err
				})
				.ok()
		} else {
			None
		};

		PeerStore {
			inner: Arc::new(Mutex::new(PeerStoreInner {
				peers: bootnodes
					.into_iter()
					.map(|peer_id| (peer_id, PeerInfo::default()))
					.collect(),
				protocols: Vec::new(),
				metrics,
			})),
		}
	}

	/// Get `PeerStoreHandle`.
	pub fn handle(&self) -> PeerStoreHandle {
		PeerStoreHandle { inner: self.inner.clone() }
	}

	/// Drive the `PeerStore`, decaying reputation values over time and removing expired entries.
	pub async fn run(self) {
		let started = Instant::now();
		let mut latest_time_update = started;

		loop {
			let now = Instant::now();
			// We basically do `(now - self.latest_update).as_secs()`, except that by the way we do
			// it we know that we're not going to miss seconds because of rounding to integers.
			let seconds_passed = {
				let elapsed_latest = latest_time_update - started;
				let elapsed_now = now - started;
				latest_time_update = now;
				elapsed_now.as_secs() - elapsed_latest.as_secs()
			};

			self.inner.lock().progress_time(seconds_passed);
			let _ = Delay::new(Duration::from_secs(1)).await;
		}
	}
}

#[async_trait::async_trait]
impl PeerStoreT for PeerStore {
	fn handle(&self) -> Arc<dyn PeerStoreProvider> {
		Arc::new(self.handle())
	}

	async fn run(self) {
		self.run().await;
	}
}

#[cfg(test)]
mod tests {
	use super::{PeerInfo, PeerStore, PeerStoreProvider};

	#[test]
	fn decaying_zero_reputation_yields_zero() {
		let mut peer_info = PeerInfo::default();
		assert_eq!(peer_info.reputation, 0);

		peer_info.decay_reputation(1);
		assert_eq!(peer_info.reputation, 0);

		peer_info.decay_reputation(100_000);
		assert_eq!(peer_info.reputation, 0);
	}

	#[test]
	fn decaying_positive_reputation_decreases_it() {
		const INITIAL_REPUTATION: i32 = 100;

		let mut peer_info = PeerInfo::default();
		peer_info.reputation = INITIAL_REPUTATION;

		peer_info.decay_reputation(1);
		assert!(peer_info.reputation >= 0);
		assert!(peer_info.reputation < INITIAL_REPUTATION);
	}

	#[test]
	fn decaying_negative_reputation_increases_it() {
		const INITIAL_REPUTATION: i32 = -100;

		let mut peer_info = PeerInfo::default();
		peer_info.reputation = INITIAL_REPUTATION;

		peer_info.decay_reputation(1);
		assert!(peer_info.reputation <= 0);
		assert!(peer_info.reputation > INITIAL_REPUTATION);
	}

	#[test]
	fn decaying_max_reputation_finally_yields_zero() {
		const INITIAL_REPUTATION: i32 = i32::MAX;
		const SECONDS: u64 = 3544;

		let mut peer_info = PeerInfo::default();
		peer_info.reputation = INITIAL_REPUTATION;

		peer_info.decay_reputation(SECONDS / 2);
		assert!(peer_info.reputation > 0);

		peer_info.decay_reputation(SECONDS / 2);
		assert_eq!(peer_info.reputation, 0);
	}

	#[test]
	fn decaying_min_reputation_finally_yields_zero() {
		const INITIAL_REPUTATION: i32 = i32::MIN;
		const SECONDS: u64 = 3544;

		let mut peer_info = PeerInfo::default();
		peer_info.reputation = INITIAL_REPUTATION;

		peer_info.decay_reputation(SECONDS / 2);
		assert!(peer_info.reputation < 0);

		peer_info.decay_reputation(SECONDS / 2);
		assert_eq!(peer_info.reputation, 0);
	}

	#[test]
	fn report_banned_peers() {
		let peer_a = sc_network_types::PeerId::random();
		let peer_b = sc_network_types::PeerId::random();
		let peer_c = sc_network_types::PeerId::random();

		let metrics_registry = prometheus_endpoint::Registry::new();
		let peerstore = PeerStore::new(
			vec![peer_a, peer_b, peer_c].into_iter().map(Into::into).collect(),
			Some(metrics_registry),
		);
		let metrics = peerstore.inner.lock().metrics.as_ref().unwrap().clone();
		let handle = peerstore.handle();

		// Check initial state. Advance time to propagate peers.
		handle.inner.lock().progress_time(1);
		assert_eq!(metrics.num_discovered.get(), 3);
		assert_eq!(metrics.num_banned_peers.get(), 0);

		// Report 2 peers with a negative reputation.
		handle.report_peer(
			peer_a,
			sc_network_common::types::ReputationChange { value: i32::MIN, reason: "test".into() },
		);
		handle.report_peer(
			peer_b,
			sc_network_common::types::ReputationChange { value: i32::MIN, reason: "test".into() },
		);

		// Advance time to propagate banned peers.
		handle.inner.lock().progress_time(1);
		assert_eq!(metrics.num_discovered.get(), 3);
		assert_eq!(metrics.num_banned_peers.get(), 2);
	}
}
