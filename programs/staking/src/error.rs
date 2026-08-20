use anchor_lang::prelude::*;

#[error_code]
pub enum StakingError {
    #[msg("Arithmetic overflowed.")]
    MathOverflow,

    #[msg("The lock range is invalid: the shortest lock must be at least one second and no longer than the longest.")]
    InvalidDurationRange,

    #[msg("The multiplier must be between 1.0x and 10x.")]
    InvalidMaxWeight,

    #[msg("The pool must end after it starts.")]
    InvalidPoolWindow,

    #[msg("A pool needs a reward pot greater than zero.")]
    EmptyRewardPot,

    #[msg("The amount must be greater than zero.")]
    ZeroAmount,

    #[msg("That lock length is outside the range this pool allows.")]
    DurationOutOfRange,

    #[msg("This pool has closed and is not accepting new stakes.")]
    PoolClosed,

    #[msg("The lock would end after the pool does. Choose a shorter lock.")]
    LockOutlastsPool,

    #[msg("These tokens are still locked. The lock has not reached its end.")]
    StillLocked,

    #[msg("The pool has not reached its end date yet.")]
    PoolNotEnded,

    #[msg("Tokens are still staked in this pool. It can only be closed once every stake has been withdrawn.")]
    PoolNotEmpty,

    #[msg("The stake mint and the reward mint of the vaults do not match the pool.")]
    VaultMintMismatch,
}
