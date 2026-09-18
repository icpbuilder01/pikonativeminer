mod agent;
mod gpu;
mod identity;
mod miner;
mod types;

use candid::{Nat, Principal};
use ic_agent::Agent;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex as AsyncMutex;

const ICP_LEDGER_FEE_E8S: u64 = 10_000; // fixed for the ICP ledger's whole history, same constant the browser site hardcodes

// Nat's own Display/to_string() inserts underscore digit-group separators
// (e.g. "250_000_000") for human-readable printing -- great for a log line,
// but JS's BigInt() can't parse that at all (throws a SyntaxError). Every
// Nat crossing the Tauri IPC boundary into the frontend needs this plain,
// underscore-free digit string instead.
fn nat_to_plain_string(n: &Nat) -> String {
    n.0.to_str_radix(10)
}

// Same formatting convention as the browser site's own formatPiko/formatIcp
// (both PIKO and ICP use 8 decimals) -- for a human-readable mining message,
// not for anything re-parsed programmatically.
fn format_piko(e8s: &Nat) -> String {
    let raw = e8s.0.to_str_radix(10);
    let padded = format!("{raw:0>9}"); // at least 1 whole digit + 8 fractional digits
    let split_at = padded.len() - 8;
    let (whole, frac) = padded.split_at(split_at);
    let frac_trimmed = frac.trim_end_matches('0');
    if frac_trimmed.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac_trimmed}")
    }
}

