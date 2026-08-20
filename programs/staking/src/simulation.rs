//! Property tests: run whole pool lifetimes against arbitrary inputs and check
//! the invariants that must hold no matter what anyone does.
//!
//! `math.rs` tests the cases we thought of. This tests the ones we did not.
//! Proptest generates thousands of stake, claim and unstake sequences and, on a
//! failure, shrinks it to the smallest input that still breaks, which is the
//! difference between "something is wrong" and "here is the bug".
//!
//! THE INVARIANT THAT MATTERS, and the reason this file exists:
//!
//!     total ever paid out  <=  rewards_emitted  <=  reward_total
//!
//! The left inequality means the vault can always cover the next claim. The
//! right means the pot is a real ceiling. Together they say the last person to
//! claim gets exactly what they are owed, which is the failure mode this design
//! was chosen to eliminate.

#![cfg(test)]

use proptest::prelude::*;

use crate::constants::{MAX_WEIGHT_CAP, WEIGHT_SCALE};
use crate::math;
use crate::state::{StakeEntry, StakePool};

/// A live position in the simulation. Mirrors the fields of `StakeEntry` that
/// take part in the arithmetic.
#[derive(Clone, Copy, Debug)]
struct SimEntry {
    weight: u128,
    reward_debt: u128,
    amount: u64,
    unlock_ts: i64,
}

fn new_pool(reward_total: u64, lifetime: i64, max_weight: u64) -> StakePool {
    StakePool {
        authority: Default::default(),
        stake_mint: Default::default(),
        reward_mint: Default::default(),
        total_staked: 0,
        total_weighted: 0,
        reward_total,
        rewards_emitted: 0,
        start_ts: 0,
        end_ts: lifetime,
        min_duration: 1,
        max_duration: lifetime,
        max_weight,
        acc_reward_per_weight: 0,
        last_update_ts: 0,
        stake_vault: Default::default(),
        reward_vault: Default::default(),
        rewards_claimed: 0,
        nonce: 0,
        bump: 0,
        stake_vault_bump: 0,
        reward_vault_bump: 0,
        reserved: [0u8; 56],
    }
}

fn entry_with(weight: u128, reward_debt: u128, amount: u64, unlock_ts: i64) -> StakeEntry {
    StakeEntry {
        owner: Default::default(),
        stake_pool: Default::default(),
        amount,
        weight,
        duration: 0,
        start_ts: 0,
        unlock_ts,
        reward_debt,
        rewards_claimed: 0,
        nonce: 0,
        bump: 0,
        reserved: [0u8; 64],
    }
}

