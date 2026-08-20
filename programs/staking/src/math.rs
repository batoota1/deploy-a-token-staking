//! Every calculation that decides how many tokens somebody gets.
//!
//! Kept as pure functions with no accounts and no context, for two reasons: it
//! is the part an auditor should be able to read without holding the rest of the
//! program in their head, and it is the part that can be exhaustively tested
//! without a validator.
//!
//! THE RULE THAT GOVERNS ALL OF IT: every division floors, and every floor is in
//! the pool's favour. Payouts under-run rather than over-run, so the vault can
//! always cover the final claim. The dust that leaves behind is not lost: it
//! returns to the creator when the pool closes.
//!
//! The opposite choice is the one this codebase has already written a comment
//! about. From `lib/presale.ts` in the deploy-a-token repo: "a rounding that
//! went up would promise a token more than the sale can deliver, and the last
//! claimer would be the one to find out."

use anchor_lang::prelude::*;

use crate::constants::{ACC_SCALE, WEIGHT_SCALE};
use crate::error::StakingError;

/// The multiplier a lock of `duration` earns, scaled by `WEIGHT_SCALE`.
///
/// Linear from 1.0x at `min_duration` to `max_weight` at `max_duration`. A pool
/// where the two durations are equal offers no bonus and every stake weighs the
/// same, which is a legitimate configuration and not an error.
///
/// Multiply before divide, so a short lock in a long range does not floor its
/// bonus to nothing.
pub fn weight_multiplier(
    duration: i64,
    min_duration: i64,
    max_duration: i64,
    max_weight: u64,
) -> Result<u64> {
    require!(
        duration >= min_duration && duration <= max_duration,
        StakingError::DurationOutOfRange
    );

    if max_duration == min_duration {
        return Ok(WEIGHT_SCALE);
    }

    let span = (max_duration - min_duration) as u128;
    let over = (duration - min_duration) as u128;
    let extra = max_weight
        .checked_sub(WEIGHT_SCALE)
        .ok_or(StakingError::MathOverflow)? as u128;

    let bonus = over
        .checked_mul(extra)
        .ok_or(StakingError::MathOverflow)?
        .checked_div(span)
        .ok_or(StakingError::MathOverflow)?;

    let multiplier = (WEIGHT_SCALE as u128)
        .checked_add(bonus)
        .ok_or(StakingError::MathOverflow)?;

    u64::try_from(multiplier).map_err(|_| StakingError::MathOverflow.into())
}

/// What a deposit of `amount` counts as, once its lock bonus is applied.
///
/// This, not `amount`, is what earns. It is summed into the pool's
/// `total_weighted`, which is the denominator every share is measured against.
pub fn stake_weight(amount: u64, multiplier: u64) -> Result<u128> {
    (amount as u128)
        .checked_mul(multiplier as u128)
        .ok_or(StakingError::MathOverflow)?
        .checked_div(WEIGHT_SCALE as u128)
        .ok_or(StakingError::MathOverflow.into())
}

/// How much of the pot `elapsed` seconds releases, out of a pool that runs for
/// `lifetime` seconds in total.
///
/// The caller clamps the result against what is left in the pot. That clamp, not
/// this function, is what guarantees emission never exceeds `reward_total`.
pub fn emitted_over(elapsed: i64, reward_total: u64, lifetime: i64) -> Result<u64> {
    if elapsed <= 0 || lifetime <= 0 {
        return Ok(0);
    }

    let emitted = (elapsed as u128)
        .checked_mul(reward_total as u128)
        .ok_or(StakingError::MathOverflow)?
        .checked_div(lifetime as u128)
        .ok_or(StakingError::MathOverflow)?;

    u64::try_from(emitted).map_err(|_| StakingError::MathOverflow.into())
}

/// How far `acc_reward_per_weight` moves when `emitted` tokens are released
/// across `total_weighted` units of weight.
///
/// Returns zero rather than dividing by zero when nothing is staked. The caller
/// never reaches this in that case, because `StakePool::accrue` returns early,
/// but a division that can panic has no business in a program.
pub fn acc_increment(emitted: u64, total_weighted: u128) -> Result<u128> {
    if total_weighted == 0 {
        return Ok(0);
    }

    (emitted as u128)
        .checked_mul(ACC_SCALE)
        .ok_or(StakingError::MathOverflow)?
        .checked_div(total_weighted)
        .ok_or(StakingError::MathOverflow.into())
}

