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
	Balances, ExtBuilder, OnBurnCalls, OnDustLostCalls, OnMintCalls, OnRepatriateCalls,
	OnReserveCalls, OnSlashReservedCalls, OnTransferCalls, OnUnreserveCalls, RawOrigin,
};
use frame_support::{
	assert_ok,
	traits::{
		fungible::Mutate,
		tokens::{BalanceStatus, Fortitude, Precision, Preservation},
		Currency, Imbalance, ReservableCurrency, WithdrawReasons,
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

#[test]
fn on_reserve_hook_fires_on_successful_reserve() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		OnReserveCalls::set(0);
		assert_ok!(<Balances as ReservableCurrency<_>>::reserve(&1, 5));
		assert_eq!(OnReserveCalls::get(), 1);

		// reserve(0) is a no-op and must NOT fire
		assert_ok!(<Balances as ReservableCurrency<_>>::reserve(&1, 0));
		assert_eq!(OnReserveCalls::get(), 1);
	});
}

#[test]
fn on_reserve_hook_does_not_fire_when_reserve_fails() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		OnReserveCalls::set(0);
		// account 1 has 10; asking 1_000_000 must fail with InsufficientBalance
		// and the hook must NOT fire
		assert!(<Balances as ReservableCurrency<_>>::reserve(&1, 1_000_000).is_err());
		assert_eq!(OnReserveCalls::get(), 0);
	});
}

#[test]
fn on_unreserve_hook_fires_with_actual_unreserved_amount() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		assert_ok!(<Balances as ReservableCurrency<_>>::reserve(&1, 5));
		OnUnreserveCalls::set(0);

		// asking to unreserve more than reserved — only 5 actually unreserved.
		let remaining = <Balances as ReservableCurrency<_>>::unreserve(&1, 1_000);
		assert_eq!(remaining, 995, "remaining = requested - actual_unreserved");
		assert_eq!(OnUnreserveCalls::get(), 1);

		// unreserve(0) is a no-op and must NOT fire
		let _ = <Balances as ReservableCurrency<_>>::unreserve(&1, 0);
		assert_eq!(OnUnreserveCalls::get(), 1);
	});
}

#[test]
fn slash_reserved_fires_on_slash_reserved_hook() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		assert_ok!(<Balances as ReservableCurrency<_>>::reserve(&1, 5));
		OnSlashReservedCalls::set(0);

		let (imbalance, remaining) = <Balances as ReservableCurrency<_>>::slash_reserved(&1, 100);
		// account had 5 reserved, asked to slash 100 — best-effort slashes 5
		assert_eq!(imbalance.peek(), 5);
		assert_eq!(remaining, 95);
		assert_eq!(OnSlashReservedCalls::get(), 1);

		// slash_reserved(0) is a no-op
		let (imbalance, remaining) = <Balances as ReservableCurrency<_>>::slash_reserved(&1, 0);
		assert_eq!(imbalance.peek(), 0);
		assert_eq!(remaining, 0);
		assert_eq!(OnSlashReservedCalls::get(), 1);
	});
}

#[test]
fn on_repatriate_hook_fires_when_slashed_differs_from_beneficiary() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		assert_ok!(<Balances as ReservableCurrency<_>>::reserve(&1, 5));
		OnRepatriateCalls::set(0);

		assert_ok!(<Balances as ReservableCurrency<_>>::repatriate_reserved(
			&1,
			&2,
			3,
			BalanceStatus::Free,
		));
		assert_eq!(OnRepatriateCalls::get(), 1);

		// repatriate(0) is a no-op and must NOT fire
		assert_ok!(<Balances as ReservableCurrency<_>>::repatriate_reserved(
			&1,
			&2,
			0,
			BalanceStatus::Free,
		));
		assert_eq!(OnRepatriateCalls::get(), 1);
	});
}

// ----------------------------------------------------------------------
// Legacy Currency-trait hook coverage. Many runtime paths still go through
// the `Currency` interface (notably the native fee path:
// pallet-transaction-payment → BasicCurrencyAdapter → Currency::withdraw),
// so without hook calls in those impls the on_burn / on_mint counters never
// move and erc20-style indexers see zero Transfer logs for fees.
// ----------------------------------------------------------------------

