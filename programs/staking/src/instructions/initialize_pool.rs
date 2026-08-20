use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

use crate::constants::*;
use crate::error::StakingError;
use crate::fee;
use crate::state::StakePool;

/// What the creator chooses. Fixed at creation and never editable.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct PoolParams {
    /// How long the pool runs, in seconds. Emission is spread evenly across it.
    pub pool_duration: i64,
    /// Shortest lock a staker may pick, in seconds.
    pub min_duration: i64,
    /// Longest lock a staker may pick, in seconds.
    pub max_duration: i64,
    /// Multiplier at `max_duration`, scaled by `WEIGHT_SCALE`. `1_000_000_000`
    /// means no bonus for locking longer.
    pub max_weight: u64,
    /// The pot, in base units of the reward mint. Moved in now, and this is the
    /// only time it can be funded.
    pub reward_amount: u64,
}

#[derive(Accounts)]
#[instruction(nonce: u8)]
pub struct InitializePool<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    pub stake_mint: Box<Account<'info, Mint>>,
    pub reward_mint: Box<Account<'info, Mint>>,

    #[account(
        init,
        payer = creator,
        space = StakePool::LEN,
        seeds = [POOL_SEED, creator.key().as_ref(), stake_mint.key().as_ref(), &[nonce]],
        bump
    )]
    pub pool: Account<'info, StakePool>,

    #[account(
        init,
        payer = creator,
        seeds = [STAKE_VAULT_SEED, pool.key().as_ref()],
        bump,
        token::mint = stake_mint,
        token::authority = pool,
    )]
    pub stake_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        init,
        payer = creator,
        seeds = [REWARD_VAULT_SEED, pool.key().as_ref()],
        bump,
        token::mint = reward_mint,
        token::authority = pool,
    )]
    pub reward_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = creator_reward_account.mint == reward_mint.key() @ StakingError::VaultMintMismatch,
        constraint = creator_reward_account.owner == creator.key() @ StakingError::VaultMintMismatch,
    )]
    pub creator_reward_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: pinned to our treasury by the `address` constraint, so a caller
    /// cannot substitute an account of their own and pay themselves the fee.
    #[account(mut, address = TREASURY)]
    pub treasury: UncheckedAccount<'info>,

    /// The ORIGINAL SPL token program, not Token-2022, and that is the
    /// Token-2022 refusal rather than a separate check.
    ///
    /// A Token-2022 mint with a transfer fee would break every number in this
    /// program: tokens received differ from tokens sent, so `total_staked` would
    /// say one thing and the vault would hold another. Typing this as
    /// `Program<Token>` means such a mint cannot reach us at all. It is the same
    /// approach Jupiter Lock v1 takes, and why our vesting tool is SPL-only.
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handle_initialize_pool(ctx: Context<InitializePool>, nonce: u8, params: PoolParams) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(params.pool_duration > 0, StakingError::InvalidPoolWindow);
    require!(params.min_duration >= 1, StakingError::InvalidDurationRange);
    require!(
        params.min_duration <= params.max_duration,
        StakingError::InvalidDurationRange
    );
    require!(
        params.max_weight >= WEIGHT_SCALE && params.max_weight <= MAX_WEIGHT_CAP,
        StakingError::InvalidMaxWeight
    );
    require!(params.reward_amount > 0, StakingError::EmptyRewardPot);

    // A lock that cannot finish before the pool does is a lock nobody should be
    // allowed to pick. Checking it here means `stake` never has to explain why
    // the pool's own settings made their choice impossible.
    require!(
        params.max_duration <= params.pool_duration,
        StakingError::InvalidPoolWindow
    );

    let end_ts = now
        .checked_add(params.pool_duration)
        .ok_or(StakingError::MathOverflow)?;

    // The pot moves in first. If this fails, nothing else in the transaction
    // happened either, so there is no state describing a pool that was never
    // funded.
    token::transfer(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            Transfer {
                from: ctx.accounts.creator_reward_account.to_account_info(),
                to: ctx.accounts.reward_vault.to_account_info(),
                authority: ctx.accounts.creator.to_account_info(),
            },
        ),
        params.reward_amount,
    )?;

    let pool = &mut ctx.accounts.pool;
    pool.authority = ctx.accounts.creator.key();
    pool.stake_mint = ctx.accounts.stake_mint.key();
    pool.reward_mint = ctx.accounts.reward_mint.key();
    pool.total_staked = 0;
    pool.total_weighted = 0;
    pool.reward_total = params.reward_amount;
    pool.rewards_emitted = 0;
    pool.start_ts = now;
    pool.end_ts = end_ts;
    pool.min_duration = params.min_duration;
    pool.max_duration = params.max_duration;
    pool.max_weight = params.max_weight;
    pool.acc_reward_per_weight = 0;
    pool.last_update_ts = now;
    pool.stake_vault = ctx.accounts.stake_vault.key();
    pool.reward_vault = ctx.accounts.reward_vault.key();
    pool.rewards_claimed = 0;
    pool.nonce = nonce;
    pool.bump = ctx.bumps.pool;
    pool.stake_vault_bump = ctx.bumps.stake_vault;
    pool.reward_vault_bump = ctx.bumps.reward_vault;
    pool.reserved = [0u8; 56];

    fee::collect(
        &ctx.accounts.creator,
        &ctx.accounts.treasury,
        &ctx.accounts.system_program,
        fee::pool_creation_fee(),
    )?;

    Ok(())
}