struct AppState {
    agent: Arc<Agent>,
    principal_text: String,
    mining_stop: Arc<AtomicBool>,
    mining_running: Arc<AsyncMutex<bool>>,
    power_percent: Arc<AtomicU32>,
    // Probed once at startup (see gpu::probe) -- cheap, but no reason to
    // redo the adapter enumeration on every single UI query.
    gpu_adapter_name: Option<String>,
    gpu_enabled: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
struct MiningProgress {
    hashrate: u64,
    #[serde(rename = "sessionAttempts")]
    session_attempts: u64,
    #[serde(rename = "sessionBlocks")]
    session_blocks: u64,
    message: String,
    #[serde(rename = "messageKind")]
    message_kind: String, // "good" | "critical" | ""
    stopped: bool,
}

#[derive(Serialize, Clone)]
struct NetworkStats {
    height: String,
    #[serde(rename = "difficultyBits")]
    difficulty_bits: u32,
    #[serde(rename = "blocksUntilRetarget")]
    blocks_until_retarget: u32,
    #[serde(rename = "retargetIntervalBlocks")]
    retarget_interval_blocks: u32,
    #[serde(rename = "lastRetargetAtNanos")]
    last_retarget_at_nanos: i64,
    #[serde(rename = "targetBlockTimeNanos")]
    target_block_time_nanos: i64,
    #[serde(rename = "nextHalvingHeight")]
    next_halving_height: String,
    #[serde(rename = "currentReward")]
    current_reward: String,
    #[serde(rename = "miningFeeE8s")]
    mining_fee_e8s: String,
    #[serde(rename = "lastBlockAtNanos")]
    last_block_at_nanos: Option<i64>,
}

#[tauri::command]
fn get_principal(state: State<AppState>) -> String {
    state.principal_text.clone()
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    if enabled {
        manager.enable().map_err(|e| e.to_string())
    } else {
        manager.disable().map_err(|e| e.to_string())
    }
}

// Closing the window only hides it (see the on_window_event handler below)
// so background mining survives an accidental close -- but that leaves no
// obvious way to actually exit short of finding the tray icon, which isn't
// always visible (stock GNOME needs an extension for it). This gives every
// platform a discoverable way to fully quit from inside the window itself.
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn get_network_stats(state: State<'_, AppState>) -> Result<NetworkStats, String> {
    let stats = agent::get_stats(&state.agent).await.map_err(|e| e.to_string())?;
    // getRecentBlocks is ordered oldest-to-newest, so the last entry (if any)
    // is the most recently mined block network-wide, by anyone -- not just
    // this miner. Best-effort: a failure here shouldn't sink the whole
    // network-stats refresh, so it just leaves the "last block" tile blank.
    let last_block_at_nanos = agent::get_recent_blocks(&state.agent)
        .await
        .ok()
        .and_then(|blocks| blocks.last().map(|b| b.timestamp.0.clone()))
        .and_then(|t| t.try_into().ok());
    Ok(NetworkStats {
        height: nat_to_plain_string(&stats.height),
        difficulty_bits: stats.difficultyBits.0.try_into().unwrap_or(0),
        blocks_until_retarget: stats.blocksUntilRetarget.0.try_into().unwrap_or(0),
        retarget_interval_blocks: stats.retargetIntervalBlocks.0.try_into().unwrap_or(0),
        last_retarget_at_nanos: stats.lastRetargetAt.0.try_into().unwrap_or(0),
        target_block_time_nanos: stats.targetBlockTimeNanos.0.try_into().unwrap_or(0),
        next_halving_height: nat_to_plain_string(&stats.nextHalvingHeight),
        current_reward: nat_to_plain_string(&stats.currentReward),
        mining_fee_e8s: nat_to_plain_string(&stats.miningFeeE8s),
        last_block_at_nanos,
    })
}

#[tauri::command]
async fn get_icp_balance(state: State<'_, AppState>) -> Result<String, String> {
    let owner = Principal::from_text(&state.principal_text).map_err(|e| e.to_string())?;
    agent::ledger_balance(&state.agent, agent::ICP_LEDGER_CANISTER_ID, owner)
        .await
        .map(|n| nat_to_plain_string(&n))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_piko_balance(state: State<'_, AppState>) -> Result<String, String> {
    let owner = Principal::from_text(&state.principal_text).map_err(|e| e.to_string())?;
    agent::ledger_balance(&state.agent, agent::PIKO_LEDGER_CANISTER_ID, owner)
        .await
        .map(|n| nat_to_plain_string(&n))
        .map_err(|e| e.to_string())
}

/// Sends `amount` (raw e8s, as a decimal string) of `token` ("PIKO" or
/// "ICP") to `to` (a principal, as text).
#[tauri::command]
async fn send_token(state: State<'_, AppState>, token: String, to: String, amount: String) -> Result<String, String> {
    let ledger_id = match token.as_str() {
        "PIKO" => agent::PIKO_LEDGER_CANISTER_ID,
        "ICP" => agent::ICP_LEDGER_CANISTER_ID,
        _ => return Err(format!("unknown token: {token}")),
    };
    let to_principal = Principal::from_text(to.trim()).map_err(|e| format!("invalid recipient: {e}"))?;
    let amount_nat = Nat::parse(amount.trim().as_bytes()).map_err(|e| format!("invalid amount: {e}"))?;
    agent::transfer(&state.agent, ledger_id, to_principal, amount_nat)
        .await
        .map(|n| nat_to_plain_string(&n))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_total_blocks_won(state: State<'_, AppState>) -> Result<String, String> {
    let owner = Principal::from_text(&state.principal_text).map_err(|e| e.to_string())?;
    agent::get_miner_stats(&state.agent, owner)
        .await
        .map(|entry| nat_to_plain_string(&entry.blocksFound))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_icp_allowance(state: State<'_, AppState>) -> Result<String, String> {
    let owner = Principal::from_text(&state.principal_text).map_err(|e| e.to_string())?;
    agent::icp_allowance(&state.agent, owner)
        .await
        .map(|n| nat_to_plain_string(&n))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_mining_fee(state: State<'_, AppState>) -> Result<String, String> {
    let work = agent::get_work(&state.agent).await.map_err(|e| e.to_string())?;
    Ok(nat_to_plain_string(&work.miningFeeE8s))
}

/// Approves enough ICP for `blocks` future submissions at the current fee.
#[tauri::command]
async fn approve_icp(state: State<'_, AppState>, blocks: u32) -> Result<String, String> {
    let work = agent::get_work(&state.agent).await.map_err(|e| e.to_string())?;
    let per_block = work.miningFeeE8s + Nat::from(ICP_LEDGER_FEE_E8S);
    let amount = per_block * Nat::from(blocks.max(1));
    agent::approve_icp(&state.agent, amount)
        .await
        .map(|n| nat_to_plain_string(&n))
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_mining(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut running = state.mining_running.lock().await;
    if *running {
        return Ok(()); // already mining, no-op
    }
    *running = true;
    drop(running);

    let owner = Principal::from_text(&state.principal_text).map_err(|e| e.to_string())?;
    state.mining_stop.store(false, Ordering::Relaxed);
    let stop_flag = Arc::clone(&state.mining_stop);
    let agent = Arc::clone(&state.agent);
    let running_flag = Arc::clone(&state.mining_running);
    let power_percent = Arc::clone(&state.power_percent);
    let gpu_enabled = Arc::clone(&state.gpu_enabled);
    let app_handle = app.clone();

    tauri::async_runtime::spawn(async move {
        mining_supervisor(app_handle, agent, owner, stop_flag, running_flag, power_percent, gpu_enabled).await;
    });

    Ok(())
}

#[tauri::command]
async fn stop_mining(state: State<'_, AppState>) -> Result<(), String> {
    state.mining_stop.store(true, Ordering::Relaxed);
    Ok(())
}

/// Sets the live power level (0-100, 100 = full speed) -- picked up
/// immediately by any already-running mining threads, same idea as the
/// browser site's own Low/High/Max dutyCycle control.
#[tauri::command]
fn set_power_percent(state: State<'_, AppState>, percent: u32) -> Result<(), String> {
    state.power_percent.store(percent.clamp(1, 100), Ordering::Relaxed);
    Ok(())
}

/// Name of the GPU adapter this machine could mine with, or null if none
/// was found at startup -- the frontend uses this to decide whether to
/// show the GPU toggle at all, rather than offering a setting that can
/// never do anything on e.g. a VM with no GPU passthrough.
#[tauri::command]
fn gpu_adapter_name(state: State<'_, AppState>) -> Option<String> {
    state.gpu_adapter_name.clone()
}

#[tauri::command]
fn get_gpu_enabled(state: State<'_, AppState>) -> bool {
    state.gpu_enabled.load(Ordering::Relaxed)
}

/// Picked up on the next job restart by an already-running mining_supervisor
/// (checked once per job, same cadence as everything else that can only
/// meaningfully change between jobs -- num_threads, the header bytes, etc.),
/// not applied to an in-flight GPU search immediately.
#[tauri::command]
fn set_gpu_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state.gpu_enabled.store(enabled, Ordering::Relaxed);
    Ok(())
}

async fn mining_supervisor(
    app: AppHandle,
    agent: Arc<Agent>,
    owner: Principal,
    stop_flag: Arc<AtomicBool>,
    running_flag: Arc<AsyncMutex<bool>>,
    power_percent: Arc<AtomicU32>,
    gpu_enabled: Arc<AtomicBool>,
) {
    let num_threads = num_cpus::get();
    let mut session_attempts: u64 = 0;
    let mut session_blocks: u64 = 0;

    'outer: loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        let work = match agent::get_work(&agent).await {
            Ok(w) => w,
            Err(e) => {
                emit_progress(&app, 0, session_attempts, session_blocks, &format!("Failed to fetch work: {e}"), "critical");
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        // Checked before spending any time searching, not just after a
        // doomed submission -- otherwise a search that can never be paid
        // for anyway still burns up to the full average find time (minutes,
        // at real difficulty) before ever discovering that.
        let cost_per_block = work.miningFeeE8s.clone() + Nat::from(ICP_LEDGER_FEE_E8S);
        let allowance = agent::icp_allowance(&agent, owner).await.unwrap_or_else(|_| Nat::from(0u64));
        let balance = agent::ledger_balance(&agent, agent::ICP_LEDGER_CANISTER_ID, owner)
            .await
            .unwrap_or_else(|_| Nat::from(0u64));
        if allowance < cost_per_block || balance < cost_per_block {
            stop_flag.store(true, Ordering::Relaxed);
            emit_stopped(
                &app,
                session_attempts,
                session_blocks,
                "Mining stopped: insufficient ICP allowance/balance -- approve more ICP to keep mining.",
            );
            break 'outer;
        }

        let mut previous_hash = [0u8; 32];
        let raw = work.previousHash.as_ref();
        if raw.len() != 32 {
            emit_progress(&app, 0, session_attempts, session_blocks, "Unexpected header length from mother", "critical");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        previous_hash.copy_from_slice(raw);
        let height: u64 = work.height.0.try_into().unwrap_or(0);
        let difficulty_bits: u32 = work.difficultyBits.0.try_into().unwrap_or(0);
        let target_height = height;

        let job = miner::MiningJob {
            previous_hash,
            height,
            difficulty_bits,
        };

        // CPU and (optionally) GPU search the same job concurrently,
        // sharing one stop flag, one hash-attempt counter, and one winner
        // channel -- the supervisor below doesn't need to know or care
        // which backend actually found the winning nonce.
        let job_stop_flag = Arc::new(AtomicBool::new(false));
        let job_hash_count = Arc::new(AtomicU64::new(0));
        let (tx, rx) = std::sync::mpsc::channel::<u64>();
        let mut handle = miner::MiningHandle::new(Arc::clone(&job_stop_flag), Arc::clone(&job_hash_count));

        let cpu_threads = miner::start_mining(
            job,
            num_threads,
            Arc::clone(&power_percent),
            Arc::clone(&job_stop_flag),
            Arc::clone(&job_hash_count),
            tx.clone(),
        );
        handle.add_threads(cpu_threads);

        if gpu_enabled.load(Ordering::Relaxed) {
            if let Some(gpu_thread) = gpu::start_gpu_mining(
                job,
                Arc::clone(&power_percent),
                Arc::clone(&job_stop_flag),
                Arc::clone(&job_hash_count),
                tx.clone(),
            ) {
                handle.add_threads(vec![gpu_thread]);
            }
        }
        drop(tx); // supervisor only reads rx; drop this end so the channel closes once every search thread's own clone is gone

        let mut last_report = std::time::Instant::now();
        let mut last_hash_count: u64 = 0;
        let mut last_stale_check = std::time::Instant::now();

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                handle.stop();
                break 'outer;
            }

            match rx.try_recv() {
                Ok(nonce) => {
                    // The winning thread already flipped the shared stop
                    // flag before sending, so the others are already
                    // winding down -- this just joins them.
                    handle.stop();

                    match agent::submit_proof(&agent, nonce).await {
                        Ok(types::SubmitResult::Ok(ok)) => {
                            session_blocks += 1;
                            let reward = format_piko(&ok.reward);
                            let height = nat_to_plain_string(&ok.height);
                            emit_progress(
                                &app,
                                0,
                                session_attempts,
                                session_blocks,
                                &format!("Block #{height} won -- +{reward} PIKO"),
                                "good",
                            );
                        }
                        Ok(types::SubmitResult::Err(err)) => {
                            // Insufficient allowance/balance can never resolve itself by
                            // retrying -- every future proof would hit the exact same
                            // wall, so looping here would just burn CPU forever instead
                            // of actually mining anything submittable.
                            let insufficient = matches!(
                                err,
                                types::SubmitError::IcpFeeFailed(
                                    types::TransferFromError::InsufficientAllowance { .. }
                                        | types::TransferFromError::InsufficientFunds { .. }
                                )
                            );
                            if insufficient {
                                stop_flag.store(true, Ordering::Relaxed);
                                emit_stopped(
                                    &app,
                                    session_attempts,
                                    session_blocks,
                                    "Mining stopped: insufficient ICP allowance/balance -- approve more ICP to keep mining.",
                                );
                                break 'outer;
                            }
                            emit_progress(
                                &app,
                                0,
                                session_attempts,
                                session_blocks,
                                &format!("Not accepted: {err:?} -- still mining"),
                                "",
                            );
                        }
                        Err(e) => {
                            emit_progress(
                                &app,
                                0,
                                session_attempts,
                                session_blocks,
                                &format!("Submission failed: {e} -- still mining"),
                                "",
                            );
                        }
                    }
                    continue 'outer;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    emit_progress(&app, 0, session_attempts, session_blocks, "Mining threads stopped unexpectedly", "critical");
                    break 'outer;
                }
            }

            // Roughly every 400ms, report the real hashrate since the last report.
            let now = std::time::Instant::now();
            if now.duration_since(last_report).as_millis() >= 400 {
                let current = handle.hash_count.load(Ordering::Relaxed);
                let delta = current.saturating_sub(last_hash_count);
                let elapsed_secs = now.duration_since(last_report).as_secs_f64().max(0.001);
                let hashrate = (delta as f64 / elapsed_secs).round() as u64;
                session_attempts += delta;
                last_hash_count = current;
                last_report = now;
                emit_progress(&app, hashrate, session_attempts, session_blocks, "", "");
            }

            // Every ~3s, check whether the chain has moved on (someone else
            // won) -- if so, this search is against a stale header and
            // should restart against the fresh one, same as the browser
            // site's own polling-driven restart.
            if now.duration_since(last_stale_check).as_secs() >= 3 {
                last_stale_check = now;
                if let Ok(fresh) = agent::get_work(&agent).await {
                    let fresh_height: u64 = fresh.height.0.clone().try_into().unwrap_or(0);
                    if fresh_height != target_height {
                        handle.stop();
                        continue 'outer;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    *running_flag.lock().await = false;
}

fn emit_progress(app: &AppHandle, hashrate: u64, session_attempts: u64, session_blocks: u64, message: &str, kind: &str) {
    let _ = app.emit(
        "mining-progress",
        MiningProgress {
            hashrate,
            session_attempts,
            session_blocks,
            message: message.to_string(),
            message_kind: kind.to_string(),
            stopped: false,
        },
    );
}

// Distinct from emit_progress's `stopped: false` -- lets the frontend tell
// "still mining, just noisy" apart from "the backend loop has actually
// exited," so it can flip its own Start/Stop button state back instead of
// showing "mining" (and burning CPU hashing for blocks that can never be
// submitted) forever after a permanent failure like exhausted allowance.
fn emit_stopped(app: &AppHandle, session_attempts: u64, session_blocks: u64, message: &str) {
    let _ = app.emit(
        "mining-progress",
        MiningProgress {
            hashrate: 0,
            session_attempts,
            session_blocks,
            message: message.to_string(),
            message_kind: "critical".to_string(),
            stopped: true,
        },
    );
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first (Tauri's own recommendation): without
        // this, launching the app a second time (e.g. from the desktop
        // icon, since GNOME hides the tray icon by default and the window
        // is only hidden -- not closed -- on X) spawns a whole separate OS
        // process with its own fresh AppState instead of focusing the
        // already-running one. That second process shows "not mining" (0
        // hashrate, Start button enabled) while the real, hidden first
        // process keeps mining untouched -- and quitting the second process
        // does nothing to the first. This plugin makes a second launch a
        // no-op that just shows/focuses the existing window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let identity = identity::load_or_create_identity().expect("failed to load/create local identity");
            let principal = ic_agent::Identity::sender(&identity).expect("identity has no principal");
            let principal_text = principal.to_text();
            let agent = agent::build_agent(identity).expect("failed to build IC agent");
            // One-off adapter enumeration at startup -- cheap (no device is
            // created here, just queried), so doing it synchronously before
            // the window even shows is simpler than plumbing an async probe
            // through the frontend's first render.
            let gpu_adapter_name = gpu::probe();
            if let Some(name) = &gpu_adapter_name {
                println!("GPU mining available: {name}");
            } else {
                println!("GPU mining unavailable: no usable adapter found");
            }

            app.manage(AppState {
                agent: Arc::new(agent),
                principal_text,
                mining_stop: Arc::new(AtomicBool::new(false)),
                mining_running: Arc::new(AsyncMutex::new(false)),
                power_percent: Arc::new(AtomicU32::new(100)),
                gpu_adapter_name,
                gpu_enabled: Arc::new(AtomicBool::new(false)),
            });

            // System tray -- lets mining keep running in the background when
            // the window is closed (see the on_window_event handler below),
            // instead of quitting the whole app the moment someone clicks
            // the window's close button, which would silently stop mining.
            use tauri::menu::{Menu, MenuItem};
            use tauri::tray::TrayIconBuilder;
            let show_i = MenuItem::with_id(app, "show", "Show PIKO Native Miner", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit (stops mining)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;
            let mut tray = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("PIKO Native Miner")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Hide instead of quitting -- mining (running in a
                // background tokio task, not tied to the window) keeps
                // going, reachable again from the tray's "Show" item.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_principal,
            get_network_stats,
            get_icp_balance,
            get_piko_balance,
            get_total_blocks_won,
            get_icp_allowance,
            get_mining_fee,
            approve_icp,
            send_token,
            get_autostart,
            set_autostart,
            start_mining,
            stop_mining,
            set_power_percent,
            gpu_adapter_name,
            get_gpu_enabled,
            set_gpu_enabled,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