#[test]
fn currency_withdraw_fires_on_burn_hook() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		OnBurnCalls::set(0);
		// Account 1 starts with 10 (from `monied(true)`).
		let imbalance = <Balances as Currency<_>>::withdraw(
			&1,
			3,
			WithdrawReasons::TRANSFER,
			frame_support::traits::ExistenceRequirement::AllowDeath,
		)
		.expect("withdraw must succeed");
		assert_eq!(imbalance.peek(), 3);
		assert_eq!(OnBurnCalls::get(), 1);

		// withdraw(0) is a no-op and must NOT fire
		let imbalance = <Balances as Currency<_>>::withdraw(
			&1,
			0,
			WithdrawReasons::TRANSFER,
			frame_support::traits::ExistenceRequirement::AllowDeath,
		)
		.expect("zero withdraw must succeed");
		assert_eq!(imbalance.peek(), 0);
		assert_eq!(OnBurnCalls::get(), 1);
	});
}

#[test]
fn currency_deposit_creating_fires_on_mint_hook() {
	ExtBuilder::default().build_and_execute_with(|| {
		OnMintCalls::set(0);
		let pos = <Balances as Currency<_>>::deposit_creating(&42, 1_000);
		assert_eq!(pos.peek(), 1_000);
		assert_eq!(OnMintCalls::get(), 1);

		// deposit_creating(0) is a no-op and must NOT fire
		let pos = <Balances as Currency<_>>::deposit_creating(&42, 0);
		assert_eq!(pos.peek(), 0);
		assert_eq!(OnMintCalls::get(), 1);
	});
}

#[test]
fn currency_deposit_into_existing_fires_on_mint_hook() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		OnMintCalls::set(0);
		let pos = <Balances as Currency<_>>::deposit_into_existing(&1, 7).expect("must succeed");
		assert_eq!(pos.peek(), 7);
		assert_eq!(OnMintCalls::get(), 1);

		// zero is a no-op and must NOT fire
		let pos = <Balances as Currency<_>>::deposit_into_existing(&1, 0).expect("must succeed");
		assert_eq!(pos.peek(), 0);
		assert_eq!(OnMintCalls::get(), 1);
	});
}

#[test]
fn currency_slash_fires_on_burn_hook_with_actual_slashed_amount() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		OnBurnCalls::set(0);
		// account 1 has 10
		let (imbalance, remaining) = <Balances as Currency<_>>::slash(&1, 1_000);
		// best-effort slashing: 10 actually slashed, 990 remaining
		assert_eq!(imbalance.peek(), 10);
		assert_eq!(remaining, 990);
		assert_eq!(OnBurnCalls::get(), 1);

		// slash(0) is a no-op and must NOT fire
		let (imbalance, remaining) = <Balances as Currency<_>>::slash(&1, 0);
		assert_eq!(imbalance.peek(), 0);
		assert_eq!(remaining, 0);
		assert_eq!(OnBurnCalls::get(), 1);
	});
}

#[test]
fn on_repatriate_hook_does_not_fire_when_slashed_equals_beneficiary() {
	ExtBuilder::default().monied(true).build_and_execute_with(|| {
		assert_ok!(<Balances as ReservableCurrency<_>>::reserve(&1, 5));
		OnRepatriateCalls::set(0);
		OnUnreserveCalls::set(0);

		// slashed == beneficiary, status=Free → delegates to unreserve internally
		assert_ok!(<Balances as ReservableCurrency<_>>::repatriate_reserved(
			&1,
			&1,
			3,
			BalanceStatus::Free,
		));
		assert_eq!(OnRepatriateCalls::get(), 0);
		assert_eq!(OnUnreserveCalls::get(), 1);

		// slashed == beneficiary, status=Reserved → no-op
		assert_ok!(<Balances as ReservableCurrency<_>>::repatriate_reserved(
			&1,
			&1,
			1,
			BalanceStatus::Reserved,
		));
		assert_eq!(OnRepatriateCalls::get(), 0);
	});
}
