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

//! Substrate block builder
//!
//! This crate provides the [`BlockBuilder`] utility and the corresponding runtime api
//! [`BlockBuilder`](sp_block_builder::BlockBuilder).
//!
//! The block builder utility is used in the node as an abstraction over the runtime api to
//! initialize a block, to push extrinsics and to finalize a block.

#![warn(missing_docs)]

use codec::Encode;

use sp_api::{
	ApiError, ApiExt, ApiRef, CallApiAt, Core, ProofRecorder, ProvideRuntimeApi, StorageChanges,
	TransactionChanges, TransactionOutcome,
};
use sp_blockchain::{ApplyExtrinsicFailed, Error, HeaderBackend};
use sp_core::traits::CallContext;
use sp_externalities::Extensions;
use sp_runtime::{
	legacy,
	traits::{Block as BlockT, Hash, HashingFor, Header as HeaderT, NumberFor, One},
	Digest, ExtrinsicInclusionMode,
};
use std::marker::PhantomData;

pub use sp_block_builder::BlockBuilder as BlockBuilderApi;

/// A builder for creating an instance of [`BlockBuilder`].
pub struct BlockBuilderBuilder<'a, B, C> {
	call_api_at: &'a C,
	_phantom: PhantomData<B>,
}

impl<'a, B, C> BlockBuilderBuilder<'a, B, C>
where
	B: BlockT,
{
	/// Create a new instance of the builder.
	///
	/// `call_api_at`: Something that implements [`CallApiAt`].
	pub fn new(call_api_at: &'a C) -> Self {
		Self { call_api_at, _phantom: PhantomData }
	}

	/// Specify the parent block to build on top of.
	pub fn on_parent_block(self, parent_block: B::Hash) -> BlockBuilderBuilderStage1<'a, B, C> {
		BlockBuilderBuilderStage1 { call_api_at: self.call_api_at, parent_block }
	}
}

/// The second stage of the [`BlockBuilderBuilder`].
///
/// This type can not be instantiated directly. To get an instance of it
/// [`BlockBuilderBuilder::new`] needs to be used.
pub struct BlockBuilderBuilderStage1<'a, B: BlockT, C> {
	call_api_at: &'a C,
	parent_block: B::Hash,
}

impl<'a, B, C> BlockBuilderBuilderStage1<'a, B, C>
where
	B: BlockT,
{
	/// Fetch the parent block number from the given `header_backend`.
	///
	/// The parent block number is used to initialize the block number of the new block.
	///
	/// Returns an error if the parent block specified in
	/// [`on_parent_block`](BlockBuilderBuilder::on_parent_block) does not exist.
	pub fn fetch_parent_block_number<H: HeaderBackend<B>>(
		self,
		header_backend: &H,
	) -> Result<BlockBuilderBuilderStage2<'a, B, C>, Error> {
		let parent_number = header_backend.number(self.parent_block)?.ok_or_else(|| {
			Error::Backend(format!(
				"Could not fetch block number for block: {:?}",
				self.parent_block
			))
		})?;

		Ok(BlockBuilderBuilderStage2 {
			call_api_at: self.call_api_at,
			proof_recorder: None,
			inherent_digests: Default::default(),
			parent_block: self.parent_block,
			parent_number,
			extra_extensions: Extensions::new(),
		})
	}

	/// Provide the block number for the parent block directly.
	///
	/// The parent block is specified in [`on_parent_block`](BlockBuilderBuilder::on_parent_block).
	/// The parent block number is used to initialize the block number of the new block.
	pub fn with_parent_block_number(
		self,
		parent_number: NumberFor<B>,
	) -> BlockBuilderBuilderStage2<'a, B, C> {
		BlockBuilderBuilderStage2 {
			call_api_at: self.call_api_at,
			proof_recorder: None,
			inherent_digests: Default::default(),
			parent_block: self.parent_block,
			parent_number,
			extra_extensions: Extensions::new(),
		}
	}
}

