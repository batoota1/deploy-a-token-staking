use anchor_lang::prelude::*;

use crate::error::StakingError;
use crate::math;

/// A staking pool: one token, one reward pot, one lock range.
///
/// FIXED SIZE, AND THE FIELD ORDER IS LOAD-BEARING.
///
/// The frontend lists every pool with a single `getProgramAccounts` call, and on
/// mainnet that only works if the request can be narrowed. Measured against a
/// live staking program with 64,360 accounts, an unfiltered scan is 14 MB even
/// with `dataSlice` set to zero. The filter is what saves you, not the slice.
///
/// So two things must stay true:
///
///   1. This account is a FIXED size, different from `StakeEntry`, so a
///      `dataSize` filter separates the two.
///   2. Every field the pool directory renders sits in the first 184 bytes, so
///      `dataSlice { offset: 0, length: 184 }` fetches a whole row and nothing
///      more.
///
/// Adding a `Vec`, a `String`, or a directory-visible field past byte 184 breaks
/// the list on mainnet, not in a test. See docs/staking-program.md, section 2.
///
/// Borsh rather than `zero_copy` on purpose: Borsh packs with no alignment
/// padding, so declaration order IS byte order and the offsets below are exactly
/// what they say. `repr(C)` would let the compiler insert padding that
/// declaration order does not show.
#[account]
pub struct StakePool {
    /// Offset 8. The creator. Only they can close the pool.
    pub authority: Pubkey,
    /// Offset 40. The token people stake.
    pub stake_mint: Pubkey,
    /// Offset 72. The token they earn. May be the same mint.
    pub reward_mint: Pubkey,
    /// Offset 104. Principal currently staked, in base units.
    pub total_staked: u64,
    /// Offset 112. Sum of every live entry's weight.
    pub total_weighted: u128,
    /// Offset 128. The pot, fixed at creation.
    pub reward_total: u64,
    /// Offset 136. How much of the pot has been released for claiming.
    /// Can never exceed `reward_total`; that clamp is the bound on the whole
    /// program.
    pub rewards_emitted: u64,
    /// Offset 144. When emission starts.
    pub start_ts: i64,
    /// Offset 152. When emission stops and no new stake is accepted.
    pub end_ts: i64,
    /// Offset 160. Shortest lock, in seconds.
    pub min_duration: i64,
    /// Offset 168. Longest lock, in seconds.
    pub max_duration: i64,
    /// Offset 176. Multiplier at `max_duration`, scaled by `WEIGHT_SCALE`.
    /// END OF THE DIRECTORY SLICE. Anything below is accounting a list never
    /// renders.
    pub max_weight: u64,
    /// Offset 184. Rewards per unit of weight, scaled by `ACC_SCALE`.
    pub acc_reward_per_weight: u128,
    /// Offset 200. Last time `accrue` ran.
    pub last_update_ts: i64,
    /// Offset 208.
    pub stake_vault: Pubkey,
    /// Offset 240.
    pub reward_vault: Pubkey,
    /// Offset 272. Paid out so far. Only ever compared against
    /// `rewards_emitted`; never an input to a calculation.
    pub rewards_claimed: u64,
    /// Offset 280.
    pub nonce: u8,
    /// Offset 281.
    pub bump: u8,
    /// Offset 282.
    pub stake_vault_bump: u8,
    /// Offset 283.
    pub reward_vault_bump: u8,
    /// Offset 284. Spare room so a later field is a logic change and not a
    /// migration.
    pub reserved: [u8; 56],
}

impl StakePool {
    /// Total account size including Anchor's 8-byte discriminator.
    pub const LEN: usize = 340;

    /// How much of each account the pool directory needs to fetch.
    pub const DIRECTORY_SLICE: usize = 184;

