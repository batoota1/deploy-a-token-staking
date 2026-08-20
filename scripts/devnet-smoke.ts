/**
 * One real pool, start to finish, against the DEPLOYED devnet program.
 *
 * Not a substitute for `anchor test`: that suite proves every refusal against a
 * local validator. This proves the thing a local validator cannot, which is that
 * the copy actually sitting at the program id works, and that the fee really
 * lands in the real treasury.
 *
 * It is deliberately frugal. Devnet faucets are rate limited and our own
 * pool-creation fee is a compiled-in 0.5 SOL that applies on devnet too, so a
 * single run costs about 0.52 SOL. One wallet plays both creator and staker to
 * avoid funding a second.
 *
 * Every signature is printed. Paste any of them into Solscan with
 * `?cluster=devnet`: a green line here is our own screen agreeing with itself,
 * which is not evidence.
 */
import { expect } from "chai";
import * as anchor from "@anchor-lang/core";
import { BN, Program } from "@anchor-lang/core";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import {
  createAssociatedTokenAccount,
  createMint,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { readFileSync } from "fs";

// Read rather than import: Node wants an import attribute for JSON, and this
// script is run from the repo root by ts-mocha either way.
const idl = JSON.parse(
  readFileSync("target/idl/deploy_a_token_staking.json", "utf8"),
);

const TREASURY = new PublicKey("HWi3LMkqSP92X3uHqsVqvBHsRwf3kjD9C6Sr6sS5z78X");
const WEIGHT_SCALE = new BN(1_000_000_000);

const POOL_SECONDS = 40;
const MIN_LOCK = 5;
const MAX_LOCK = 20;
const STAKE_AMOUNT = 1_000_000_000; // 1 token at 9 decimals
const REWARD_POT = 1_000_000; // 1 token at 6 decimals

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const sig = (label: string, s: string) =>
  console.log(`      ${label}\n        https://solscan.io/tx/${s}?cluster=devnet`);

describe("devnet smoke: one real pool, start to finish", function () {
  this.timeout(300_000);

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program(idl as anchor.Idl, provider);
  const me = (provider.wallet as any).payer as Keypair;

  let stakeMint: PublicKey;
  let rewardMint: PublicKey;
  let myStake: PublicKey;
  let myReward: PublicKey;
  let pool: PublicKey;
  let stakeVault: PublicKey;
  let rewardVault: PublicKey;
  let entry: PublicKey;

  it("sets up two mints with different decimals", async () => {
    const before = await provider.connection.getBalance(me.publicKey);
    console.log(`      wallet ${me.publicKey.toBase58()}`);
    console.log(`      starting balance ${(before / 1e9).toFixed(6)} SOL`);

    stakeMint = await createMint(provider.connection, me, me.publicKey, null, 9);
    rewardMint = await createMint(provider.connection, me, me.publicKey, null, 6);
    myStake = await createAssociatedTokenAccount(provider.connection, me, stakeMint, me.publicKey);
    myReward = await createAssociatedTokenAccount(provider.connection, me, rewardMint, me.publicKey);

    await mintTo(provider.connection, me, stakeMint, myStake, me, STAKE_AMOUNT);
    await mintTo(provider.connection, me, rewardMint, myReward, me, REWARD_POT);

    console.log(`      stake mint  ${stakeMint.toBase58()}`);
    console.log(`      reward mint ${rewardMint.toBase58()}`);
  });

  it("creates a pool and pays the real 0.5 SOL fee to the real treasury", async () => {
    pool = PublicKey.findProgramAddressSync(
      [Buffer.from("pool"), me.publicKey.toBuffer(), stakeMint.toBuffer(), Buffer.from([0])],
      program.programId,
    )[0];
    stakeVault = PublicKey.findProgramAddressSync(
      [Buffer.from("stake_vault"), pool.toBuffer()],
      program.programId,
    )[0];
    rewardVault = PublicKey.findProgramAddressSync(
      [Buffer.from("reward_vault"), pool.toBuffer()],
      program.programId,
    )[0];

    const treasuryBefore = await provider.connection.getBalance(TREASURY);

    const s = await program.methods
      .initializePool(0, {
        poolDuration: new BN(POOL_SECONDS),
        minDuration: new BN(MIN_LOCK),
        maxDuration: new BN(MAX_LOCK),
        maxWeight: WEIGHT_SCALE.muln(2),
        rewardAmount: new BN(REWARD_POT),
      })
      .accounts({
        creator: me.publicKey,
        stakeMint,
        rewardMint,
        pool,
        stakeVault,
        rewardVault,
        creatorRewardAccount: myReward,
        treasury: TREASURY,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    sig("initialize_pool", s);

    const treasuryAfter = await provider.connection.getBalance(TREASURY);
    console.log(`      treasury delta ${(treasuryAfter - treasuryBefore) / 1e9} SOL`);
    expect(treasuryAfter - treasuryBefore).to.equal(500_000_000);

    const acct: any = await program.account.stakePool.fetch(pool);
    expect(acct.rewardTotal.toNumber()).to.equal(REWARD_POT);
    expect(acct.totalStaked.toNumber()).to.equal(0);
    console.log(`      pool ${pool.toBase58()}`);
  });

  it("stakes, weighting the deposit and paying the 0.005 SOL fee", async () => {
    entry = PublicKey.findProgramAddressSync(
      [Buffer.from("entry"), pool.toBuffer(), me.publicKey.toBuffer(), Buffer.from([0])],
      program.programId,
    )[0];

    const treasuryBefore = await provider.connection.getBalance(TREASURY);

    const s = await program.methods
      .stake(0, new BN(STAKE_AMOUNT), new BN(MAX_LOCK))
      .accounts({
        staker: me.publicKey,
        pool,
        entry,
        stakeVault,
        stakerTokenAccount: myStake,
        treasury: TREASURY,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    sig("stake", s);

    const treasuryAfter = await provider.connection.getBalance(TREASURY);
    expect(treasuryAfter - treasuryBefore).to.equal(5_000_000);

    const e: any = await program.account.stakeEntry.fetch(entry);
    // Longest lock at a 2x multiplier, so the weight is double the deposit.
    expect(e.weight.toString()).to.equal(String(2 * STAKE_AMOUNT));
    console.log(`      weight ${e.weight.toString()} for a deposit of ${STAKE_AMOUNT}`);

    const vault = await provider.connection.getTokenAccountBalance(stakeVault);
    expect(vault.value.amount).to.equal(String(STAKE_AMOUNT));
  });

  it("accrues rewards and pays out no more than it emitted", async () => {
    await sleep(10_000);

    const s = await program.methods
      .claim()
      .accounts({
        staker: me.publicKey,
        pool,
        entry,
        rewardVault,
        stakerRewardAccount: myReward,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    sig("claim", s);

    const acct: any = await program.account.stakePool.fetch(pool);
    console.log(
      `      emitted ${acct.rewardsEmitted.toString()} of ${acct.rewardTotal.toString()}, claimed ${acct.rewardsClaimed.toString()}`,
    );

    expect(acct.rewardsClaimed.toNumber()).to.be.greaterThan(0);
    expect(acct.rewardsClaimed.lte(acct.rewardsEmitted)).to.equal(true);
    expect(acct.rewardsEmitted.lte(acct.rewardTotal)).to.equal(true);
  });

  it("refuses to unstake while the lock is still running", async () => {
    try {
      await program.methods
        .unstake()
        .accounts({
          staker: me.publicKey,
          pool,
          entry,
          stakeVault,
          rewardVault,
          stakerTokenAccount: myStake,
          stakerRewardAccount: myReward,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .rpc();
      throw new Error("the deployed program allowed an early unstake");
    } catch (err: any) {
      const text = `${err?.error?.errorCode?.code} ${err?.message}`;
      expect(text).to.include("StillLocked");
      console.log("      refused with StillLocked, as it should");
    }
  });

  it("returns principal, the rest of the rewards, and the entry rent", async () => {
    await sleep(12_000); // let the 20 second lock expire

    const s = await program.methods
      .unstake()
      .accounts({
        staker: me.publicKey,
        pool,
        entry,
        stakeVault,
        rewardVault,
        stakerTokenAccount: myStake,
        stakerRewardAccount: myReward,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    sig("unstake", s);

    const back = await provider.connection.getTokenAccountBalance(myStake);
    expect(back.value.amount).to.equal(String(STAKE_AMOUNT));
    expect(await program.account.stakeEntry.fetchNullable(entry)).to.equal(null);

    const acct: any = await program.account.stakePool.fetch(pool);
    expect(acct.totalStaked.toNumber()).to.equal(0);
    console.log(`      principal returned in full, entry closed`);
  });

  it("closes the pool and returns the unearned residue", async () => {
    // Wait out whatever is left of the pool's life.
    for (;;) {
      const acct: any = await program.account.stakePool.fetch(pool);
      const now = Math.floor(Date.now() / 1000);
      if (now >= acct.endTs.toNumber()) break;
      await sleep(3_000);
    }

    const vaultBefore = await provider.connection.getTokenAccountBalance(rewardVault);
    const mineBefore = await provider.connection.getTokenAccountBalance(myReward);

    const s = await program.methods
      .closePool()
      .accounts({
        creator: me.publicKey,
        pool,
        stakeVault,
        rewardVault,
        creatorRewardAccount: myReward,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();
    sig("close_pool", s);

    const mineAfter = await provider.connection.getTokenAccountBalance(myReward);
    expect(BigInt(mineAfter.value.amount) - BigInt(mineBefore.value.amount)).to.equal(
      BigInt(vaultBefore.value.amount),
    );
    expect(await program.account.stakePool.fetchNullable(pool)).to.equal(null);
    expect(await provider.connection.getAccountInfo(rewardVault)).to.equal(null);

    const left = await provider.connection.getBalance(me.publicKey);
    console.log(`      residue ${vaultBefore.value.amount} returned, pool and vaults closed`);
    console.log(`      ending balance ${(left / 1e9).toFixed(6)} SOL`);
  });
});
