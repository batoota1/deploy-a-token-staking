use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::error::StakingError;
use crate::state::StakePool;

#[derive(Accounts)]
pub struct ClosePool<'info> {
    #[account(mut, address = pool.authority)]
    pub creator: Signer<'info>,

    #[account(
        mut,
        close = creator,
        seeds = [POOL_SEED, pool.authority.as_ref(), pool.stake_mint.as_ref(), &[pool.nonce]],
        bump = pool.bump,
    )]
    pub pool: Account<'info, StakePool>,

    #[account(mut, address = pool.stake_vault)]
    pub stake_vault: Box<Account<'info, TokenAccount>>,

    #[account(mut, address = pool.reward_vault)]
    pub reward_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = creator_reward_account.mint == pool.reward_mint @ StakingError::VaultMintMismatch,
        constraint = creator_reward_account.owner == creator.key() @ StakingError::VaultMintMismatch,
    )]
    pub creator_reward_account: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

/// Wind up a finished pool and return whatever the stakers did not earn.
///
/// TWO CONDITIONS, AND THE SECOND IS A DELIBERATE ASYMMETRY.
///
/// The pool must have ended, and every stake must have been withdrawn. The
/// second one means a single staker who never comes back to unstake keeps the
/// creator's residue locked up indefinitely.
///
/// That is the right way round. The alternative is a creator able to close a
/// pool while somebody still has tokens in it, and there is no version of that
/// which is safe. The residue is the creator's least urgent money; a staker's
/// principal is their most urgent. When the two are in tension, the staker wins.
///
/// A creator who wants their residue back sooner has a real option: the pool's
/// terms are public, so they can ask. What they cannot do is take it.
pub fn handle_close_pool(ctx: Context<ClosePool>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(now >= ctx.accounts.pool.end_ts, StakingError::PoolNotEnded);
    require!(
        ctx.accounts.pool.total_staked == 0,
        StakingError::PoolNotEmpty
    );

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

    // Whatever is actually in the vault, read from the vault rather than
    // computed. With `total_staked == 0` every entry has been unstaked, and
    // unstaking claims, so what remains is the part of the pot that was never
    // emitted plus the dust left by flooring. Reading the balance is safe HERE,
    // and only here, because nothing downstream depends on it: this is the last
    // instruction the pool will ever run.
    let residue = ctx.accounts.reward_vault.amount;

    if residue > 0 {
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                Transfer {
                    from: ctx.accounts.reward_vault.to_account_info(),
                    to: ctx.accounts.creator_reward_account.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                &[seeds],
            ),
            residue,
        )?;
    }

    // Both vaults close, returning their rent to the creator who paid it.
    for vault in [
        ctx.accounts.reward_vault.to_account_info(),
        ctx.accounts.stake_vault.to_account_info(),
    ] {
        token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            CloseAccount {
                account: vault,
                destination: ctx.accounts.creator.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            },
            &[seeds],
        ))?;
    }

    Ok(())
}
