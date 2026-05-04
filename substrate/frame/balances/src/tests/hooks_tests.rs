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

#![cfg(test)]

use crate::tests::{
	Balances, ExtBuilder, OnBurnCalls, OnDustLostCalls, OnMintCalls, OnTransferCalls, RawOrigin,
};
use frame_support::{
	assert_ok,
	traits::{
		fungible::Mutate,
		tokens::{Fortitude, Precision, Preservation},
		Currency,
	},
};

#[test]
fn on_transfer_hook_fires_on_transfer() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		OnTransferCalls::set(0);
		assert_ok!(Balances::transfer_keep_alive(RawOrigin::Signed(1).into(), 2, 2));
		assert_eq!(OnTransferCalls::get(), 1);

		assert_ok!(Balances::transfer_allow_death(RawOrigin::Signed(1).into(), 2, 2));
		assert_eq!(OnTransferCalls::get(), 2);

		assert_ok!(Balances::force_transfer(RawOrigin::Root.into(), 1, 2, 2));
		assert_eq!(OnTransferCalls::get(), 3);
	});
}

#[test]
fn on_mint_hook_fires_on_mint_into() {
	ExtBuilder::default().build_and_execute_with(|| {
		OnMintCalls::set(0);
		assert_ok!(<Balances as Mutate<_>>::mint_into(&42, 1_000));
		assert_eq!(OnMintCalls::get(), 1);
	});
}

#[test]
fn on_burn_hook_fires_on_burn_from() {
	ExtBuilder::default().build_and_execute_with(|| {
		assert_ok!(<Balances as Mutate<_>>::mint_into(&42, 1_000));
		OnBurnCalls::set(0);
		assert_ok!(<Balances as Mutate<_>>::burn_from(
			&42,
			500,
			Preservation::Expendable,
			Precision::Exact,
			Fortitude::Polite,
		));
		assert_eq!(OnBurnCalls::get(), 1);
	});
}

#[test]
fn on_dust_lost_hook_fires_when_account_dusts() {
	ExtBuilder::default().existential_deposit(100).build_and_execute_with(|| {
		assert_ok!(Balances::force_set_balance(RawOrigin::Root.into(), 1, 100));
		OnDustLostCalls::set(0);
		// Slash 1 of 100 leaves 99 which is below ED (100); the residue is dust.
		Balances::slash(&1, 1);
		assert_eq!(OnDustLostCalls::get(), 1);
	});
}