    /// Bring `acc_reward_per_weight` up to `now`.
    ///
    /// MUST BE THE FIRST THING every state-changing instruction does. Claiming,
    /// staking or unstaking against a stale accumulator pays the wrong number.
    ///
    /// Two behaviours here are deliberate and must not be "tidied":
    ///
    ///   - Emission stops at `end_ts`. `now` is clamped, so a pool left running
    ///     for a year past its end does not keep emitting.
    ///   - When nothing is staked, the accumulator does NOT advance. Those
    ///     rewards are not emitted into an empty pool and lost; they stay in the
    ///     pot for whoever stakes next, and whatever is left at the end goes
    ///     back to the creator. Advancing here would also divide by zero.
    pub fn accrue(&mut self, now: i64) -> Result<()> {
        let now = now.min(self.end_ts);
        if now <= self.last_update_ts {
            return Ok(());
        }

        if self.total_weighted == 0 {
            self.last_update_ts = now;
            return Ok(());
        }

        let elapsed = now
            .checked_sub(self.last_update_ts)
            .ok_or(StakingError::MathOverflow)?;
        let lifetime = self
            .end_ts
            .checked_sub(self.start_ts)
            .ok_or(StakingError::MathOverflow)?;

        let uncapped = math::emitted_over(elapsed, self.reward_total, lifetime)?;

        // The clamp, and it is the bound the whole program rests on: emission
        // can never exceed the pot, so the vault can always cover what it owes.
        let headroom = self
            .reward_total
            .checked_sub(self.rewards_emitted)
            .ok_or(StakingError::MathOverflow)?;
        let emitted = uncapped.min(headroom);

        self.rewards_emitted = self
            .rewards_emitted
            .checked_add(emitted)
            .ok_or(StakingError::MathOverflow)?;
        self.acc_reward_per_weight = self
            .acc_reward_per_weight
            .checked_add(math::acc_increment(emitted, self.total_weighted)?)
            .ok_or(StakingError::MathOverflow)?;
        self.last_update_ts = now;

        Ok(())
    }
}

/// One deposit by one wallet. Every stake creates a new one.
///
/// FIXED SIZE, and `owner` is at offset 8 on purpose.
///
/// The staker page finds a wallet's positions with a `memcmp` filter on that
/// offset. It is the same shape as Jupiter Lock's escrow, whose recipient also
/// sits at offset 8, and which our vesting tool already runs against mainnet
/// every day. `stake_pool` at offset 40 gives the second useful filter: every
/// entry in one pool.
#[account]
pub struct StakeEntry {
    /// Offset 8. The staker. The memcmp target.
    pub owner: Pubkey,
    /// Offset 40. The pool. The other memcmp target.
    pub stake_pool: Pubkey,
    /// Offset 72. Principal, in base units.
    pub amount: u64,
    /// Offset 80. `amount` scaled by the duration multiplier.
    pub weight: u128,
    /// Offset 96. The lock length chosen at stake time, in seconds.
    pub duration: i64,
    /// Offset 104.
    pub start_ts: i64,
    /// Offset 112. Before this, `unstake` is refused.
    pub unlock_ts: i64,
    /// Offset 120. `weight * acc_reward_per_weight` as of the last claim, LEFT
    /// SCALED by `ACC_SCALE`. The difference against the current value, divided
    /// once, is what is owed.
    ///
    /// Storing it scaled is not an optimisation. Storing it already divided lets
    /// a claim compute `floor(a) - floor(b)`, which can exceed `floor(a - b)` by
    /// one unit, and a property test proved that repeated claims then pay out
    /// more than the pool ever emitted. See `math::gross_scaled`.
    pub reward_debt: u128,
    /// Offset 136. Paid out to this entry so far. For display; never an input.
    pub rewards_claimed: u64,
    /// Offset 144.
    pub nonce: u8,
    /// Offset 145.
    pub bump: u8,
    /// Offset 146.
    pub reserved: [u8; 64],
}

impl StakeEntry {
    pub const LEN: usize = 210;

    /// Byte offset of `owner`, which the frontend's memcmp filter depends on.
    pub const OWNER_OFFSET: usize = 8;
    /// Byte offset of `stake_pool`.
    pub const POOL_OFFSET: usize = 40;

    /// What this entry is owed right now, given the pool's current accumulator.
    ///
    /// Floors, like everything else that pays out, so the pot can always cover
    /// the final claim.
    pub fn pending(&self, acc_reward_per_weight: u128) -> Result<u64> {
        math::pending(self.weight, acc_reward_per_weight, self.reward_debt)
    }

