// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use frame_support::{construct_runtime, derive_impl};
use sp_core::sr25519;
use sp_runtime::{generic, traits::BlakeTwo256};

#[frame_support::pallet]
mod pallet {
	#[pallet::config]
	pub trait Config: frame_system::Config {}

	#[pallet::pallet]
	pub struct Pallet<T>(core::marker::PhantomData<T>);

	#[pallet::error]
	pub enum Error<T> {
		MyError(crate::Nested1),
	}
}

#[derive(
	scale_info::TypeInfo,
	frame_support::PalletError,
	codec::Encode,
	codec::Decode,
	codec::DecodeWithMemTracking,
)]
pub enum Nested1 {
	Nested2(Nested2),
}

#[derive(
	scale_info::TypeInfo,
	frame_support::PalletError,
	codec::Encode,
	codec::Decode,
	codec::DecodeWithMemTracking,
)]
pub enum Nested2 {
	Nested3(Nested3),
}

#[derive(
	scale_info::TypeInfo,
	frame_support::PalletError,
	codec::Encode,
	codec::Decode,
	codec::DecodeWithMemTracking,
)]
pub enum Nested3 {
	Nested4(Nested4),
}

#[derive(
	scale_info::TypeInfo,
	frame_support::PalletError,
	codec::Encode,
	codec::Decode,
	codec::DecodeWithMemTracking,
)]
pub enum Nested4 {
	Num(u8),
}

pub type Signature = sr25519::Signature;
pub type BlockNumber = u32;
pub type Header = generic::Header<BlockNumber, BlakeTwo256>;
pub type Block = generic::Block<Header, UncheckedExtrinsic>;
pub type UncheckedExtrinsic = generic::UncheckedExtrinsic<u32, RuntimeCall, Signature, ()>;

impl pallet::Config for Runtime {}

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Runtime {
	type BaseCallFilter = frame_support::traits::Everything;
	type RuntimeOrigin = RuntimeOrigin;
	type Nonce = u64;
	type RuntimeCall = RuntimeCall;
	type Hash = sp_runtime::testing::H256;
	type Hashing = sp_runtime::traits::BlakeTwo256;
	type AccountId = u64;
	type Lookup = sp_runtime::traits::IdentityLookup<Self::AccountId>;
	type Block = Block;
	type RuntimeEvent = RuntimeEvent;
	type BlockHashCount = frame_support::traits::ConstU32<250>;
	type BlockWeights = ();
	type BlockLength = ();
	type DbWeight = ();
	type Version = ();
	type PalletInfo = PalletInfo;
	type AccountData = ();
	type OnNewAccount = ();
	type OnKilledAccount = ();
	type SystemWeightInfo = ();
	type SS58Prefix = ();
	type OnSetCode = ();
	type MaxConsumers = frame_support::traits::ConstU32<16>;
}

construct_runtime! {
	pub struct Runtime
	{
		System: frame_system::{Pallet, Call, Storage, Config<T>, Event<T>},
		Pallet: pallet::{Pallet},
	}
}

fn main() {}
