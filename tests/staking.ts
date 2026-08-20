import { expect } from "chai";
import {
  anchor,
  BN,
  Keypair,
  LAMPORTS_PER_SOL,
  POOL_CREATION_FEE,
  PublicKey,
  STAKE_FEE,
  SystemProgram,
  TOKEN_PROGRAM_ID,
  TREASURY,
  WEIGHT_SCALE,
  entryPda,
  expectError,
  fundedActor,
  makeMint,
  poolPda,
  sleep,
  tokenBalance,
  vaultPdas,
  type Actor,
} from "./helpers";
import { transfer } from "@solana/spl-token";

/**
 * Integration tests against a real validator.
 *
 * The unit tests in `math.rs` prove the arithmetic. These prove the program:
 * that the guards actually refuse, that the tokens actually move, and that the
 * fee actually lands in the treasury.
 *
 * EVERY "CAN NEVER" ROW FROM THE THREAT MODEL GETS A TEST HERE. A test that
 * proves an attack fails is worth more than one that proves the happy path
 * works, because the happy path is the part somebody would notice.
 *
 * Pools are deliberately short-lived (a minute) and locks are seconds, so the
 * time-dependent paths can be exercised without waiting. Each test builds its
 * own pool under a fresh creator, so nothing depends on the order they run in.
 */

const DECIMALS_STAKE = 9;
const DECIMALS_REWARD = 6;
const ONE_STAKE_TOKEN = 10 ** DECIMALS_STAKE;
const ONE_REWARD_TOKEN = 10 ** DECIMALS_REWARD;