    /// The scaled figure `reward_debt` is set to after a claim.
    pub fn gross_scaled(&self, acc_reward_per_weight: u128) -> Result<u128> {
        math::gross_scaled(self.weight, acc_reward_per_weight)
    }
}

// The sizes above are not documentation, they are a contract with the frontend's
// dataSize filters. If a field is added or reordered without updating LEN, the
// pool list and the "my stakes" lookup both silently return nothing, which reads
// as "you have no stakes" rather than as a bug.
#[cfg(test)]
mod layout_tests {
    use super::*;
    use anchor_lang::AnchorSerialize;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    fn ser<T: AnchorSerialize>(v: &T) -> Vec<u8> {
        let mut out = Vec::new();
        v.serialize(&mut out).unwrap();
        out
    }

    fn empty_pool() -> StakePool {
        StakePool {
            authority: Pubkey::new_unique(),
            stake_mint: Pubkey::new_unique(),
            reward_mint: Pubkey::new_unique(),
            total_staked: 0,
            total_weighted: 0,
            reward_total: 0,
            rewards_emitted: 0,
            start_ts: 0,
            end_ts: 0,
            min_duration: 0,
            max_duration: 0,
            max_weight: 0,
            acc_reward_per_weight: 0,
            last_update_ts: 0,
            stake_vault: Pubkey::new_unique(),
            reward_vault: Pubkey::new_unique(),
            rewards_claimed: 0,
            nonce: 0,
            bump: 0,
            stake_vault_bump: 0,
            reward_vault_bump: 0,
            reserved: [0u8; 56],
        }
    }

    fn empty_entry() -> StakeEntry {
        StakeEntry {
            owner: Pubkey::new_unique(),
            stake_pool: Pubkey::new_unique(),
            amount: 0,
            weight: 0,
            duration: 0,
            start_ts: 0,
            unlock_ts: 0,
            reward_debt: 0,
            rewards_claimed: 0,
            nonce: 0,
            bump: 0,
            reserved: [0u8; 64],
        }
    }

    /// `space = LEN` must equal what Borsh actually writes, plus the 8-byte
    /// discriminator. Too small and every write fails; too large and every pool
    /// overpays rent forever.
    #[test]
    fn the_declared_sizes_are_the_real_serialized_sizes() {
        assert_eq!(
            ser(&empty_pool()).len() + 8,
            StakePool::LEN,
            "StakePool::LEN disagrees with what Borsh writes"
        );
        assert_eq!(
            ser(&empty_entry()).len() + 8,
            StakeEntry::LEN,
            "StakeEntry::LEN disagrees with what Borsh writes"
        );
    }

    /// The frontend finds a wallet's positions with
    /// `memcmp(offset: 8, wallet)`. If `owner` ever stops being the first field,
    /// that filter matches nothing and the staker page tells people they have no
    /// stakes, which is worse than an error.
    #[test]
    fn owner_is_where_the_memcmp_filter_expects_it() {
        let entry = empty_entry();
        let bytes = ser(&entry);
        let at = StakeEntry::OWNER_OFFSET - 8; // discriminator is not in the borsh output
        assert_eq!(
            &bytes[at..at + 32],
            entry.owner.as_ref(),
            "owner is not at byte {} of the account",
            StakeEntry::OWNER_OFFSET
        );

        let at = StakeEntry::POOL_OFFSET - 8;
        assert_eq!(&bytes[at..at + 32], entry.stake_pool.as_ref());
    }

