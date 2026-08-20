use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::error::StakingError;
use crate::state::{StakeEntry, StakePool};

#[derive(Accounts)]
pub struct Claim<'info> {
    pub staker: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.authority.as_ref(), pool.stake_mint.as_ref(), &[pool.nonce]],
        bump = pool.bump,
    )]
    pub pool: Account<'info, StakePool>,

    /// The entry being claimed against.
    ///
    /// Two constraints, and both matter. `has_one = owner` would only check the
    /// field; pairing it with the signer is what stops one wallet claiming
    /// another's rewards into its own account. The `stake_pool` check stops an
    /// entry from a different pool being pointed at this pool's vault.
    #[account(
        mut,
        constraint = entry.owner == staker.key() @ StakingError::VaultMintMismatch,
        constraint = entry.stake_pool == pool.key() @ StakingError::VaultMintMismatch,
    )]
    pub entry: Account<'info, StakeEntry>,

    #[account(mut, address = pool.reward_vault)]
    pub reward_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = staker_reward_account.mint == pool.reward_mint @ StakingError::VaultMintMismatch,
        constraint = staker_reward_account.owner == staker.key() @ StakingError::VaultMintMismatch,
    )]
    pub staker_reward_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

pub fn handle_claim(ctx: Context<Claim>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    ctx.accounts.pool.accrue(now)?;

    let acc = ctx.accounts.pool.acc_reward_per_weight;
    let pending = ctx.accounts.entry.pending(acc)?;

    if pending == 0 {
        // Not an error. Claiming twice in the same slot, or claiming from a pool
        // that has not moved, is a wasted transaction rather than a mistake.
        return Ok(());
    }

    // Written BEFORE the transfer. If the transfer fails the whole instruction
    // reverts, so ordering cannot leak tokens; doing it first means there is no
    // arrangement of a future edit where the payout happens and the debt does
    // not.
    ctx.accounts.entry.reward_debt = ctx.accounts.entry.gross_scaled(acc)?;
    ctx.accounts.entry.rewards_claimed = ctx
        .accounts
        .entry
        .rewards_claimed
        .checked_add(pending)
        .ok_or(StakingError::MathOverflow)?;
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
    )
}

/// Move rewards out of the vault, signed by the pool PDA.
///
/// Shared with `unstake`, which pays the same way before returning principal.
pub fn transfer_reward<'info>(
    pool: &Account<'info, StakePool>,
    reward_vault: &Box<Account<'info, TokenAccount>>,
    destination: &Box<Account<'info, TokenAccount>>,
    token_program: &Program<'info, Token>,
    amount: u64,
) -> Result<()> {
    let authority = pool.authority;
    let stake_mint = pool.stake_mint;
    let nonce = pool.nonce;
    let bump = pool.bump;

    let seeds: &[&[u8]] = &[
        POOL_SEED,
        authority.as_ref(),
        stake_mint.as_ref(),
        core::slice::from_ref(&nonce),
        core::slice::from_ref(&bump),
    ];

    token::transfer(
        CpiContext::new_with_signer(
            token_program.key(),
            Transfer {
                from: reward_vault.to_account_info(),
                to: destination.to_account_info(),
                authority: pool.to_account_info(),
            },
            &[seeds],
        ),
        amount,
    )
}
