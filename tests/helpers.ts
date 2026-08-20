import * as anchor from "@anchor-lang/core";
import { BN, Program } from "@anchor-lang/core";
import {
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
} from "@solana/web3.js";
import {
  createAssociatedTokenAccount,
  createMint,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";

/// Where both fees go. The same constant the program compiles in, copied from
/// `lib/treasury.ts` in the deploy-a-token repo and nowhere else.
export const TREASURY = new PublicKey(
  "HWi3LMkqSP92X3uHqsVqvBHsRwf3kjD9C6Sr6sS5z78X",
);

export const WEIGHT_SCALE = new BN(1_000_000_000);
export const POOL_CREATION_FEE = 500_000_000;
export const STAKE_FEE = 5_000_000;

export const POOL_SEED = Buffer.from("pool");
export const ENTRY_SEED = Buffer.from("entry");
export const STAKE_VAULT_SEED = Buffer.from("stake_vault");
export const REWARD_VAULT_SEED = Buffer.from("reward_vault");

export function poolPda(
  programId: PublicKey,
  creator: PublicKey,
  stakeMint: PublicKey,
  nonce: number,
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [POOL_SEED, creator.toBuffer(), stakeMint.toBuffer(), Buffer.from([nonce])],
    programId,
  )[0];
}

export function entryPda(
  programId: PublicKey,
  pool: PublicKey,
  owner: PublicKey,
  nonce: number,
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [ENTRY_SEED, pool.toBuffer(), owner.toBuffer(), Buffer.from([nonce])],
    programId,
  )[0];
}

export function vaultPdas(programId: PublicKey, pool: PublicKey) {
  const stakeVault = PublicKey.findProgramAddressSync(
    [STAKE_VAULT_SEED, pool.toBuffer()],
    programId,
  )[0];
  const rewardVault = PublicKey.findProgramAddressSync(
    [REWARD_VAULT_SEED, pool.toBuffer()],
    programId,
  )[0];
  return { stakeVault, rewardVault };
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/// A funded wallet with token accounts for both mints, ready to stake or create.
export interface Actor {
  keypair: Keypair;
  stakeAccount: PublicKey;
  rewardAccount: PublicKey;
}

export async function fundedActor(
  provider: anchor.AnchorProvider,
  stakeMint: PublicKey,
  rewardMint: PublicKey,
  sol: number,
  stakeTokens?: { amount: number; authority: Keypair },
): Promise<Actor> {
  const keypair = Keypair.generate();

  const sig = await provider.connection.requestAirdrop(
    keypair.publicKey,
    sol * LAMPORTS_PER_SOL,
  );
  const bh = await provider.connection.getLatestBlockhash();
  await provider.connection.confirmTransaction({ signature: sig, ...bh });

  const stakeAccount = await createAssociatedTokenAccount(
    provider.connection,
    keypair,
    stakeMint,
    keypair.publicKey,
  );
  const rewardAccount = await createAssociatedTokenAccount(
    provider.connection,
    keypair,
    rewardMint,
    keypair.publicKey,
  );

  if (stakeTokens) {
    await mintTo(
      provider.connection,
      stakeTokens.authority,
      stakeMint,
      stakeAccount,
      stakeTokens.authority,
      stakeTokens.amount,
    );
  }

  return { keypair, stakeAccount, rewardAccount };
}

export async function makeMint(
  provider: anchor.AnchorProvider,
  payer: Keypair,
  decimals: number,
): Promise<PublicKey> {
  return createMint(
    provider.connection,
    payer,
    payer.publicKey,
    null,
    decimals,
  );
}

export async function tokenBalance(
  provider: anchor.AnchorProvider,
  account: PublicKey,
): Promise<bigint> {
  const info = await provider.connection.getTokenAccountBalance(account);
  return BigInt(info.value.amount);
}

/// Assert that a transaction fails, and that it fails for the RIGHT reason.
///
/// Checking only that it threw is how a test passes because the accounts were
/// wrong rather than because the guard worked.
export async function expectError(
  promise: Promise<unknown>,
  expected: string,
): Promise<void> {
  try {
    await promise;
  } catch (err: any) {
    const haystack = [
      err?.error?.errorCode?.code,
      err?.error?.errorMessage,
      err?.message,
      JSON.stringify(err?.logs ?? []),
    ]
      .filter(Boolean)
      .join(" | ");

    if (!haystack.includes(expected)) {
      throw new Error(
        `expected the transaction to fail with "${expected}", got: ${haystack}`,
      );
    }
    return;
  }
  throw new Error(`expected the transaction to fail with "${expected}", but it succeeded`);
}

export { anchor, BN, Program, SystemProgram, TOKEN_PROGRAM_ID, Keypair, PublicKey, LAMPORTS_PER_SOL };