/// The running total an entry has ever been entitled to, LEFT SCALED.
///
/// NOT DIVIDED BY `ACC_SCALE`, AND THAT IS THE WHOLE POINT. A property test
/// caught the alternative: storing `reward_debt` already floored means a claim
/// computes `floor(a) - floor(b)`, and that can exceed `floor(a - b)` by one
/// base unit. One unit per claim looks like nothing until you notice it is paid
/// out of a pot that is a hard ceiling, so enough claims and the pool owes more
/// than it holds. The shrunk counterexample was two claims in a row.
///
/// Keeping the debt scaled means the division happens exactly ONCE, in
/// `pending`, on the difference. Nobody can then collect a unit that was never
/// emitted.
///
/// WHY THIS CANNOT OVERFLOW, worth stating because `weight * acc` looks
/// alarming: an entry's weight is part of `total_weighted` for as long as the
/// entry is live, and `acc` grows by `emitted * ACC_SCALE / total_weighted`. So
/// `weight * acc` is bounded above by `total_weighted * acc`, which telescopes
/// to at most `rewards_emitted * ACC_SCALE`, at most `u64::MAX * 1e18`, well
/// inside a u128. The `checked_mul` stays anyway: an argument is not a
/// guarantee.
pub fn gross_scaled(weight: u128, acc_reward_per_weight: u128) -> Result<u128> {
    weight
        .checked_mul(acc_reward_per_weight)
        .ok_or(StakingError::MathOverflow.into())
}