/// One step of a pool's life: wait a while, then do something.
#[derive(Clone, Copy, Debug)]
enum Op {
    Stake { amount: u64, duration_frac: u8 },
    Claim { which: usize },
    Unstake { which: usize },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (1u64..1_000_000_000u64, 0u8..=255).prop_map(|(amount, duration_frac)| Op::Stake {
            amount,
            duration_frac
        }),
        (0usize..8).prop_map(|which| Op::Claim { which }),
        (0usize..8).prop_map(|which| Op::Unstake { which }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Whatever anyone does, in whatever order, the pool never promises more
    /// than it holds.
    #[test]
    fn the_pot_always_covers_what_it_owes(
        reward_total in 1u64..1_000_000_000_000_000u64,
        lifetime in 60i64..(3650 * 86_400i64),
        max_weight in WEIGHT_SCALE..=MAX_WEIGHT_CAP,
        steps in prop::collection::vec((1i64..90 * 86_400i64, op_strategy()), 1..40),
    ) {
        let mut pool = new_pool(reward_total, lifetime, max_weight);
        let mut entries: Vec<SimEntry> = Vec::new();
        let mut now = 0i64;
        let mut total_paid: u128 = 0;

        for (wait, op) in steps {
            now = now.saturating_add(wait);
            pool.accrue(now).unwrap();

            match op {
                Op::Stake { amount, duration_frac } => {
                    if now >= pool.end_ts {
                        continue;
                    }
                    // Pick a lock that still fits inside the pool, the way the
                    // real `stake` instruction requires.
                    let remaining = pool.end_ts - now;
                    let duration = 1 + (remaining - 1) * (duration_frac as i64) / 255;
                    if duration < pool.min_duration || duration > pool.max_duration {
                        continue;
                    }

                    let multiplier = math::weight_multiplier(
                        duration, pool.min_duration, pool.max_duration, pool.max_weight,
                    ).unwrap();
                    let weight = math::stake_weight(amount, multiplier).unwrap();
                    if weight == 0 {
                        continue;
                    }

                    let reward_debt = math::gross_scaled(weight, pool.acc_reward_per_weight).unwrap();
                    pool.total_staked = pool.total_staked.checked_add(amount).unwrap();
                    pool.total_weighted = pool.total_weighted.checked_add(weight).unwrap();
                    entries.push(SimEntry { weight, reward_debt, amount, unlock_ts: now + duration });
                }

                Op::Claim { which } => {
                    if entries.is_empty() { continue; }
                    let i = which % entries.len();
                    let e = entry_with(entries[i].weight, entries[i].reward_debt, entries[i].amount, entries[i].unlock_ts);
                    let pending = e.pending(pool.acc_reward_per_weight).unwrap();
                    entries[i].reward_debt = e.gross_scaled(pool.acc_reward_per_weight).unwrap();
                    pool.rewards_claimed = pool.rewards_claimed.checked_add(pending).unwrap();
                    total_paid += pending as u128;
                }

                Op::Unstake { which } => {
                    if entries.is_empty() { continue; }
                    let i = which % entries.len();
                    if now < entries[i].unlock_ts { continue; }

                    let e = entry_with(entries[i].weight, entries[i].reward_debt, entries[i].amount, entries[i].unlock_ts);
                    let pending = e.pending(pool.acc_reward_per_weight).unwrap();

                    pool.total_staked = pool.total_staked.checked_sub(entries[i].amount).unwrap();
                    pool.total_weighted = pool.total_weighted.checked_sub(entries[i].weight).unwrap();
                    pool.rewards_claimed = pool.rewards_claimed.checked_add(pending).unwrap();
                    total_paid += pending as u128;
                    entries.remove(i);
                }
            }

            // ---- the invariants, checked after every single step ----

            prop_assert!(
                pool.rewards_emitted <= pool.reward_total,
                "emitted {} exceeds the pot {}", pool.rewards_emitted, pool.reward_total
            );

            prop_assert!(
                total_paid <= pool.rewards_emitted as u128,
                "paid out {} but only {} has been emitted", total_paid, pool.rewards_emitted
            );

            let live: u128 = entries.iter().map(|e| e.weight).sum();
            prop_assert_eq!(
                pool.total_weighted, live,
                "the pool's weight has drifted from the sum of its entries"
            );

            let principal: u64 = entries.iter().map(|e| e.amount).sum();
            prop_assert_eq!(pool.total_staked, principal);
        }

        // Everyone left standing settles up at the end. Even then, the pot holds.
        pool.accrue(pool.end_ts).unwrap();
        let mut final_paid = total_paid;
        for e in &entries {
            let entry = entry_with(e.weight, e.reward_debt, e.amount, e.unlock_ts);
            final_paid += entry.pending(pool.acc_reward_per_weight).unwrap() as u128;
        }
        prop_assert!(
            final_paid <= pool.reward_total as u128,
            "once everyone claims, {} would be paid from a pot of {}",
            final_paid, pool.reward_total
        );
    }

    /// A pool nobody stakes in must not burn its pot on empty air.
    ///
    /// This is the rule that keeps rewards for whoever arrives late, and the one
    /// somebody will eventually try to "simplify" out of `accrue`.
    #[test]
    fn an_empty_pool_emits_nothing_however_long_it_waits(
        reward_total in 1u64..1_000_000_000_000u64,
        lifetime in 60i64..(3650 * 86_400i64),
        waits in prop::collection::vec(1i64..(400 * 86_400i64), 1..20),
    ) {
        let mut pool = new_pool(reward_total, lifetime, WEIGHT_SCALE);
        let mut now = 0i64;
        for w in waits {
            now = now.saturating_add(w);
            pool.accrue(now).unwrap();
            prop_assert_eq!(pool.rewards_emitted, 0);
            prop_assert_eq!(pool.acc_reward_per_weight, 0);
        }
    }

    /// Emission stops dead at `end_ts`, however long the pool is left running.
    #[test]
    fn a_pool_left_running_past_its_end_stops_emitting(
        reward_total in 1_000u64..1_000_000_000_000u64,
        lifetime in 60i64..(365 * 86_400i64),
        overrun in 1i64..(3650 * 86_400i64),
    ) {
        let mut pool = new_pool(reward_total, lifetime, WEIGHT_SCALE);
        pool.total_weighted = 1_000_000;
        pool.total_staked = 1_000_000;

        pool.accrue(lifetime).unwrap();
        let at_end = pool.rewards_emitted;

        pool.accrue(lifetime + overrun).unwrap();
        prop_assert_eq!(
            pool.rewards_emitted, at_end,
            "the pool kept emitting after it ended"
        );
    }
}
