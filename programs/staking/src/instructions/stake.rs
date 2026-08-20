use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::error::StakingError;
use crate::fee;
use crate::math;
use crate::state::{StakeEntry, StakePool};

#[derive(Accounts)]
#[instruction(nonce: u8)]
pub struct Stake<'info> {
    #[account(mut)]
    pub staker: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.authority.as_ref(), pool.stake_mint.as_ref(), &[pool.nonce]],
        bump = pool.bump,
    )]
    pub pool: Account<'info, StakePool>,

    #[account(
        init,
        payer = staker,
        space = StakeEntry::LEN,
        seeds = [ENTRY_SEED, pool.key().as_ref(), staker.key().as_ref(), &[nonce]],
        bump
    )]
    pub entry: Account<'info, StakeEntry>,

    #[account(mut, address = pool.stake_vault)]
    pub stake_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = staker_token_account.mint == pool.stake_mint @ StakingError::VaultMintMismatch,
        constraint = staker_token_account.owner == staker.key() @ StakingError::VaultMintMismatch,
    )]
    pub staker_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: pinned to our treasury by the `address` constraint.
    #[account(mut, address = TREASURY)]
    pub treasury: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_stake(ctx: Context<Stake>, nonce: u8, amount: u64, duration: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let pool = &mut ctx.accounts.pool;

    // FIRST, ALWAYS. Everything below reads `acc_reward_per_weight`, and a stale
    // accumulator sets this entry's opening debt to the wrong number, which pays
    // them for time before they arrived.
    pool.accrue(now)?;

    require!(now < pool.end_ts, StakingError::PoolClosed);
    require!(amount > 0, StakingError::ZeroAmount);

    let unlock_ts = now
        .checked_add(duration)
        .ok_or(StakingError::MathOverflow)?;
    require!(unlock_ts <= pool.end_ts, StakingError::LockOutlastsPool);

    // Also range-checks `duration` against the pool's min and max.
    let multiplier = math::weight_multiplier(
        duration,
        pool.min_duration,
        pool.max_duration,
        pool.max_weight,
    )?;
    let weight = math::stake_weight(amount, multiplier)?;

    // A deposit so small its weight floors to zero would earn nothing forever
    // while still costing rent and a fee. Refuse it rather than take the money.
    require!(weight > 0, StakingError::ZeroAmount);

    token::transfer(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            Transfer {
                from: ctx.accounts.staker_token_account.to_account_info(),
                to: ctx.accounts.stake_vault.to_account_info(),
                authority: ctx.accounts.staker.to_account_info(),
            },
        ),
        amount,
    )?;

    pool.total_staked = pool
        .total_staked
        .checked_add(amount)
        .ok_or(StakingError::MathOverflow)?;
    pool.total_weighted = pool
        .total_weighted
        .checked_add(weight)
        .ok_or(StakingError::MathOverflow)?;

    let entry = &mut ctx.accounts.entry;
    entry.owner = ctx.accounts.staker.key();
    entry.stake_pool = pool.key();
    entry.amount = amount;
    entry.weight = weight;
    entry.duration = duration;
    entry.start_ts = now;
    entry.unlock_ts = unlock_ts;
    // The opening debt is everything the pool has ALREADY paid out per unit of
    // weight. Without it, a wallet that stakes on the last day claims the whole
    // pool's history the moment it arrives.
    entry.reward_debt = math::gross_scaled(weight, pool.acc_reward_per_weight)?;
    entry.rewards_claimed = 0;
    entry.nonce = nonce;
    entry.bump = ctx.bumps.entry;
    entry.reserved = [0u8; 64];

    fee::collect(
        &ctx.accounts.staker,
        &ctx.accounts.treasury,
        &ctx.accounts.system_program,
        fee::stake_fee(),
    )?;

    Ok(())
}