/// What an entry can claim right now: everything it has earned, less what it has
/// already taken.
///
/// The single floor in the whole reward path. Anything the division discards
/// stays in the pot rather than being paid to somebody it was not emitted for.
///
/// Saturating rather than checked on the subtraction, and clamped at zero. `acc`
/// only ever grows, so the gross should never fall below `reward_debt`. If it
/// ever did, the honest answer is "nothing to claim" rather than a panic or a
/// wrapped number the size of the universe. `claimableAt` in the vesting module
/// clamps at zero for the same reason.
pub fn pending(weight: u128, acc_reward_per_weight: u128, reward_debt: u128) -> Result<u64> {
    let gross = gross_scaled(weight, acc_reward_per_weight)?;
    let owed = gross.saturating_sub(reward_debt) / ACC_SCALE;
    u64::try_from(owed).map_err(|_| StakingError::MathOverflow.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MAX_WEIGHT_CAP;

    const DAY: i64 = 86_400;

    #[test]
    fn multiplier_is_one_at_the_shortest_lock() {
        let m = weight_multiplier(DAY, DAY, 30 * DAY, 3 * WEIGHT_SCALE).unwrap();
        assert_eq!(m, WEIGHT_SCALE);
    }

    #[test]
    fn multiplier_is_the_maximum_at_the_longest_lock() {
        let m = weight_multiplier(30 * DAY, DAY, 30 * DAY, 3 * WEIGHT_SCALE).unwrap();
        assert_eq!(m, 3 * WEIGHT_SCALE);
    }

    #[test]
    fn multiplier_is_linear_in_between() {
        // Halfway along the range earns halfway to the bonus: 1x + (3x-1x)/2 = 2x.
        let min = 0;
        let max = 100;
        let m = weight_multiplier(50, min, max, 3 * WEIGHT_SCALE).unwrap();
        assert_eq!(m, 2 * WEIGHT_SCALE);
    }

    #[test]
    fn a_pool_with_one_lock_length_gives_everyone_the_same_weight() {
        // min == max is a legitimate configuration, not a division by zero.
        let m = weight_multiplier(7 * DAY, 7 * DAY, 7 * DAY, 5 * WEIGHT_SCALE).unwrap();
        assert_eq!(m, WEIGHT_SCALE);
    }

    #[test]
    fn a_lock_outside_the_range_is_refused() {
        assert!(weight_multiplier(DAY - 1, DAY, 30 * DAY, 2 * WEIGHT_SCALE).is_err());
        assert!(weight_multiplier(30 * DAY + 1, DAY, 30 * DAY, 2 * WEIGHT_SCALE).is_err());
    }

    #[test]
    fn a_short_lock_in_a_long_range_still_earns_its_bonus() {
        // The reason multiply comes before divide. One day into a ten year
        // range, with a 10x bonus available, is a small number but not zero.
        let ten_years = 3650 * DAY;
        let m = weight_multiplier(DAY, 0, ten_years, MAX_WEIGHT_CAP).unwrap();
        assert!(m > WEIGHT_SCALE, "a divide-first implementation floors this to 1.0x");
        assert_eq!(m, WEIGHT_SCALE + (MAX_WEIGHT_CAP - WEIGHT_SCALE) / 3650);
    }

    #[test]
    fn weight_tracks_the_multiplier() {
        assert_eq!(stake_weight(1_000, WEIGHT_SCALE).unwrap(), 1_000);
        assert_eq!(stake_weight(1_000, 2 * WEIGHT_SCALE).unwrap(), 2_000);
        assert_eq!(stake_weight(1_000, MAX_WEIGHT_CAP).unwrap(), 10_000);
    }

    #[test]
    fn weight_survives_the_largest_stake_at_the_largest_multiplier() {
        // u64::MAX tokens at 10x is 1.8e20, far inside a u128, but this is the
        // combination an overflow would appear at if the intermediate were u64.
        let w = stake_weight(u64::MAX, MAX_WEIGHT_CAP).unwrap();
        assert_eq!(w, (u64::MAX as u128) * 10);
    }

    #[test]
    fn emission_is_proportional_to_elapsed_time() {
        let pot = 1_000_000u64;
        let lifetime = 100i64;
        assert_eq!(emitted_over(0, pot, lifetime).unwrap(), 0);
        assert_eq!(emitted_over(50, pot, lifetime).unwrap(), 500_000);
        assert_eq!(emitted_over(100, pot, lifetime).unwrap(), 1_000_000);
    }

    #[test]
    fn emission_floors_and_never_rounds_up() {
        // 1 second of a 3-token pot over 2 seconds is 1.5, which must not become 2.
        assert_eq!(emitted_over(1, 3, 2).unwrap(), 1);
    }

    #[test]
    fn emission_of_a_degenerate_window_is_zero_not_a_panic() {
        assert_eq!(emitted_over(10, 1_000, 0).unwrap(), 0);
        assert_eq!(emitted_over(-10, 1_000, 100).unwrap(), 0);
    }

    #[test]
    fn an_empty_pool_moves_the_accumulator_nowhere() {
        assert_eq!(acc_increment(1_000, 0).unwrap(), 0);
    }

    #[test]
    fn the_accumulator_keeps_precision_on_a_tiny_share() {
        // One token released across a very large stake. Without the 1e18 scale
        // this floors to zero and that reward is never payable.
        let inc = acc_increment(1, 1_000_000_000_000).unwrap();
        assert!(inc > 0, "1e18 scaling exists precisely to stop this being zero");
        assert_eq!(inc, ACC_SCALE / 1_000_000_000_000);
    }

    #[test]
    fn a_fresh_entry_is_owed_nothing() {
        let weight = 1_000u128;
        let acc = 5 * ACC_SCALE;
        let debt = gross_scaled(weight, acc).unwrap();
        assert_eq!(pending(weight, acc, debt).unwrap(), 0);
    }

    #[test]
    fn pending_never_goes_negative_on_a_stale_read() {
        // reward_debt higher than gross should read as "nothing to claim", not
        // wrap around into an enormous payout.
        assert_eq!(pending(1_000, ACC_SCALE, u128::MAX).unwrap(), 0);
    }

    #[test]
    fn claiming_twice_pays_once() {
        let weight = stake_weight(1_000_000, WEIGHT_SCALE).unwrap();
        let acc = acc_increment(500, weight).unwrap();

        let first = pending(weight, acc, 0).unwrap();
        assert_eq!(first, 500);

        let debt = gross_scaled(weight, acc).unwrap();
        let second = pending(weight, acc, debt).unwrap();
        assert_eq!(second, 0, "the accumulator has not moved, so nothing more is owed");
    }

    /// The invariant the whole program exists to hold.
    ///
    /// In the style of `lib/vesting.test.ts`, which loops 180 awkward
    /// combinations rather than picking one: whatever the pot, the stake sizes
    /// and the multipliers, the sum of what everyone is owed never exceeds what
    /// has actually been emitted.
    #[test]
    fn nobody_is_ever_owed_more_than_the_pot_released() {
        for pot in [1u64, 7, 99, 1_000_000, 1_000_000_007, u64::MAX / 1_000_000] {
            for stakes in [
                vec![1u64],
                vec![1, 1],
                vec![1, 999_999],
                vec![333, 333, 334],
                vec![1, 10, 100, 1_000, 10_000],
                vec![u64::MAX / 1_000_000_000, 1],
            ] {
                for mult in [WEIGHT_SCALE, 2 * WEIGHT_SCALE, MAX_WEIGHT_CAP] {
                    let weights: Vec<u128> = stakes
                        .iter()
                        .map(|a| stake_weight(*a, mult).unwrap())
                        .collect();
                    let total_weighted: u128 = weights.iter().sum();
                    if total_weighted == 0 {
                        continue;
                    }

                    // Release the whole pot in one step, then in ten, and check
                    // both. Splitting it is where per-step flooring accumulates.
                    for steps in [1u64, 10] {
                        let mut acc = 0u128;
                        let mut emitted_total = 0u64;
                        for _ in 0..steps {
                            let slice = pot / steps;
                            emitted_total += slice;
                            acc += acc_increment(slice, total_weighted).unwrap();
                        }

                        let owed: u64 = weights
                            .iter()
                            .map(|w| pending(*w, acc, 0).unwrap())
                            .sum();

                        assert!(
                            owed <= emitted_total,
                            "owed {owed} exceeds emitted {emitted_total} \
                             (pot {pot}, stakes {stakes:?}, mult {mult}, steps {steps})"
                        );
                    }
                }
            }
        }
    }
}
