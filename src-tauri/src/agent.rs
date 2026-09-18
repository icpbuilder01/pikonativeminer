// Thin wrapper around ic-agent for the handful of calls this app needs:
// reading the current mining work, submitting a found proof, and the ICP
// ledger's ICRC-2 approve/balance/allowance -- the same four operations
// the browser mining site performs, just from a native Rust agent instead
// of agent-js in a Web Worker.
use crate::types::*;
use anyhow::{bail, Context, Result};
use candid::{Decode, Encode, Nat, Principal};
use ic_agent::identity::BasicIdentity;
use ic_agent::Agent;

pub const MOTHER_CANISTER_ID: &str = "45mjf-rqaaa-aaaaj-qsedq-cai";
// PikoPool (~/pikopool, built 2026-09-18), deployed to mainnet, its
// motherId/pikoLedgerId/icpLedgerId locked to the real mainnet values and
// already funded/approved to pay mother's mining fee.
pub const POOL_CANISTER_ID: &str = "feqm6-7aaaa-aaaap-quzsa-cai";
pub const ICP_LEDGER_CANISTER_ID: &str = "ryjl3-tyaaa-aaaaa-aaaba-cai";
pub const PIKO_LEDGER_CANISTER_ID: &str = "56aad-fiaaa-aaaaj-qsefa-cai";
// DFINITY's recommended mainnet endpoint for a raw agent (not going through
// a browser's own canister-subdomain routing, which only static-asset
// canisters serve anyway -- mother/the ledger are plain canisters).
const MAINNET_URL: &str = "https://icp-api.io";

pub fn build_agent(identity: BasicIdentity) -> Result<Agent> {
    // No `fetch_root_key()` call here on purpose: that call is only correct
    // (and only needed) against a local replica, whose root key isn't known
    // in advance. Skipping it on mainnet is what keeps the agent verifying
    // every response against the IC's real, hardcoded root key -- calling
    // it here would be a real trust downgrade, not a convenience.
    let agent = Agent::builder()
        .with_url(MAINNET_URL)
        .with_identity(identity)
        .build()
        .context("failed to build the IC agent")?;
    Ok(agent)
}

pub async fn get_stats(agent: &Agent) -> Result<Stats> {
    let canister_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getStats")
        .with_arg(Encode!()?)
        .call()
        .await
        .context("getStats call failed")?;
    let stats = Decode!(response.as_slice(), Stats)?;
    Ok(stats)
}

pub async fn get_recent_blocks(agent: &Agent) -> Result<Vec<Block>> {
    let canister_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getRecentBlocks")
        .with_arg(Encode!()?)
        .call()
        .await
        .context("getRecentBlocks call failed")?;
    let blocks = Decode!(response.as_slice(), Vec<Block>)?;
    Ok(blocks)
}

pub async fn get_miner_stats(agent: &Agent, owner: Principal) -> Result<LeaderboardEntry> {
    let canister_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getMinerStats")
        .with_arg(Encode!(&owner)?)
        .call()
        .await
        .context("getMinerStats call failed")?;
    let entry = Decode!(response.as_slice(), LeaderboardEntry)?;
    Ok(entry)
}

pub async fn get_work(agent: &Agent) -> Result<Work> {
    let canister_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getWork")
        .with_arg(Encode!()?)
        .call()
        .await
        .context("getWork call failed")?;
    let work = Decode!(response.as_slice(), Work)?;
    Ok(work)
}

pub async fn submit_proof(agent: &Agent, nonce: u64) -> Result<SubmitResult> {
    let canister_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let response = agent
        .update(&canister_id, "submitProof")
        .with_arg(Encode!(&Nat::from(nonce))?)
        .call_and_wait()
        .await
        .context("submitProof call failed")?;
    let result = Decode!(response.as_slice(), SubmitResult)?;
    Ok(result)
}

