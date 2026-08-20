use anchor_lang::prelude::*;

/// Fixed-point scale for the duration multiplier. `1_000_000_000` means 1.0x.
///
/// A stake locked for exactly `min_duration` gets this multiplier and no more.
pub const WEIGHT_SCALE: u64 = 1_000_000_000;

/// The largest multiplier a pool may offer: 10x.
///
/// This is a bound on the arithmetic as much as on the product. Every overflow
/// argument in `math.rs` needs an upper limit on `max_weight` to work against,
/// and "whatever a u64 holds" is not a useful one. Ten is also past the point
/// where a longer lock is a real incentive rather than a number on a page.
pub const MAX_WEIGHT_CAP: u64 = 10_000_000_000;

/// Fixed-point scale on `acc_reward_per_weight`: 1e18.
///
/// The accumulator is rewards-per-unit-of-weight, which is almost always far
/// below 1, so it is held scaled up. 1e18 is chosen for the same reason
/// MasterChef-style contracts choose it: enough headroom that a small first
/// stake in a large pool does not floor its share to zero.
pub const ACC_SCALE: u128 = 1_000_000_000_000_000_000;

/// Our fee for creating a pool: 0.5 SOL.
///
/// WHY THIS IS COMPILED IN RATHER THAN READ FROM A CONFIG ACCOUNT.
///
/// A config account would let us reprice without a program upgrade, but it adds
/// an account to every instruction, a second authority to secure, and a window
/// where the price a staker read is not the price they paid. A constant has one
/// cost: changing it needs an upgrade. Given the audit is the budget for this
/// program, the smaller surface wins.
///
/// Both fees are read through `fee.rs` and nowhere else, so moving to a config
/// account later is a change in one file rather than five.
pub const POOL_CREATION_FEE_LAMPORTS: u64 = 500_000_000;

/// Our fee per stake: 0.005 SOL.
///
/// Charged INSIDE the instruction, which is the whole reason this program
/// exists. Every other tool in the deploy-a-token repo appends its fee as a
/// separate `SystemProgram.transfer` that anyone forking the frontend can drop.
/// This one cannot be dropped without also dropping the stake.
///
/// For scale: the base network fee is 5,000 lamports, so this is a thousand
/// times the cost of the transaction and well under Smithii's 0.008 SOL.
pub const STAKE_FEE_LAMPORTS: u64 = 5_000_000;

/// Where both fees go.
///
/// COPIED FROM `lib/treasury.ts` IN THE deploy-a-token REPO AND NOWHERE ELSE.
///
/// A lookalike of this address is actively dusting our wallets, so it must never
/// be copied from a block explorer, a transaction history, or a message. Checked
/// on 2026-08-13 against the live `GET https://deployatoken.com/v1/config`,
/// which serves the same value.
///
/// A wrong address here is the one mistake in this program that succeeds: every
/// transaction confirms, the staker is happy, and the money simply arrives
/// somewhere else.
pub const TREASURY: Pubkey = pubkey!("HWi3LMkqSP92X3uHqsVqvBHsRwf3kjD9C6Sr6sS5z78X");

/// PDA seed prefixes.
///
/// Distinct per account type on purpose: a shared prefix is how two roles end up
/// deriving the same address, which is the "PDA sharing" class in the Solana
/// security checklist.
pub const POOL_SEED: &[u8] = b"pool";
pub const ENTRY_SEED: &[u8] = b"entry";
pub const STAKE_VAULT_SEED: &[u8] = b"stake_vault";
pub const REWARD_VAULT_SEED: &[u8] = b"reward_vault";