/// The second stage of the [`BlockBuilderBuilder`].
///
/// This type can not be instantiated directly. To get an instance of it
/// [`BlockBuilderBuilder::new`] needs to be used.
pub struct BlockBuilderBuilderStage2<'a, B: BlockT, C> {
	call_api_at: &'a C,
	proof_recorder: Option<ProofRecorder<B>>,
	inherent_digests: Digest,
	parent_block: B::Hash,
	parent_number: NumberFor<B>,
	extra_extensions: Extensions,
}

impl<'a, B: BlockT, C> BlockBuilderBuilderStage2<'a, B, C> {
	/// Enable/disable proof recording for the block builder using the given proof recorder.
	pub fn with_proof_recorder(
		mut self,
		proof_recorder: impl Into<Option<ProofRecorder<B>>>,
	) -> Self {
		self.proof_recorder = proof_recorder.into();
		self
	}

	/// Enable proof recording.
	pub fn enable_proof_recording(mut self) -> Self {
		self.proof_recorder = Some(Default::default());
		self
	}

	/// Build the block with the given inherent digests.
	pub fn with_inherent_digests(mut self, inherent_digests: Digest) -> Self {
		self.inherent_digests = inherent_digests;
		self
	}

	/// Set the extra extensions to be registered with the runtime API during block building.
	pub fn with_extra_extensions(mut self, extra_extensions: impl Into<Extensions>) -> Self {
		self.extra_extensions = extra_extensions.into();
		self
	}

	/// Create the instance of the [`BlockBuilder`].
	pub fn build(self) -> Result<BlockBuilder<'a, B, C>, Error>
	where
		C: CallApiAt<B> + ProvideRuntimeApi<B>,
		C::Api: BlockBuilderApi<B>,
	{
		BlockBuilder::new(
			self.call_api_at,
			self.parent_block,
			self.parent_number,
			self.proof_recorder,
			self.inherent_digests,
			self.extra_extensions,
		)
	}
}

/// A block that was build by [`BlockBuilder`] plus some additional data.
///
/// This additional data includes the `storage_changes`, these changes can be applied to the
/// backend to get the state of the block.
pub struct BuiltBlock<Block: BlockT> {
	/// The actual block that was build.
	pub block: Block,
	/// The changes that need to be applied to the backend to get the state of the build block.
	pub storage_changes: StorageChanges<Block>,
}

impl<Block: BlockT> BuiltBlock<Block> {
	/// Convert into the inner values.
	pub fn into_inner(self) -> (Block, StorageChanges<Block>) {
		(self.block, self.storage_changes)
	}
}

/// A runtime API view whose every call runs in a nested transaction that is rolled back.
///
/// A runtime call made inside an open transaction joins it, so reads made while an extrinsic's
/// transaction is about to be committed would otherwise end up in the block's storage proof.
pub struct Isolated<'a, Block: BlockT, Api> {
	api: &'a Api,
	parent_hash: Block::Hash,
}

impl<'a, Block: BlockT, Api: ApiExt<Block>> Isolated<'a, Block, Api> {
	/// Run `f` in a nested transaction and roll it back afterwards.
	pub fn read<R>(&self, f: impl FnOnce(&Api) -> R) -> R {
		self.api.execute_in_transaction(|api| TransactionOutcome::Rollback(f(api)))
	}

	/// Pass the current value of `key` to `f`, read natively without a runtime call.
	pub fn storage<R>(
		&self,
		key: &[u8],
		f: impl FnOnce(Option<&[u8]>) -> R,
	) -> Result<R, ApiError> {
		self.api.read_storage(self.parent_hash, key, f)
	}

	/// What the innermost open transaction changed in top-level storage; child tries aren't
	/// included. Inside [`BlockBuilder::push_with`]'s `decide` that is exactly the extrinsic being
	/// decided on. Only keys `keep` accepts are resolved.
	pub fn top_changes(
		&self,
		keep: &dyn Fn(&[u8]) -> bool,
	) -> Result<TransactionChanges, ApiError> {
		self.api.transaction_top_changes(self.parent_hash, keep)
	}
}

