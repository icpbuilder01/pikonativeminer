// Candid types mirroring mother's public interface (mother/mother.did) and
// the ICP ledger's ICRC-2 interface (frontend/idl/icp_ledger.did) in the
// main piko-icp repo. Copied by hand rather than codegen'd, since we only
// need a handful of methods -- field names must match exactly (Candid
// records match by name, not position, so declaration order here doesn't
// need to match the .did file's).

#![allow(non_snake_case)] // field names deliberately match the .did wire format exactly, not Rust convention

use candid::{CandidType, Deserialize, Nat, Principal};
use serde_bytes::ByteBuf;

// A deliberate subset of mother's real Stats record -- Candid record
// decoding allows the receiver to declare fewer fields than the sender
// actually returns (standard record subtyping), so this only pulls what
// the dashboard needs rather than mirroring every field.
#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct Stats {
    pub height: Nat,
    pub difficultyBits: Nat,
    pub blocksUntilRetarget: Nat,
    pub retargetIntervalBlocks: Nat,
    pub lastRetargetAt: candid::Int,
    pub targetBlockTimeNanos: Nat,
    pub nextHalvingHeight: Nat,
    pub currentReward: Nat,
    pub miningFeeE8s: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct Block {
    pub hash: ByteBuf,
    pub height: Nat,
    pub miner: Principal,
    pub reward: Nat,
    pub timestamp: candid::Int,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct LeaderboardEntry {
    pub blocksFound: Nat,
    pub miner: Principal,
    pub totalReward: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct Work {
    pub difficultyBits: Nat,
    pub height: Nat,
    pub miningFeeE8s: Nat,
    pub previousHash: ByteBuf,
    pub reward: Nat,
}

// --- pikopool (mining pool) ---

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct PoolConfig {
    pub shareDifficultyBits: Nat,
    pub networkDifficultyBits: Nat,
    pub height: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct MyShare {
    pub sharesThisRound: Nat,
    pub pendingReward: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct PoolStats {
    pub blocksWon: Nat,
    pub totalSharesAllTime: Nat,
    pub totalPikoDistributed: Nat,
    pub currentRoundTotalShares: Nat,
    pub activeMinersThisRound: Nat,
    pub activeMinersNow: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct ShareOutcome {
    pub accepted: bool,
    pub isBlockWinner: bool,
    pub poolSubmitResult: Option<SubmitResult>,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum ShareError {
    Anonymous,
    BelowShareTarget,
    DuplicateNonce,
    StaleWork,
    TooSoon(TooSoonInfo),
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum ShareResult {
    Ok(ShareOutcome),
    Err(ShareError),
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct SubmitOk {
    pub hash: ByteBuf,
    pub height: Nat,
    pub reward: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct TooSoonInfo {
    pub retryAfterNanos: Nat,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum TransferFromError {
    BadBurn { min_burn_amount: Nat },
    BadFee { expected_fee: Nat },
    CreatedInFuture { ledger_time: u64 },
    Duplicate { duplicate_of: Nat },
    GenericError { error_code: Nat, message: String },
    InsufficientAllowance { allowance: Nat },
    InsufficientFunds { balance: Nat },
    TemporarilyUnavailable,
    TooOld,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum SubmitError {
    Anonymous,
    IcpFeeFailed(TransferFromError),
    InvalidProof,
    NothingToClaim,
    StaleWork,
    TooSoon(TooSoonInfo),
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum SubmitResult {
    Ok(SubmitOk),
    Err(SubmitError),
}

// --- ICP ledger (ICRC-1/ICRC-2) ---

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct Account {
    pub owner: Principal,
    pub subaccount: Option<ByteBuf>,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct ApproveArgs {
    pub fee: Option<Nat>,
    pub memo: Option<ByteBuf>,
    pub from_subaccount: Option<ByteBuf>,
    pub created_at_time: Option<u64>,
    pub amount: Nat,
    pub expected_allowance: Option<Nat>,
    pub expires_at: Option<u64>,
    pub spender: Account,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum ApproveError {
    BadFee { expected_fee: Nat },
    InsufficientFunds { balance: Nat },
    AllowanceChanged { current_allowance: Nat },
    Expired { ledger_time: u64 },
    TooOld,
    CreatedInFuture { ledger_time: u64 },
    Duplicate { duplicate_of: Nat },
    TemporarilyUnavailable,
    GenericError { error_code: Nat, message: String },
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum ApproveResult {
    Ok(Nat),
    Err(ApproveError),
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct TransferArg {
    pub from_subaccount: Option<ByteBuf>,
    pub to: Account,
    pub amount: Nat,
    pub fee: Option<Nat>,
    pub memo: Option<ByteBuf>,
    pub created_at_time: Option<u64>,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum TransferError {
    BadFee { expected_fee: Nat },
    BadBurn { min_burn_amount: Nat },
    InsufficientFunds { balance: Nat },
    TooOld,
    CreatedInFuture { ledger_time: u64 },
    TemporarilyUnavailable,
    Duplicate { duplicate_of: Nat },
    GenericError { error_code: Nat, message: String },
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub enum TransferResult {
    Ok(Nat),
    Err(TransferError),
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct AllowanceArgs {
    pub account: Account,
    pub spender: Account,
}

#[derive(CandidType, Deserialize, Debug, Clone)]
pub struct Allowance {
    pub allowance: Nat,
    pub expires_at: Option<u64>,
}