pub async fn get_pool_config(agent: &Agent) -> Result<PoolConfig> {
    let canister_id = Principal::from_text(POOL_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getPoolConfig")
        .with_arg(Encode!()?)
        .call()
        .await
        .context("getPoolConfig call failed")?;
    let config = Decode!(response.as_slice(), PoolConfig)?;
    Ok(config)
}

pub async fn get_pool_stats(agent: &Agent) -> Result<PoolStats> {
    let canister_id = Principal::from_text(POOL_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getPoolStats")
        .with_arg(Encode!()?)
        .call()
        .await
        .context("getPoolStats call failed")?;
    let stats = Decode!(response.as_slice(), PoolStats)?;
    Ok(stats)
}

/// Reports a candidate nonce to the pool instead of directly to `mother` --
/// used for every qualifying find while pool mode is on, whether it only
/// clears the pool's own (easier) share target or clears the real network
/// difficulty too. Solo-submitting a real winning nonce straight to
/// `mother` while pool mode is on would let that one lucky thread keep
/// 100% of the reward instead of sharing it, defeating the entire point.
pub async fn submit_share(agent: &Agent, height: u64, nonce: u64) -> Result<ShareResult> {
    let canister_id = Principal::from_text(POOL_CANISTER_ID)?;
    let response = agent
        .update(&canister_id, "submitShare")
        .with_arg(Encode!(&Nat::from(height), &Nat::from(nonce))?)
        .call_and_wait()
        .await
        .context("submitShare call failed")?;
    let result = Decode!(response.as_slice(), ShareResult)?;
    Ok(result)
}

/// Approves `amount` e8s of ICP for pikopool to pull as this miner's
/// proportional share of mother's mining fee whenever the pool wins a
/// block -- a separate approval from `approve_icp` (which is scoped to
/// mother, for solo mining), since pikopool is a different spender.
pub async fn approve_icp_for_pool(agent: &Agent, amount: Nat) -> Result<Nat> {
    let canister_id = Principal::from_text(ICP_LEDGER_CANISTER_ID)?;
    let pool_id = Principal::from_text(POOL_CANISTER_ID)?;
    let args = ApproveArgs {
        fee: None,
        memo: None,
        from_subaccount: None,
        created_at_time: None,
        amount,
        expected_allowance: None,
        expires_at: None,
        spender: Account {
            owner: pool_id,
            subaccount: None,
        },
    };
    let response = agent
        .update(&canister_id, "icrc2_approve")
        .with_arg(Encode!(&args)?)
        .call_and_wait()
        .await
        .context("icrc2_approve (pikopool) call failed")?;
    let result = Decode!(response.as_slice(), ApproveResult)?;
    match result {
        ApproveResult::Ok(block_index) => Ok(block_index),
        ApproveResult::Err(e) => bail!("approval rejected: {:?}", e),
    }
}

/// Current ICP allowance this miner has granted pikopool (distinct from
/// `icp_allowance`, which checks mother's own allowance).
pub async fn icp_allowance_for_pool(agent: &Agent, owner: Principal) -> Result<Nat> {
    let canister_id = Principal::from_text(ICP_LEDGER_CANISTER_ID)?;
    let pool_id = Principal::from_text(POOL_CANISTER_ID)?;
    let args = AllowanceArgs {
        account: Account {
            owner,
            subaccount: None,
        },
        spender: Account {
            owner: pool_id,
            subaccount: None,
        },
    };
    let response = agent
        .query(&canister_id, "icrc2_allowance")
        .with_arg(Encode!(&args)?)
        .call()
        .await
        .context("icrc2_allowance (pikopool) call failed")?;
    let allowance = Decode!(response.as_slice(), Allowance)?;
    Ok(allowance.allowance)
}

pub async fn get_my_pool_share(agent: &Agent) -> Result<MyShare> {
    let canister_id = Principal::from_text(POOL_CANISTER_ID)?;
    let response = agent
        .query(&canister_id, "getMyShare")
        .with_arg(Encode!()?)
        .call()
        .await
        .context("getMyShare call failed")?;
    let share = Decode!(response.as_slice(), MyShare)?;
    Ok(share)
}

pub async fn claim_pool_reward(agent: &Agent) -> Result<TransferResult> {
    let canister_id = Principal::from_text(POOL_CANISTER_ID)?;
    let response = agent
        .update(&canister_id, "claimPoolReward")
        .with_arg(Encode!()?)
        .call_and_wait()
        .await
        .context("claimPoolReward call failed")?;
    let result = Decode!(response.as_slice(), TransferResult)?;
    Ok(result)
}

/// Works for any ICRC-1 ledger -- PIKO and ICP share the identical
/// interface (the PIKO ledger is the same unmodified DFINITY ledger wasm).
pub async fn ledger_balance(agent: &Agent, ledger_id: &str, owner: Principal) -> Result<Nat> {
    let canister_id = Principal::from_text(ledger_id)?;
    let account = Account {
        owner,
        subaccount: None,
    };
    let response = agent
        .query(&canister_id, "icrc1_balance_of")
        .with_arg(Encode!(&account)?)
        .call()
        .await
        .context("icrc1_balance_of call failed")?;
    let balance = Decode!(response.as_slice(), Nat)?;
    Ok(balance)
}

/// Sends `amount` of whichever token `ledger_id` is, to `to`, from this
/// agent's own identity. Returns the resulting ledger block index.
pub async fn transfer(agent: &Agent, ledger_id: &str, to: Principal, amount: Nat) -> Result<Nat> {
    let canister_id = Principal::from_text(ledger_id)?;
    let args = TransferArg {
        from_subaccount: None,
        to: Account {
            owner: to,
            subaccount: None,
        },
        amount,
        fee: None,
        memo: None,
        created_at_time: None,
    };
    let response = agent
        .update(&canister_id, "icrc1_transfer")
        .with_arg(Encode!(&args)?)
        .call_and_wait()
        .await
        .context("icrc1_transfer call failed")?;
    let result = Decode!(response.as_slice(), TransferResult)?;
    match result {
        TransferResult::Ok(block_index) => Ok(block_index),
        TransferResult::Err(e) => bail!("transfer rejected: {:?}", e),
    }
}

pub async fn icp_allowance(agent: &Agent, owner: Principal) -> Result<Nat> {
    let canister_id = Principal::from_text(ICP_LEDGER_CANISTER_ID)?;
    let mother_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let args = AllowanceArgs {
        account: Account {
            owner,
            subaccount: None,
        },
        spender: Account {
            owner: mother_id,
            subaccount: None,
        },
    };
    let response = agent
        .query(&canister_id, "icrc2_allowance")
        .with_arg(Encode!(&args)?)
        .call()
        .await
        .context("icrc2_allowance call failed")?;
    let allowance = Decode!(response.as_slice(), Allowance)?;
    Ok(allowance.allowance)
}

/// Approves `amount` e8s of ICP for `mother` to pull as mining fees.
pub async fn approve_icp(agent: &Agent, amount: Nat) -> Result<Nat> {
    let canister_id = Principal::from_text(ICP_LEDGER_CANISTER_ID)?;
    let mother_id = Principal::from_text(MOTHER_CANISTER_ID)?;
    let args = ApproveArgs {
        fee: None,
        memo: None,
        from_subaccount: None,
        created_at_time: None,
        amount,
        expected_allowance: None,
        expires_at: None,
        spender: Account {
            owner: mother_id,
            subaccount: None,
        },
    };
    let response = agent
        .update(&canister_id, "icrc2_approve")
        .with_arg(Encode!(&args)?)
        .call_and_wait()
        .await
        .context("icrc2_approve call failed")?;
    let result = Decode!(response.as_slice(), ApproveResult)?;
    match result {
        ApproveResult::Ok(block_index) => Ok(block_index),
        ApproveResult::Err(e) => bail!("approval rejected: {:?}", e),
    }
}
