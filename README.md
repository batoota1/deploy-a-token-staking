# deploy-a-token-staking

The Anchor program behind the Staking tool on deployatoken.com.

**Deployed to devnet only. Not audited. Do not put real money near this yet.**

| | |
|---|---|
| Devnet program | [`GVeXtwSWuSF9zhSBBzDGAusrSVrwKwfYTPuJL42NCnjQ`](https://explorer.solana.com/address/GVeXtwSWuSF9zhSBBzDGAusrSVrwKwfYTPuJL42NCnjQ?cluster=devnet) |
| Deployed | 2026-08-13, slot 483536887, 325,872 bytes |
| Upgrade authority | `2TJMWhAr37iHTvgufYXzNbkGwyA6pe2776BarQMm6U68` (a devnet key file, devnet only) |

**Verified on devnet, 2026-08-13.** A real pool went through create, stake,
claim, unstake and close against the deployed copy: 7/7, signatures recorded in
`docs/test-results-log.md` section 34 in the deploy-a-token repo. Run it yourself
with:

```bash
ANCHOR_PROVIDER_URL=<devnet rpc> ANCHOR_WALLET=<keypair> \
  npx ts-mocha -p ./tsconfig.json -t 300000 scripts/devnet-smoke.ts
```

The run cost about 0.51 SOL, most of it our own pool-creation fee.

**Note that our own fee applies on devnet.** It is a compiled-in constant with no
idea which cluster it is on, so a pool costs a real 0.5 devnet SOL to create and
each stake costs 0.005. That is good for testing, because the rehearsal exercises
the real fee path, and it is a difference from every other tool on the site,
which are free on testnet. One devnet rehearsal needs about 0.52 SOL.

## What it does

A creator deposits a pot of reward tokens and fixes the terms. Holders lock
tokens for a duration of their choosing inside the pool's range, and share the
pot in proportion to `amount x multiplier x time`. Locks cannot be broken early.
Whatever the stakers do not earn goes back to the creator at the end.

**Fixed pot, not fixed APY.** A creator cannot promise a rate. Promising one
means an unbounded liability against a bounded pot, and the person who finds the
shortfall is whoever claims last. Here the pot is the ceiling by construction, so
that failure does not exist. The cost is real and belongs on the page: the APY
moves as people join and leave.

| | |
|---|---|
| Program id | `GVeXtwSWuSF9zhSBBzDGAusrSVrwKwfYTPuJL42NCnjQ` |
| Instructions | `initialize_pool`, `stake`, `claim`, `unstake`, `close_pool` |
| Token standard | SPL only. Token-2022 is refused by typing the program as `Program<Token>` |
| Fees | 0.5 SOL to create a pool, 0.005 SOL per stake, both collected inside the instruction |
| Size | 327 KB, about 2.28 SOL of rent to deploy on mainnet |

The full design and threat model is `docs/staking-program.md` in the
[deploy-a-token](../deploy-a-token) repo. **Read it before changing `math.rs` or
the field order in `state.rs`.**

## Toolchain

Anchor moved a long way in 1.0 and most tutorials still assume the 0.3x line.

| Tool | Version |
|---|---|
| `anchor-lang` | 1.1.2 |
| Solana CLI | 4.1.1 (Anchor 1.0 targets Solana 3.x crates; the 4.x CLI builds them) |
| Rust | 1.97.1 |

`overflow-checks = true` is set in the workspace `Cargo.toml` and is not
optional: Cargo disables integer overflow checks in release builds, and without
it a multiplication that overflows wraps silently and the program carries on with
a wrong reward balance.

## Running the tests

```bash
cargo test --lib
```

24 unit tests: the reward math, the account layout, and three property tests.

```bash
anchor test --validator legacy
```

23 integration tests against a real validator: the happy path, every "must be
impossible" row from the threat model, and the donation attack. Takes about three
minutes, because pools genuinely have to end and locks genuinely have to expire.

`--validator legacy` is not optional here. Anchor 1.0 defaults to Surfpool, which
is not installed; `legacy` uses `solana-test-validator`, which is the real Agave
runtime rather than an emulator. For a program that will hold other people's
tokens, testing against the thing that actually runs it is worth the slower start.

```bash
PROPTEST_CASES=20000 cargo test --lib simulation
```

The property tests generate arbitrary stake, claim and unstake sequences and
check, after every step, that:

```
total ever paid out  <=  rewards_emitted  <=  reward_total
```

**This has already earned its place.** On its first run it found a real bug: the
original code stored `reward_debt` already divided, so a claim computed
`floor(a) - floor(b)`, which can exceed `floor(a - b)` by one base unit. One unit
per claim, paid from a pot that is a hard ceiling. The shrunk counterexample was
two claims in a row. The fix is to keep the debt scaled so the division happens
exactly once, on the difference. See `math::gross_scaled`.

That is the failure the design document lists as "the final claimant finding the
vault short from accumulated rounding", and it was in the code within an hour of
the code existing.

## Layout is a contract with the frontend

The pool list and the "my stakes" lookup are both `getProgramAccounts` calls, and
on mainnet those only work if the request can be narrowed. Measured against a
live staking program with 64,360 accounts, an unfiltered scan is 14 MB even with
`dataSlice` set to zero.

So three things must stay true, and `state.rs` has tests that fail if they stop
being true:

- `StakePool` (340 bytes) and `StakeEntry` (210 bytes) are fixed size and differ,
  so a `dataSize` filter separates them.
- `StakeEntry.owner` is at byte 8, so `memcmp` finds a wallet's positions.
- Every field a pool row renders is inside the first 184 bytes, so `dataSlice`
  fetches one row and nothing more.

Adding a `Vec`, a `String`, or a directory field past byte 184 breaks the list on
mainnet, not in a test.

## State of the work

Done:

- All five instructions, two accounts, thirteen errors
- Builds to SBF; the IDL generates
- 24 unit tests, including 60,000 generated property-test cases
- 23 integration tests on a real validator

Every "must be impossible" row in the threat model has a test that proves it
fails: unstaking early, claiming another wallet's entry, unstaking another
wallet's principal, claiming against a closed entry, re-initialising over a live
pool, a lock outside the range, a lock outlasting the pool, a zero stake, a
multiplier outside the cap, an empty pot, paying the fee to any account but the
treasury, closing a pool that still holds stakes, closing one before it has
ended, closing one as somebody other than the creator, and the donation attack.

Not done, and needed before this goes anywhere near mainnet:

- A devnet deployment and the `verify-staking.devnet.ts` harness in the frontend
  repo, plus the account-offset fixture it pins
- The frontend itself: `lib/staking.ts`, the creator page, the staker page
- An audit
- A verifiable build (`anchor build --verifiable`) published on chain
- Upgrade authority moved to a Squads multisig with a timelock

## The program keypair is a secret

It lives at `target/deploy/deploy_a_token_staking-keypair.json`, is gitignored,
and must be backed up somewhere that is not this repository. Whoever holds it can
deploy arbitrary code to our program address. Losing it means the program can
never be upgraded; leaking it means somebody else can upgrade it.
