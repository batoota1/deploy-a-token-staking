use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::error::StakingError;
use crate::instructions::claim::transfer_reward;
use crate::state::{StakeEntry, StakePool};

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut)]
    pub staker: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.authority.as_ref(), pool.stake_mint.as_ref(), &[pool.nonce]],
        bump = pool.bump,
    )]
    pub pool: Account<'info, StakePool>,

    /// Closed at the end of this instruction, returning its rent to the staker.
    ///
    /// `close` rather than leaving it behind: the rent is 0.00235248 SOL of the
    /// staker's money, and a tool that returns the balance but strands the rent
    /// is the failure `lib/tools.ts` records against the SOL Wrapper. Anchor's
    /// `close` also zeroes the discriminator, which is what stops a closed entry
    /// being revived and drained a second time.
    #[account(
        mut,
        close = staker,
        constraint = entry.owner == staker.key() @ StakingError::VaultMintMismatch,
        constraint = entry.stake_pool == pool.key() @ StakingError::VaultMintMismatch,
    )]
    pub entry: Account<'info, StakeEntry>,

    #[account(mut, address = pool.stake_vault)]
    pub stake_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = pool.reward_vault)]
    pub reward_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = staker_token_account.mint == pool.stake_mint @ StakingError::VaultMintMismatch,
        constraint = staker_token_account.owner == staker.key() @ StakingError::VaultMintMismatch,
    )]
    pub staker_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = staker_reward_account.mint == pool.reward_mint @ StakingError::VaultMintMismatch,
        constraint = staker_reward_account.owner == staker.key() @ StakingError::VaultMintMismatch,
    )]
    pub staker_reward_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn handle_unstake(ctx: Context<Unstake>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    ctx.accounts.pool.accrue(now)?;

    require!(
        now >= ctx.accounts.entry.unlock_ts,
        StakingError::StillLocked
    );

    let acc = ctx.accounts.pool.acc_reward_per_weight;
    let pending = ctx.accounts.entry.pending(acc)?;
    let amount = ctx.accounts.entry.amount;
    let weight = ctx.accounts.entry.weight;

    // The weight leaves the pool BEFORE anything is paid out. From this point on
    // the entry is no longer earning, which is what makes closing it safe.
    ctx.accounts.pool.total_staked = ctx
        .accounts
        .pool
        .total_staked
        .checked_sub(amount)
        .ok_or(StakingError::MathOverflow)?;
    ctx.accounts.pool.total_weighted = ctx
        .accounts
        .pool
        .total_weighted
        .checked_sub(weight)
        .ok_or(StakingError::MathOverflow)?;

    if pending > 0 {
        ctx.accounts.pool.rewards_claimed = ctx
            .accounts
            .pool
            .rewards_claimed
            .checked_add(pending)
            .ok_or(StakingError::MathOverflow)?;

        transfer_reward(
            &ctx.accounts.pool,
            &ctx.accounts.reward_vault,
            &ctx.accounts.staker_reward_account,
            &ctx.accounts.token_program,
            pending,
        )?;
    }

    // Principal last. It is the part that must come back whatever else happened,
    // so it is the part with the least standing between it and the staker.
    let authority = ctx.accounts.pool.authority;
    let stake_mint = ctx.accounts.pool.stake_mint;
    let nonce = ctx.accounts.pool.nonce;
    let bump = ctx.accounts.pool.bump;
    let seeds: &[&[u8]] = &[
        POOL_SEED,
        authority.as_ref(),
        stake_mint.as_ref(),
        core::slice::from_ref(&nonce),
        core::slice::from_ref(&bump),
    ];

    token::transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            Transfer {
                from: ctx.accounts.stake_vault.to_account_info(),
                to: ctx.accounts.staker_token_account.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            },
            &[seeds],
        ),
        amount,
    )
}
