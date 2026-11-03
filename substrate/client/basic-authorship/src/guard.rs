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

//! Node-side hook deciding, per pool transaction, whether it goes into a block being authored.

use sc_block_builder::BlockBuilder;
use sp_api::ProvideRuntimeApi;
use sp_runtime::traits::{Block as BlockT, NumberFor};
use std::time::Instant;

/// What one proposal tells its guard session up front.
#[derive(Clone, Debug)]
pub struct GuardContext<Block: BlockT> {
	/// The block the proposal builds on.
	pub parent_hash: Block::Hash,
	/// Number of `parent_hash`.
	pub parent_number: NumberFor<Block>,
	/// The proposer's hard deadline for pushing transactions.
	pub deadline: Instant,
}

/// Result of handing one pool transaction to a [`GuardSession`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardOutcome {
	/// The transaction is in the block.
	Pushed,
	/// The transaction is not in the block and stays in the pool; its dependents are skipped for
	/// this block.
	Skipped,
	/// Stop adding transactions and propose what has been built so far.
	Stop,
}

/// A guard installed into the [`ProposerFactory`](crate::ProposerFactory).
///
/// `H` is the transaction pool's hash type.
pub trait ExtrinsicGuard<Block: BlockT, C, H>: Send + Sync {
	/// Start a session for one proposal. The session is dropped when the proposal stops pushing
	/// transactions, so nothing it holds can leak into another proposal.
	fn start(&self, ctx: GuardContext<Block>) -> Box<dyn GuardSession<Block, C, H> + Send + '_>;
}

/// The per-proposal half of an [`ExtrinsicGuard`]. It replaces `BlockBuilder::push`.
pub trait GuardSession<Block: BlockT, C, H> {
	/// Push `xt` (or not) onto `bb`.
	///
	/// An `Err` is handled exactly like an error returned by `BlockBuilder::push`.
	fn apply(
		&mut self,
		bb: &mut BlockBuilder<'_, Block, C>,
		xt: Block::Extrinsic,
		hash: &H,
	) -> Result<GuardOutcome, sp_blockchain::Error>
	where
		C: ProvideRuntimeApi<Block>;
}