describe("staking", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.deployATokenStaking as anchor.Program;
  const payer = (provider.wallet as any).payer as Keypair;

  let stakeMint: PublicKey;
  let rewardMint: PublicKey;

  before(async () => {
    // Different decimals on purpose. A program that quietly assumes the stake
    // and reward tokens share a scale passes with equal decimals and is wrong
    // for everybody else.
    stakeMint = await makeMint(provider, payer, DECIMALS_STAKE);
    rewardMint = await makeMint(provider, payer, DECIMALS_REWARD);
  });

  interface Pool {
    creator: Actor;
    pool: PublicKey;
    stakeVault: PublicKey;
    rewardVault: PublicKey;
    rewardAmount: number;
  }

  async function createPool(opts?: {
    poolDuration?: number;
    minDuration?: number;
    maxDuration?: number;
    maxWeight?: BN;
    rewardAmount?: number;
  }): Promise<Pool> {
    const rewardAmount = opts?.rewardAmount ?? 1_000 * ONE_REWARD_TOKEN;

    const creator = await fundedActor(provider, stakeMint, rewardMint, 3);
    await import("@solana/spl-token").then((spl) =>
      spl.mintTo(
        provider.connection,
        payer,
        rewardMint,
        creator.rewardAccount,
        payer,
        rewardAmount,
      ),
    );

    const pool = poolPda(program.programId, creator.keypair.publicKey, stakeMint, 0);
    const { stakeVault, rewardVault } = vaultPdas(program.programId, pool);

    await program.methods
      .initializePool(0, {
        poolDuration: new BN(opts?.poolDuration ?? 60),
        minDuration: new BN(opts?.minDuration ?? 2),
        maxDuration: new BN(opts?.maxDuration ?? 20),
        maxWeight: opts?.maxWeight ?? WEIGHT_SCALE.muln(3),
        rewardAmount: new BN(rewardAmount),
      })
      .accounts({
        creator: creator.keypair.publicKey,
        stakeMint,
        rewardMint,
        pool,
        stakeVault,
        rewardVault,
        creatorRewardAccount: creator.rewardAccount,
        treasury: TREASURY,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator.keypair])
      .rpc();

    return { creator, pool, stakeVault, rewardVault, rewardAmount };
  }

  async function makeStaker(amount: number): Promise<Actor> {
    return fundedActor(provider, stakeMint, rewardMint, 2, {
      amount,
      authority: payer,
    });
  }

  async function stake(
    p: Pool,
    staker: Actor,
    amount: number,
    duration: number,
    nonce = 0,
  ): Promise<PublicKey> {
    const entry = entryPda(program.programId, p.pool, staker.keypair.publicKey, nonce);
    await program.methods
      .stake(nonce, new BN(amount), new BN(duration))
      .accounts({
        staker: staker.keypair.publicKey,
        pool: p.pool,
        entry,
        stakeVault: p.stakeVault,
        stakerTokenAccount: staker.stakeAccount,
        treasury: TREASURY,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([staker.keypair])
      .rpc();
    return entry;
  }

  // ---------------------------------------------------------------- happy path

  describe("the happy path", () => {
    it("creates a pool, moves the pot in, and charges the creation fee", async () => {
      const before = await provider.connection.getBalance(TREASURY);
      const p = await createPool();
      const after = await provider.connection.getBalance(TREASURY);

      expect(after - before).to.equal(
        POOL_CREATION_FEE,
        "the treasury delta must equal the advertised fee exactly",
      );

      const acct = await program.account.stakePool.fetch(p.pool);
      expect(acct.authority.toBase58()).to.equal(p.creator.keypair.publicKey.toBase58());
      expect(acct.stakeMint.toBase58()).to.equal(stakeMint.toBase58());
      expect(acct.rewardTotal.toNumber()).to.equal(p.rewardAmount);
      expect(acct.rewardsEmitted.toNumber()).to.equal(0);
      expect(acct.totalStaked.toNumber()).to.equal(0);

      // The pot is really in the vault, not merely recorded.
      expect(await tokenBalance(provider, p.rewardVault)).to.equal(
        BigInt(p.rewardAmount),
      );
    });

    it("weights a stake by its lock length and charges the stake fee", async () => {
      const p = await createPool();
      const staker = await makeStaker(10 * ONE_STAKE_TOKEN);

      const before = await provider.connection.getBalance(TREASURY);
      const entry = await stake(p, staker, 10 * ONE_STAKE_TOKEN, 20); // the longest lock
      const after = await provider.connection.getBalance(TREASURY);

      expect(after - before).to.equal(STAKE_FEE);

      const e = await program.account.stakeEntry.fetch(entry);
      expect(e.owner.toBase58()).to.equal(staker.keypair.publicKey.toBase58());
      expect(e.amount.toNumber()).to.equal(10 * ONE_STAKE_TOKEN);

      // maxDuration at maxWeight 3x, so the weight is three times the deposit.
      expect(e.weight.toString()).to.equal(String(3 * 10 * ONE_STAKE_TOKEN));

      // The principal actually moved.
      expect(await tokenBalance(provider, p.stakeVault)).to.equal(
        BigInt(10 * ONE_STAKE_TOKEN),
      );
      expect(await tokenBalance(provider, staker.stakeAccount)).to.equal(0n);
    });

    it("gives a longer lock a bigger share than a shorter one", async () => {
      const p = await createPool();
      const shortStaker = await makeStaker(ONE_STAKE_TOKEN);
      const longStaker = await makeStaker(ONE_STAKE_TOKEN);

      const shortEntry = await stake(p, shortStaker, ONE_STAKE_TOKEN, 2);
      const longEntry = await stake(p, longStaker, ONE_STAKE_TOKEN, 20);

      const s = await program.account.stakeEntry.fetch(shortEntry);
      const l = await program.account.stakeEntry.fetch(longEntry);

      expect(s.amount.toString()).to.equal(l.amount.toString());
      expect(new BN(l.weight.toString()).gt(new BN(s.weight.toString()))).to.equal(
        true,
        "the same deposit locked longer must weigh more",
      );
    });

    it("pays out only what it has emitted, and never more", async () => {
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 2);

      await sleep(5_000);

      await program.methods
        .claim()
        .accounts({
          staker: staker.keypair.publicKey,
          pool: p.pool,
          entry,
          rewardVault: p.rewardVault,
          stakerRewardAccount: staker.rewardAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([staker.keypair])
        .rpc();

      const acct = await program.account.stakePool.fetch(p.pool);
      const received = await tokenBalance(provider, staker.rewardAccount);

      expect(received > 0n).to.equal(true, "time passed, so something must have accrued");
      expect(received).to.equal(BigInt(acct.rewardsClaimed.toString()));

      // The invariant, on chain this time rather than in a unit test.
      expect(acct.rewardsClaimed.lte(acct.rewardsEmitted)).to.equal(
        true,
        `claimed ${acct.rewardsClaimed} exceeds emitted ${acct.rewardsEmitted}`,
      );
      expect(acct.rewardsEmitted.lte(acct.rewardTotal)).to.equal(true);
    });

    it("a second claim with no time in between pays nothing", async () => {
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 2);
      await sleep(4_000);

      const claim = () =>
        program.methods
          .claim()
          .accounts({
            staker: staker.keypair.publicKey,
            pool: p.pool,
            entry,
            rewardVault: p.rewardVault,
            stakerRewardAccount: staker.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([staker.keypair])
          .rpc();

      await claim();
      const afterFirst = await tokenBalance(provider, staker.rewardAccount);
      await claim();
      const afterSecond = await tokenBalance(provider, staker.rewardAccount);

      // Not zero-difference exactly: a second or two may pass between the two
      // transactions and that time is genuinely earned. What must hold is that
      // the second claim does not re-pay the first one's rewards.
      expect(afterSecond - afterFirst < afterFirst).to.equal(
        true,
        "the second claim paid out a whole period's rewards again",
      );
    });

    it("returns principal, remaining rewards and the entry rent on unstake", async () => {
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 2);

      await sleep(4_000);

      const solBefore = await provider.connection.getBalance(staker.keypair.publicKey);

      await program.methods
        .unstake()
        .accounts({
          staker: staker.keypair.publicKey,
          pool: p.pool,
          entry,
          stakeVault: p.stakeVault,
          rewardVault: p.rewardVault,
          stakerTokenAccount: staker.stakeAccount,
          stakerRewardAccount: staker.rewardAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([staker.keypair])
        .rpc();

      expect(await tokenBalance(provider, staker.stakeAccount)).to.equal(
        BigInt(ONE_STAKE_TOKEN),
        "the principal must come back in full",
      );
      expect((await tokenBalance(provider, staker.rewardAccount)) > 0n).to.equal(true);

      // The entry is gone and its rent came back, so the staker is up on SOL
      // despite paying a transaction fee.
      expect(await program.account.stakeEntry.fetchNullable(entry)).to.equal(null);
      const solAfter = await provider.connection.getBalance(staker.keypair.publicKey);
      expect(solAfter).to.be.greaterThan(
        solBefore,
        "the entry rent should have been returned, net of the transaction fee",
      );

      const acct = await program.account.stakePool.fetch(p.pool);
      expect(acct.totalStaked.toNumber()).to.equal(0);
      expect(acct.totalWeighted.toString()).to.equal("0");
    });

    it("returns the unearned residue to the creator when the pool closes", async () => {
      const p = await createPool({ poolDuration: 12, minDuration: 2, maxDuration: 4 });
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 2);

      await sleep(5_000);
      await program.methods
        .unstake()
        .accounts({
          staker: staker.keypair.publicKey,
          pool: p.pool,
          entry,
          stakeVault: p.stakeVault,
          rewardVault: p.rewardVault,
          stakerTokenAccount: staker.stakeAccount,
          stakerRewardAccount: staker.rewardAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([staker.keypair])
        .rpc();

      // Wait out the rest of the pool.
      await sleep(9_000);

      const creatorBefore = await tokenBalance(provider, p.creator.rewardAccount);
      const vaultBefore = await tokenBalance(provider, p.rewardVault);

      await program.methods
        .closePool()
        .accounts({
          creator: p.creator.keypair.publicKey,
          pool: p.pool,
          stakeVault: p.stakeVault,
          rewardVault: p.rewardVault,
          creatorRewardAccount: p.creator.rewardAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([p.creator.keypair])
        .rpc();

      const creatorAfter = await tokenBalance(provider, p.creator.rewardAccount);
      expect(creatorAfter - creatorBefore).to.equal(
        vaultBefore,
        "everything left in the vault belongs to the creator",
      );

      // Pool and both vaults are gone.
      expect(await program.account.stakePool.fetchNullable(p.pool)).to.equal(null);
      expect(await provider.connection.getAccountInfo(p.rewardVault)).to.equal(null);
      expect(await provider.connection.getAccountInfo(p.stakeVault)).to.equal(null);
    });
  });

  // ------------------------------------------------------------------ refusals

  describe("what must be impossible", () => {
    it("refuses to unstake before the lock ends", async () => {
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 20);

      await expectError(
        program.methods
          .unstake()
          .accounts({
            staker: staker.keypair.publicKey,
            pool: p.pool,
            entry,
            stakeVault: p.stakeVault,
            rewardVault: p.rewardVault,
            stakerTokenAccount: staker.stakeAccount,
            stakerRewardAccount: staker.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([staker.keypair])
          .rpc(),
        "StillLocked",
      );
    });

    it("refuses to let one wallet claim another wallet's entry", async () => {
      const p = await createPool();
      const victim = await makeStaker(ONE_STAKE_TOKEN);
      const attacker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, victim, ONE_STAKE_TOKEN, 2);

      await sleep(3_000);

      await expectError(
        program.methods
          .claim()
          .accounts({
            staker: attacker.keypair.publicKey,
            pool: p.pool,
            entry, // the victim's
            rewardVault: p.rewardVault,
            stakerRewardAccount: attacker.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([attacker.keypair])
          .rpc(),
        "VaultMintMismatch",
      );

      expect(await tokenBalance(provider, attacker.rewardAccount)).to.equal(0n);
    });

    it("refuses to let one wallet unstake another wallet's principal", async () => {
      const p = await createPool();
      const victim = await makeStaker(ONE_STAKE_TOKEN);
      const attacker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, victim, ONE_STAKE_TOKEN, 2);

      await sleep(3_000);

      await expectError(
        program.methods
          .unstake()
          .accounts({
            staker: attacker.keypair.publicKey,
            pool: p.pool,
            entry,
            stakeVault: p.stakeVault,
            rewardVault: p.rewardVault,
            stakerTokenAccount: attacker.stakeAccount,
            stakerRewardAccount: attacker.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([attacker.keypair])
          .rpc(),
        "VaultMintMismatch",
      );
    });

    it("refuses to claim against an entry that has already been unstaked", async () => {
      // `close = staker` zeroes the discriminator, which is what stops a closed
      // entry being revived and drained a second time.
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 2);

      await sleep(3_000);
      await program.methods
        .unstake()
        .accounts({
          staker: staker.keypair.publicKey,
          pool: p.pool,
          entry,
          stakeVault: p.stakeVault,
          rewardVault: p.rewardVault,
          stakerTokenAccount: staker.stakeAccount,
          stakerRewardAccount: staker.rewardAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([staker.keypair])
        .rpc();

      await expectError(
        program.methods
          .claim()
          .accounts({
            staker: staker.keypair.publicKey,
            pool: p.pool,
            entry,
            rewardVault: p.rewardVault,
            stakerRewardAccount: staker.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([staker.keypair])
          .rpc(),
        "AccountNotInitialized",
      );
    });

    it("refuses to re-initialise over a pool that already exists", async () => {
      // The pool PDA is seeded on creator + mint + nonce, and `init` rather than
      // `init_if_needed`. Without that, a creator could reset a live pool's terms
      // out from under everybody staked in it.
      const p = await createPool();

      await expectError(
        program.methods
          .initializePool(0, {
            poolDuration: new BN(60),
            minDuration: new BN(2),
            maxDuration: new BN(20),
            maxWeight: WEIGHT_SCALE,
            rewardAmount: new BN(1),
          })
          .accounts({
            creator: p.creator.keypair.publicKey,
            stakeMint,
            rewardMint,
            pool: p.pool,
            stakeVault: p.stakeVault,
            rewardVault: p.rewardVault,
            creatorRewardAccount: p.creator.rewardAccount,
            treasury: TREASURY,
            tokenProgram: TOKEN_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([p.creator.keypair])
          .rpc(),
        "already in use",
      );
    });

    it("refuses a lock shorter or longer than the pool allows", async () => {
      const p = await createPool({ minDuration: 5, maxDuration: 10 });
      const staker = await makeStaker(2 * ONE_STAKE_TOKEN);

      await expectError(
        stake(p, staker, ONE_STAKE_TOKEN, 4),
        "DurationOutOfRange",
      );
      await expectError(
        stake(p, staker, ONE_STAKE_TOKEN, 11, 1),
        "DurationOutOfRange",
      );
    });

    it("refuses a lock that would outlast the pool", async () => {
      // A 20 second pool whose longest lock is 20 seconds: staking late means
      // the lock cannot finish before the pool does.
      const p = await createPool({ poolDuration: 20, minDuration: 2, maxDuration: 20 });
      const staker = await makeStaker(ONE_STAKE_TOKEN);

      await sleep(6_000);

      await expectError(stake(p, staker, ONE_STAKE_TOKEN, 20), "LockOutlastsPool");
    });

    it("refuses a zero stake", async () => {
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      await expectError(stake(p, staker, 0, 2), "ZeroAmount");
    });

    it("refuses a multiplier above the cap", async () => {
      await expectError(
        createPool({ maxWeight: WEIGHT_SCALE.muln(11) }),
        "InvalidMaxWeight",
      );
    });

    it("refuses a multiplier below 1.0x", async () => {
      await expectError(
        createPool({ maxWeight: WEIGHT_SCALE.divn(2) }),
        "InvalidMaxWeight",
      );
    });

    it("refuses an empty reward pot", async () => {
      await expectError(createPool({ rewardAmount: 0 }), "EmptyRewardPot");
    });

    it("refuses to pay the fee anywhere but the treasury", async () => {
      // The whole revenue model rests on this constraint. If a caller can
      // substitute their own account, the fee is optional.
      const creator = await fundedActor(provider, stakeMint, rewardMint, 3);
      await import("@solana/spl-token").then((spl) =>
        spl.mintTo(
          provider.connection,
          payer,
          rewardMint,
          creator.rewardAccount,
          payer,
          1_000,
        ),
      );

      const pool = poolPda(program.programId, creator.keypair.publicKey, stakeMint, 0);
      const { stakeVault, rewardVault } = vaultPdas(program.programId, pool);
      const impostor = Keypair.generate().publicKey;

      await expectError(
        program.methods
          .initializePool(0, {
            poolDuration: new BN(60),
            minDuration: new BN(2),
            maxDuration: new BN(20),
            maxWeight: WEIGHT_SCALE,
            rewardAmount: new BN(1_000),
          })
          .accounts({
            creator: creator.keypair.publicKey,
            stakeMint,
            rewardMint,
            pool,
            stakeVault,
            rewardVault,
            creatorRewardAccount: creator.rewardAccount,
            treasury: impostor,
            tokenProgram: TOKEN_PROGRAM_ID,
            systemProgram: SystemProgram.programId,
          })
          .signers([creator.keypair])
          .rpc(),
        "ConstraintAddress",
      );
    });

    it("refuses to close a pool that still holds stakes", async () => {
      const p = await createPool({ poolDuration: 10, minDuration: 2, maxDuration: 4 });
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      await stake(p, staker, ONE_STAKE_TOKEN, 2);

      await sleep(11_000); // the pool has ended, but the stake is still in it

      await expectError(
        program.methods
          .closePool()
          .accounts({
            creator: p.creator.keypair.publicKey,
            pool: p.pool,
            stakeVault: p.stakeVault,
            rewardVault: p.rewardVault,
            creatorRewardAccount: p.creator.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([p.creator.keypair])
          .rpc(),
        "PoolNotEmpty",
      );
    });

    it("refuses to close a pool before it has ended", async () => {
      const p = await createPool();

      await expectError(
        program.methods
          .closePool()
          .accounts({
            creator: p.creator.keypair.publicKey,
            pool: p.pool,
            stakeVault: p.stakeVault,
            rewardVault: p.rewardVault,
            creatorRewardAccount: p.creator.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([p.creator.keypair])
          .rpc(),
        "PoolNotEnded",
      );
    });

    it("refuses to let anyone but the creator close a pool", async () => {
      const p = await createPool({ poolDuration: 8, minDuration: 2, maxDuration: 4 });
      const stranger = await makeStaker(ONE_STAKE_TOKEN);
      await sleep(9_000);

      await expectError(
        program.methods
          .closePool()
          .accounts({
            creator: stranger.keypair.publicKey,
            pool: p.pool,
            stakeVault: p.stakeVault,
            rewardVault: p.rewardVault,
            creatorRewardAccount: stranger.rewardAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([stranger.keypair])
          .rpc(),
        "ConstraintAddress",
      );
    });
  });

  // ----------------------------------------------------------- donation attack

  describe("the donation attack", () => {
    it("ignores tokens transferred straight into the reward vault", async () => {
      // Anyone can send tokens to a program-owned vault. If any calculation
      // read the vault's balance, this would inflate every share for the price
      // of a transfer. It is the classic MasterChef failure, and the reason
      // `close_pool` is the only place in the program that reads a balance.
      const p = await createPool();
      const staker = await makeStaker(ONE_STAKE_TOKEN);
      const entry = await stake(p, staker, ONE_STAKE_TOKEN, 2);

      const donor = await fundedActor(provider, stakeMint, rewardMint, 2);
      const donation = 500_000 * ONE_REWARD_TOKEN; // 500x the real pot
      await import("@solana/spl-token").then((spl) =>
        spl.mintTo(
          provider.connection,
          payer,
          rewardMint,
          donor.rewardAccount,
          payer,
          donation,
        ),
      );
      await transfer(
        provider.connection,
        donor.keypair,
        donor.rewardAccount,
        p.rewardVault,
        donor.keypair,
        donation,
      );

      const afterDonation = await program.account.stakePool.fetch(p.pool);
      expect(afterDonation.rewardTotal.toNumber()).to.equal(
        p.rewardAmount,
        "a donation must not change the pot the program thinks it has",
      );

      await sleep(4_000);

      await program.methods
        .claim()
        .accounts({
          staker: staker.keypair.publicKey,
          pool: p.pool,
          entry,
          rewardVault: p.rewardVault,
          stakerRewardAccount: staker.rewardAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([staker.keypair])
        .rpc();

      const received = await tokenBalance(provider, staker.rewardAccount);
      const acct = await program.account.stakePool.fetch(p.pool);

      expect(received < BigInt(p.rewardAmount)).to.equal(
        true,
        "the payout tracked the donated balance instead of the recorded pot",
      );
      expect(acct.rewardsEmitted.lte(acct.rewardTotal)).to.equal(true);
      expect(received).to.equal(BigInt(acct.rewardsClaimed.toString()));
    });
  });
});