/// Utility for building new (valid) blocks from a stream of extrinsics.
pub struct BlockBuilder<'a, Block: BlockT, C: ProvideRuntimeApi<Block> + 'a> {
	extrinsics: Vec<Block::Extrinsic>,
	api: ApiRef<'a, C::Api>,
	call_api_at: &'a C,
	/// Version of the [`BlockBuilderApi`] runtime API.
	version: u32,
	parent_hash: Block::Hash,
	/// The estimated size of the block header.
	estimated_header_size: usize,
	extrinsic_inclusion_mode: ExtrinsicInclusionMode,
}

impl<'a, Block, C> BlockBuilder<'a, Block, C>
where
	Block: BlockT,
	C: CallApiAt<Block> + ProvideRuntimeApi<Block> + 'a,
	C::Api: BlockBuilderApi<Block>,
{
	/// Create a new instance of builder based on the given `parent_hash` and `parent_number`.
	///
	/// While proof recording is enabled, all accessed trie nodes are saved.
	/// These recorded trie nodes can be used by a third party to prove the
	/// output of this block builder without having access to the full storage.
	fn new(
		call_api_at: &'a C,
		parent_hash: Block::Hash,
		parent_number: NumberFor<Block>,
		proof_recorder: Option<ProofRecorder<Block>>,
		inherent_digests: Digest,
		extra_extensions: Extensions,
	) -> Result<Self, Error> {
		let header = <<Block as BlockT>::Header as HeaderT>::new(
			parent_number + One::one(),
			Default::default(),
			Default::default(),
			parent_hash,
			inherent_digests,
		);

		let estimated_header_size = header.encoded_size();

		let mut api = call_api_at.runtime_api();

		if let Some(recorder) = proof_recorder {
			api.record_proof_with_recorder(recorder);
		}

		extra_extensions.into_extensions().for_each(|e| {
			api.register_extension(e);
		});

		api.set_call_context(CallContext::Onchain);

		let core_version = api
			.api_version::<dyn Core<Block>>(parent_hash)?
			.ok_or_else(|| Error::VersionInvalid("Core".to_string()))?;

		let extrinsic_inclusion_mode = if core_version >= 5 {
			api.initialize_block(parent_hash, &header)?
		} else {
			#[allow(deprecated)]
			api.initialize_block_before_version_5(parent_hash, &header)?;
			ExtrinsicInclusionMode::AllExtrinsics
		};

		let bb_version = api
			.api_version::<dyn BlockBuilderApi<Block>>(parent_hash)?
			.ok_or_else(|| Error::VersionInvalid("BlockBuilderApi".to_string()))?;

		Ok(Self {
			parent_hash,
			extrinsics: Vec::new(),
			api,
			version: bb_version,
			estimated_header_size,
			call_api_at,
			extrinsic_inclusion_mode,
		})
	}

	/// The extrinsic inclusion mode of the runtime for this block.
	pub fn extrinsic_inclusion_mode(&self) -> ExtrinsicInclusionMode {
		self.extrinsic_inclusion_mode
	}

	/// Push onto the block's list of extrinsics.
	///
	/// This will ensure the extrinsic can be validly executed (by executing it).
	pub fn push(&mut self, xt: <Block as BlockT>::Extrinsic) -> Result<(), Error> {
		self.push_with(xt, |_| true).map(drop)
	}

	/// Push an extrinsic, letting `decide` inspect the post-execution state before it is kept.
	///
	/// `decide` runs only when the extrinsic was applied (`Ok(Ok(_))` or a failed dispatch), inside
	/// the same storage transaction. Returning `true` commits the extrinsic, `false` rolls back its
	/// state changes and its proof recording as if it had never been attempted.
	///
	/// `decide` only gets an [`Isolated`] view, so whatever it reads never reaches the block's
	/// storage proof, even on the commit path.
	///
	/// Returns `Ok(true)` when the extrinsic was included and `Ok(false)` when `decide` rejected
	/// it.
	pub fn push_with<F>(
		&mut self,
		xt: <Block as BlockT>::Extrinsic,
		decide: F,
	) -> Result<bool, Error>
	where
		F: FnOnce(&Isolated<'_, Block, C::Api>) -> bool,
	{
		let parent_hash = self.parent_hash;
		let extrinsics = &mut self.extrinsics;
		let version = self.version;

		self.api.execute_in_transaction(|api| {
			let res = if version < 6 {
				#[allow(deprecated)]
				api.apply_extrinsic_before_version_6(parent_hash, xt.clone())
					.map(legacy::byte_sized_error::convert_to_latest)
			} else {
				api.apply_extrinsic(parent_hash, xt.clone())
			};

			match res {
				Ok(Ok(_)) => {
					let recorded_before = api.proof_recorder().map(|r| r.estimate_encoded_size());
					let keep = decide(&Isolated { api, parent_hash });
					debug_assert_eq!(
						recorded_before,
						api.proof_recorder().map(|r| r.estimate_encoded_size()),
						"`decide` must not add to the storage proof",
					);

					if keep {
						extrinsics.push(xt);
						TransactionOutcome::Commit(Ok(true))
					} else {
						TransactionOutcome::Rollback(Ok(false))
					}
				},
				Ok(Err(tx_validity)) => TransactionOutcome::Rollback(Err(
					ApplyExtrinsicFailed::Validity(tx_validity).into(),
				)),
				Err(e) => TransactionOutcome::Rollback(Err(Error::from(e))),
			}
		})
	}

	/// Run `f` against the current block state in a transaction that is always rolled back.
	///
	/// Neither the state changes nor the storage proof entries made by `f` survive.
	pub fn read_with<R>(&self, f: impl FnOnce(&C::Api) -> R) -> R {
		self.isolated().read(f)
	}

	/// An [`Isolated`] view of the current block state.
	pub fn isolated(&self) -> Isolated<'_, Block, C::Api> {
		Isolated { api: &*self.api, parent_hash: self.parent_hash }
	}

	/// Consume the builder to build a valid `Block` containing all pushed extrinsics.
	///
	/// Returns the build `Block`, the changes to the storage and an optional `StorageProof`
	/// supplied by `self.api`, combined as [`BuiltBlock`].
	/// The storage proof will be `Some(_)` when proof recording was enabled.
	pub fn build(self) -> Result<BuiltBlock<Block>, Error> {
		let header = self.api.finalize_block(self.parent_hash)?;

		debug_assert_eq!(
			header.extrinsics_root().clone(),
			HashingFor::<Block>::ordered_trie_root(
				self.extrinsics.iter().map(Encode::encode).collect(),
				self.api.version(self.parent_hash)?.extrinsics_root_state_version(),
			),
		);

		let state = self.call_api_at.state_at(self.parent_hash)?;

		let storage_changes = self
			.api
			.into_storage_changes(&state, self.parent_hash)
			.map_err(sp_blockchain::Error::StorageChanges)?;

		Ok(BuiltBlock { block: <Block as BlockT>::new(header, self.extrinsics), storage_changes })
	}

	/// Create the inherents for the block.
	///
	/// Returns the inherents created by the runtime or an error if something failed.
	pub fn create_inherents(
		&mut self,
		inherent_data: sp_inherents::InherentData,
	) -> Result<Vec<Block::Extrinsic>, Error> {
		let parent_hash = self.parent_hash;
		self.api
			.execute_in_transaction(move |api| {
				// `create_inherents` should not change any state, to ensure this we always rollback
				// the transaction.
				TransactionOutcome::Rollback(api.inherent_extrinsics(parent_hash, inherent_data))
			})
			.map_err(|e| Error::Application(Box::new(e)))
	}

	/// Estimate the size of the block in the current state.
	///
	/// If `include_proof` is `true`, the estimated size of the storage proof will be added
	/// to the estimation.
	pub fn estimate_block_size(&self) -> usize {
		let size = self.estimated_header_size + self.extrinsics.encoded_size();

		size + self.api.proof_recorder().map_or(0, |pr| pr.estimate_encoded_size())
	}

	/// Returns the [`ProofRecorder`] set for the block building.
	pub fn proof_recorder(&self) -> Option<ProofRecorder<Block>> {
		self.api.proof_recorder()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use sp_blockchain::HeaderBackend;
	use sp_core::Blake2Hasher;
	use sp_state_machine::Backend;
	use substrate_test_runtime_client::{
		runtime::{Block, Block as TestBlock, Extrinsic, ExtrinsicBuilder, TestAPI, Transfer},
		DefaultTestClientBuilderExt, Sr25519Keyring, TestClient, TestClientBuilderExt,
	};

	#[test]
	fn block_building_storage_proof_does_not_include_runtime_by_default() {
		let builder = substrate_test_runtime_client::TestClientBuilder::new();
		let client = builder.build();

		let genesis_hash = client.info().best_hash;

		let storage_proof_recorder = ProofRecorder::<Block>::default();

		BlockBuilderBuilder::new(&client)
			.on_parent_block(genesis_hash)
			.with_parent_block_number(0)
			.with_proof_recorder(storage_proof_recorder.clone())
			.build()
			.unwrap()
			.build()
			.unwrap();

		let proof = storage_proof_recorder.drain_storage_proof();
		let genesis_state_root = client.header(genesis_hash).unwrap().unwrap().state_root;

		let backend =
			sp_state_machine::create_proof_check_backend::<Blake2Hasher>(genesis_state_root, proof)
				.unwrap();

		assert!(backend
			.storage(&sp_core::storage::well_known_keys::CODE)
			.unwrap_err()
			.contains("Database missing expected key"),);
	}

	#[test]
	fn failing_extrinsic_rolls_back_changes_in_storage_proof() {
		let builder = substrate_test_runtime_client::TestClientBuilder::new();
		let client = builder.build();
		let genesis_hash = client.info().best_hash;

		let proof_recorder = ProofRecorder::<Block>::default();

		let mut block_builder = BlockBuilderBuilder::new(&client)
			.on_parent_block(genesis_hash)
			.with_parent_block_number(0)
			.with_proof_recorder(proof_recorder.clone())
			.build()
			.unwrap();

		block_builder.push(ExtrinsicBuilder::new_read_and_panic(8).build()).unwrap_err();

		block_builder.build().unwrap();

		let proof_with_panic = proof_recorder.drain_storage_proof().encoded_size();

		let proof_recorder = ProofRecorder::<Block>::default();

		let mut block_builder = BlockBuilderBuilder::new(&client)
			.on_parent_block(genesis_hash)
			.with_parent_block_number(0)
			.with_proof_recorder(proof_recorder.clone())
			.build()
			.unwrap();

		block_builder.push(ExtrinsicBuilder::new_read(8).build()).unwrap();

		block_builder.build().unwrap();

		let proof_without_panic = proof_recorder.drain_storage_proof().encoded_size();

		let proof_recorder = ProofRecorder::<Block>::default();

		BlockBuilderBuilder::new(&client)
			.on_parent_block(genesis_hash)
			.with_parent_block_number(0)
			.with_proof_recorder(proof_recorder.clone())
			.build()
			.unwrap()
			.build()
			.unwrap();

		let proof_empty_block = proof_recorder.drain_storage_proof().encoded_size();

		// Ensure that we rolled back the changes of the panicked transaction.
		assert!(proof_without_panic > proof_with_panic);
		assert!(proof_without_panic > proof_empty_block);
		assert_eq!(proof_empty_block, proof_with_panic);
	}

	fn new_builder(client: &TestClient) -> BlockBuilder<'_, TestBlock, TestClient> {
		BlockBuilderBuilder::new(client)
			.on_parent_block(client.info().best_hash)
			.with_parent_block_number(0)
			.enable_proof_recording()
			.build()
			.unwrap()
	}

	fn transfer(nonce: u64) -> Extrinsic {
		Transfer {
			from: Sr25519Keyring::Alice.into(),
			to: Sr25519Keyring::Bob.into(),
			amount: 69,
			nonce,
		}
		.into_unchecked_extrinsic()
	}

	fn proof_of(builder: BlockBuilder<'_, TestBlock, TestClient>) -> sp_api::StorageProof {
		let recorder = builder.proof_recorder().expect("Proof recording is enabled; qed");
		builder.build().unwrap();
		recorder.drain_storage_proof()
	}

	fn balance(builder: &BlockBuilder<'_, TestBlock, TestClient>, who: Sr25519Keyring) -> u64 {
		let parent = builder.parent_hash;
		builder.read_with(|api| api.balance_of(parent, who.into()).unwrap())
	}

	fn account_key(who: Sr25519Keyring) -> Vec<u8> {
		let who: sp_core::crypto::AccountId32 = who.into();
		let mut key = sp_core::twox_128(b"System").to_vec();
		key.extend(sp_core::twox_128(b"Account"));
		key.extend(sp_core::blake2_128(who.as_ref()));
		key.extend(who.as_ref() as &[u8]);
		key
	}

	fn change_of(
		changes: &TransactionChanges,
		key: &[u8],
	) -> Option<(Option<Vec<u8>>, Option<Vec<u8>>)> {
		changes
			.changes
			.iter()
			.find(|c| c.key == key)
			.map(|c| (c.pre.clone(), c.post.clone()))
	}

	#[test]
	fn changes_should_list_sender_and_receiver_accounts_when_transfer_applied() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let (alice, bob) = (account_key(Sr25519Keyring::Alice), account_key(Sr25519Keyring::Bob));
		let (alice_pre, bob_pre) =
			(stored(&builder.isolated(), &alice), stored(&builder.isolated(), &bob));

		let mut seen = None;
		builder
			.push_with(transfer(0), |iso| {
				seen = Some((
					iso.top_changes(&|_| true).unwrap(),
					stored(iso, &alice),
					stored(iso, &bob),
				));
				false
			})
			.unwrap();
		let (changes, alice_post, bob_post) = seen.unwrap();

		assert_eq!(change_of(&changes, &alice), Some((alice_pre, alice_post)));
		assert_eq!(change_of(&changes, &bob), Some((bob_pre, bob_post)));
		assert!(changes.changes.iter().all(|c| c.pre != c.post));
	}

	#[test]
	fn changes_should_carry_earlier_extrinsic_value_as_pre_when_key_written_earlier_in_block() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let bob = account_key(Sr25519Keyring::Bob);
		builder.push(transfer(0)).unwrap();
		let after_first = stored(&builder.isolated(), &bob);

		let mut seen = None;
		builder
			.push_with(transfer(1), |iso| {
				seen = Some(iso.top_changes(&|_| true).unwrap());
				true
			})
			.unwrap();

		assert_eq!(
			change_of(&seen.unwrap(), &bob),
			Some((after_first, stored(&builder.isolated(), &bob)))
		);
	}

	#[test]
	fn changes_should_resolve_when_keep_calls_into_the_same_api() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let bob = account_key(Sr25519Keyring::Bob);

		let mut seen = None;
		builder
			.push_with(transfer(0), |iso| {
				seen = Some(iso.top_changes(&|key| iso.read(|_| key == &bob[..])).unwrap());
				false
			})
			.unwrap();

		let changes = seen.unwrap();
		assert_eq!(changes.changes.iter().map(|c| c.key.clone()).collect::<Vec<_>>(), vec![bob]);
	}

	#[test]
	fn changes_should_report_key_as_appended_when_extrinsic_appends_over_earlier_append() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let events = [sp_core::twox_128(b"System"), sp_core::twox_128(b"Events")].concat();
		builder.push(transfer(0)).unwrap();

		let mut seen = None;
		builder
			.push_with(transfer(1), |iso| {
				seen = Some(iso.top_changes(&|key| key == &events[..]).unwrap());
				false
			})
			.unwrap();

		assert_eq!(seen.unwrap(), TransactionChanges { changes: vec![], appended: vec![events] });
	}

	#[test]
	fn changes_should_be_empty_when_no_extrinsic_is_being_decided() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		builder.push(transfer(0)).unwrap();

		assert_eq!(
			builder.isolated().top_changes(&|_| true).unwrap(),
			TransactionChanges::default()
		);
	}

	#[test]
	fn push_with_should_leave_proof_unchanged_when_decide_reads_changes() {
		let client = substrate_test_runtime_client::new();

		let mut read = new_builder(&client);
		read.push_with(transfer(0), |iso| iso.top_changes(&|_| true).is_ok()).unwrap();

		let mut plain = new_builder(&client);
		plain.push(transfer(0)).unwrap();

		assert_eq!(proof_of(read), proof_of(plain));
	}

	#[test]
	fn push_with_should_commit_when_decide_returns_true() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let bob_before = balance(&builder, Sr25519Keyring::Bob);

		let xt = transfer(0);

		assert_eq!(builder.push_with(xt.clone(), |_| true).unwrap(), true);

		assert_eq!(balance(&builder, Sr25519Keyring::Bob), bob_before + 69);
		assert_eq!(builder.build().unwrap().block.extrinsics, vec![xt]);
	}

	#[test]
	fn push_with_should_roll_back_state_when_decide_returns_false() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let bob_before = balance(&builder, Sr25519Keyring::Bob);

		let xt = transfer(0);

		assert_eq!(builder.push_with(xt.clone(), |_| false).unwrap(), false);

		assert_eq!(balance(&builder, Sr25519Keyring::Bob), bob_before);
		builder.push(xt.clone()).unwrap();
		assert_eq!(builder.build().unwrap().block.extrinsics, vec![xt]);
	}

	#[test]
	fn push_with_should_let_decide_see_post_execution_state_when_extrinsic_applied() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let parent = builder.parent_hash;
		let bob_before = balance(&builder, Sr25519Keyring::Bob);

		let mut seen = 0;
		builder
			.push_with(transfer(0), |iso| {
				seen = iso.read(|api| api.balance_of(parent, Sr25519Keyring::Bob.into()).unwrap());
				false
			})
			.unwrap();

		assert_eq!(seen, bob_before + 69);
	}

	#[test]
	fn push_with_should_leave_proof_unchanged_when_decide_returns_false() {
		let client = substrate_test_runtime_client::new();

		let mut rejected = new_builder(&client);
		rejected.push_with(transfer(0), |_| false).unwrap();

		assert_eq!(proof_of(rejected), proof_of(new_builder(&client)));
	}

	#[test]
	fn push_with_should_produce_include_once_proof_when_simulated_then_pushed() {
		let client = substrate_test_runtime_client::new();

		let mut simulated = new_builder(&client);
		simulated.push_with(transfer(0), |_| false).unwrap();
		simulated.push(transfer(0)).unwrap();

		let mut plain = new_builder(&client);
		plain.push(transfer(0)).unwrap();

		assert_eq!(proof_of(simulated), proof_of(plain));
	}

	#[test]
	fn read_with_should_not_add_keys_to_proof_when_closure_reads_storage() {
		let client = substrate_test_runtime_client::new();

		let read = new_builder(&client);
		balance(&read, Sr25519Keyring::Charlie);
		let mut unread = new_builder(&client);
		unread.push(transfer(0)).unwrap();
		let mut read_then_push = read;
		read_then_push.push(transfer(0)).unwrap();

		assert_eq!(proof_of(read_then_push), proof_of(unread));
	}

	#[test]
	fn read_with_should_not_persist_writes_when_closure_writes_storage() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let parent = builder.parent_hash;
		let bob_before = balance(&builder, Sr25519Keyring::Bob);

		let applied = builder.read_with(|api| api.apply_extrinsic(parent, transfer(0)).unwrap());

		assert!(matches!(applied, Ok(Ok(()))));
		assert_eq!(balance(&builder, Sr25519Keyring::Bob), bob_before);
		builder.push(transfer(0)).unwrap();
		assert_eq!(balance(&builder, Sr25519Keyring::Bob), bob_before + 69);
	}

	#[test]
	fn nested_read_inside_committing_push_with_should_not_add_keys_to_proof() {
		let client = substrate_test_runtime_client::new();

		let mut guarded = new_builder(&client);
		let parent = guarded.parent_hash;
		guarded
			.push_with(transfer(0), |iso| {
				iso.read(|api| api.balance_of(parent, Sr25519Keyring::Charlie.into()).unwrap());
				true
			})
			.unwrap();

		let mut plain = new_builder(&client);
		plain.push(transfer(0)).unwrap();

		let mut leaky = new_builder(&client);
		leaky.push(transfer(0)).unwrap();
		leaky.api.balance_of(parent, Sr25519Keyring::Charlie.into()).unwrap();

		let plain = proof_of(plain);
		assert_ne!(proof_of(leaky), plain);
		assert_eq!(proof_of(guarded), plain);
	}

	#[test]
	#[cfg(debug_assertions)]
	#[should_panic(expected = "`decide` must not add to the storage proof")]
	fn push_with_should_panic_in_debug_when_decide_changes_recorder_size() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		let parent = builder.parent_hash;

		let _ = builder.push_with(transfer(0), |iso| {
			iso.api.balance_of(parent, Sr25519Keyring::Charlie.into()).unwrap();
			true
		});
	}

	#[test]
	fn nested_read_should_keep_node_in_proof_when_earlier_committed_tx_recorded_it() {
		let client = substrate_test_runtime_client::new();
		let bob_transfer = |nonce| {
			Transfer {
				from: Sr25519Keyring::Bob.into(),
				to: Sr25519Keyring::Charlie.into(),
				amount: 1,
				nonce,
			}
			.into_unchecked_extrinsic()
		};

		let mut guarded = new_builder(&client);
		let parent = guarded.parent_hash;
		guarded.push(transfer(0)).unwrap();
		guarded
			.push_with(bob_transfer(0), |iso| {
				iso.read(|api| api.balance_of(parent, Sr25519Keyring::Alice.into()).unwrap());
				true
			})
			.unwrap();

		let mut plain = new_builder(&client);
		plain.push(transfer(0)).unwrap();
		plain.push(bob_transfer(0)).unwrap();

		assert_eq!(proof_of(guarded), proof_of(plain));
	}

	#[test]
	fn estimate_block_size_should_be_unchanged_when_guarded_push_rolls_back() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		builder.push(transfer(0)).unwrap();
		let before = builder.estimate_block_size();

		builder.push_with(transfer(1), |_| false).unwrap();

		assert_eq!(builder.estimate_block_size(), before);
	}

	fn set(key: &[u8], value: Option<&[u8]>) -> Extrinsic {
		ExtrinsicBuilder::new_storage_change(key.to_vec(), value.map(|v| v.to_vec())).build()
	}

	fn stored(
		iso: &Isolated<'_, TestBlock, <TestClient as ProvideRuntimeApi<TestBlock>>::Api>,
		key: &[u8],
	) -> Option<Vec<u8>> {
		iso.storage(key, |v| v.map(|v| v.to_vec())).unwrap()
	}

	#[test]
	fn isolated_storage_should_return_post_state_when_called_in_decide() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);

		let mut seen = None;
		builder
			.push_with(set(b"guard", Some(b"post")), |iso| {
				seen = stored(iso, b"guard");
				false
			})
			.unwrap();

		assert_eq!(seen, Some(b"post".to_vec()));
		assert_eq!(stored(&builder.isolated(), b"guard"), None);
	}

	#[test]
	fn isolated_storage_should_return_none_when_key_is_deleted_in_overlay() {
		let client = substrate_test_runtime_client::new();
		let mut builder = new_builder(&client);
		builder.push(set(b"guard", Some(b"pre"))).unwrap();

		let mut seen = Some(Vec::new());
		builder
			.push_with(set(b"guard", None), |iso| {
				seen = stored(iso, b"guard");
				false
			})
			.unwrap();

		assert_eq!(seen, None);
		assert_eq!(stored(&builder.isolated(), b"guard"), Some(b"pre".to_vec()));
	}

	#[test]
	fn isolated_storage_should_read_parent_state_when_key_is_not_in_overlay() {
		let client = substrate_test_runtime_client::new();
		let builder = new_builder(&client);
		let parent = client.state_at(client.info().best_hash).unwrap();

		assert_eq!(
			stored(&builder.isolated(), sp_core::storage::well_known_keys::CODE),
			parent.storage(sp_core::storage::well_known_keys::CODE).unwrap(),
		);
	}

	#[test]
	fn isolated_storage_should_not_add_keys_to_proof_when_reading_parent_state() {
		let client = substrate_test_runtime_client::new();

		let mut read = new_builder(&client);
		read.push_with(transfer(0), |iso| {
			stored(iso, sp_core::storage::well_known_keys::CODE).is_some()
		})
		.unwrap();
		let mut unread = new_builder(&client);
		unread.push(transfer(0)).unwrap();

		assert_eq!(proof_of(read), proof_of(unread));
	}
}
