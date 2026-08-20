pub mod claim;
pub mod close_pool;
pub mod initialize_pool;
pub mod stake;
pub mod unstake;

// Glob re-exports, which Anchor requires: `#[derive(Accounts)]` generates
// `__client_accounts_*` and `__cpi_client_accounts_*` modules alongside each
// struct, and `#[program]` resolves them through the crate root. Exporting the
// structs by name alone drops those and the program macro stops compiling.
//
// The handlers are therefore named `handle_<instruction>` rather than all being
// called `handler`, which would make five of them ambiguous in one namespace.
pub use claim::*;
pub use close_pool::*;
pub use initialize_pool::*;
pub use stake::*;
pub use unstake::*;
