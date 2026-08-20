//! Fixed-pot SPL token staking with duration-weighted rewards.
//!
//! A creator deposits a pot of reward tokens and fixes the terms. Holders lock
//! tokens for a duration of their choosing inside the pool's range, and share
//! the pot in proportion to `amount x multiplier x time`. Locks cannot be broken
//! early. Whatever the stakers do not earn goes back to the creator at the end.
//!
//! WHAT THIS DELIBERATELY IS NOT: a fixed-APY program. The creator cannot
//! promise a rate. Promising one means an unbounded liability against a bounded
//! pot, and the person who finds the shortfall is whoever claims last. Here the
//! pot is the ceiling, by construction, so the failure does not exist.
//!
//! The full design and threat model live in `docs/staking-program.md` in the
//! deploy-a-token repository. Read it before changing anything in `math.rs` or
//! the field order in `state.rs`.

use anchor_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod fee;
pub mod instructions;
pub mod math;
pub mod state;

#[cfg(test)]
mod simulation;

use instructions::*;

declare_id!("GVeXtwSWuSF9zhSBBzDGAusrSVrwKwfYTPuJL42NCnjQ");

#[program]
pub mod deploy_a_token_staking {
    use super::*;

    /// Create a pool and fund its reward pot. Charges the creation fee.
    ///
    /// Everything set here is permanent. There is no instruction to edit a pool,
    /// and that is the point: a staker locking tokens for six months is relying
    /// on terms nobody can move afterwards.
    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        nonce: u8,
        params: PoolParams,
    ) -> Result<()> {
        instructions::initialize_pool::handle_initialize_pool(ctx, nonce, params)
    }

    /// Lock `amount` for `duration` seconds. Charges the per-stake fee.
    pub fn stake(ctx: Context<Stake>, nonce: u8, amount: u64, duration: i64) -> Result<()> {
        instructions::stake::handle_stake(ctx, nonce, amount, duration)
    }

    /// Take whatever this entry has earned so far. Available at any time.
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        instructions::claim::handle_claim(ctx)
    }

    /// Take back the principal, and anything still owed, after the lock ends.
    /// Closes the entry and returns its rent.
    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        instructions::unstake::handle_unstake(ctx)
    }

    /// Wind up a finished, empty pool and return the unearned residue.
    pub fn close_pool(ctx: Context<ClosePool>) -> Result<()> {
        instructions::close_pool::handle_close_pool(ctx)
    }
}
