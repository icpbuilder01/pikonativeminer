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