    /// Emit the fixture the FRONTEND pins its byte offsets against.
    ///
    /// Ignored by default because it prints rather than asserts. Run it with
    ///
    ///     cargo test --lib emit_layout_fixture -- --ignored --nocapture
    ///
    /// and paste the output into `lib/fixtures/stake-accounts.json` in the
    /// deploy-a-token repo. That file is what makes `lib/staking.ts`'s offsets a
    /// cross-language contract rather than two lists of numbers that agree
    /// today: if a field moves here, the frontend's test fails there.
    ///
    /// Every value below is distinctive on purpose. Zeroes and small integers
    /// look identical at the wrong offset; `0x1122334455667788` does not.
    #[test]
    #[ignore]
    fn emit_layout_fixture() {
        use anchor_lang::AnchorSerialize;

        let mut pool = empty_pool();
        pool.total_staked = 0x1122334455667788;
        pool.total_weighted = 0x0102030405060708090a0b0c0d0e0f10;
        pool.reward_total = 0xaabbccddeeff0011;
        pool.rewards_emitted = 0x2233445566778899;
        pool.start_ts = 1_700_000_001;
        pool.end_ts = 1_700_000_002;
        pool.min_duration = 86_400;
        pool.max_duration = 2_592_000;
        pool.max_weight = 3_000_000_000;
        pool.acc_reward_per_weight = 0x1112131415161718191a1b1c1d1e1f20;
        pool.last_update_ts = 1_700_000_003;
        pool.rewards_claimed = 0x99aabbccddeeff00;
        pool.nonce = 7;

        let mut entry = empty_entry();
        entry.amount = 0x1122334455667788;
        entry.weight = 0x0102030405060708090a0b0c0d0e0f10;
        entry.duration = 604_800;
        entry.start_ts = 1_700_000_004;
        entry.unlock_ts = 1_700_000_005;
        entry.reward_debt = 0x2122232425262728292a2b2c2d2e2f30;
        entry.rewards_claimed = 0x33445566778899aa;
        entry.nonce = 9;

        let mut pb = Vec::new();
        pool.serialize(&mut pb).unwrap();
        let mut eb = Vec::new();
        entry.serialize(&mut eb).unwrap();

        // The 8-byte discriminator is not part of the borsh body, so prepend it
        // to match what an RPC actually returns.
        let pool_full = [StakePool::DISCRIMINATOR, &pb[..]].concat();
        let entry_full = [StakeEntry::DISCRIMINATOR, &eb[..]].concat();

        println!("{{");
        println!("  \"pool\": {{");
        println!("    \"authority\": \"{}\",", pool.authority);
        println!("    \"stakeMint\": \"{}\",", pool.stake_mint);
        println!("    \"rewardMint\": \"{}\",", pool.reward_mint);
        println!("    \"stakeVault\": \"{}\",", pool.stake_vault);
        println!("    \"rewardVault\": \"{}\",", pool.reward_vault);
        println!("    \"base64\": \"{}\"", STANDARD.encode(&pool_full));
        println!("  }},");
        println!("  \"entry\": {{");
        println!("    \"owner\": \"{}\",", entry.owner);
        println!("    \"stakePool\": \"{}\",", entry.stake_pool);
        println!("    \"base64\": \"{}\"", STANDARD.encode(&entry_full));
        println!("  }}");
        println!("}}");
    }

    /// The pool directory fetches only `DIRECTORY_SLICE` bytes per row. That has
    /// to reach the end of `max_weight`, the last field a row renders.
    #[test]
    fn the_directory_slice_covers_every_field_a_pool_row_shows() {
        let mut pool = empty_pool();
        pool.max_weight = u64::MAX;
        let bytes = ser(&pool);

        let end = StakePool::DIRECTORY_SLICE - 8;
        assert_eq!(
            &bytes[end - 8..end],
            &u64::MAX.to_le_bytes(),
            "max_weight does not end exactly at the slice boundary"
        );
        assert!(StakePool::DIRECTORY_SLICE < StakePool::LEN);
    }
}

// discriminator + 5 Pubkeys + 10 eight-byte fields + 2 u128 + 4 u8 + reserved
const _: () = assert!(StakePool::LEN == 8 + 32 * 5 + 8 * 10 + 16 * 2 + 4 + 56);
// discriminator + 2 Pubkeys + 5 eight-byte fields + 2 u128 + 2 u8 + reserved
const _: () = assert!(StakeEntry::LEN == 8 + 32 * 2 + 8 * 5 + 16 * 2 + 2 + 64);
// The dataSize filter can only tell the two apart if they differ.
const _: () = assert!(StakePool::LEN != StakeEntry::LEN);
// The directory slice must end exactly after `max_weight`:
// discriminator + 3 Pubkeys + 8 eight-byte fields + total_weighted.
const _: () = assert!(StakePool::DIRECTORY_SLICE == 8 + 32 * 3 + 8 * 8 + 16);
