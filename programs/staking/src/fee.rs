//! Our two fees, and the only place they are read.
//!
//! WHY THE FEE IS IN THE PROGRAM AT ALL.
//!
//! Every other tool in the deploy-a-token repo attaches its fee in the browser,
//! as a `SystemProgram.transfer` appended to the transaction. That is honest and
//! visible, and it is also trivially removable: anyone who forks the frontend
//! deletes one instruction and keeps the tool. It works because those tools wrap
//! programs we do not own, so there is nowhere else to put it.
//!
//! This program is ours, so the fee rides inside the instruction that does the
//! work. It cannot be dropped without dropping the stake.
//!
//! That comes with an obligation, and it is written here because this is the
//! file someone will read when they wonder about it: **the number the page shows
//! must equal the number this takes.** A fee that is harder to see is a fee that
//! has to be stated more plainly, not less.

use anchor_lang::prelude::*;
use anchor_lang::system_program;

use crate::constants::{POOL_CREATION_FEE_LAMPORTS, STAKE_FEE_LAMPORTS};

/// Charged once, to the creator, when a pool is made.
pub fn pool_creation_fee() -> u64 {
    POOL_CREATION_FEE_LAMPORTS
}

/// Charged to the staker on every stake.
pub fn stake_fee() -> u64 {
    STAKE_FEE_LAMPORTS
}

/// Move `lamports` from the signer to the treasury.
///
/// The treasury account is pinned by an `address = TREASURY` constraint on every
/// instruction that calls this, so the caller cannot substitute their own
/// account and pay themselves.
///
/// A zero amount transfers nothing rather than sending an empty instruction,
/// which matches `addPlatformFee` in the frontend: "a zero amount adds nothing
/// at all."
pub fn collect<'info>(
    payer: &Signer<'info>,
    treasury: &UncheckedAccount<'info>,
    system_program: &Program<'info, System>,
    lamports: u64,
) -> Result<()> {
    if lamports == 0 {
        return Ok(());
    }

    system_program::transfer(
        CpiContext::new(
            system_program.key(),
            system_program::Transfer {
                from: payer.to_account_info(),
                to: treasury.to_account_info(),
            },
        ),
        lamports,
    )
}
