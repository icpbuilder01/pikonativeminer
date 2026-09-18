import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Confetti } from "./components/Confetti";
import "./App.css";

interface MiningProgress {
  hashrate: number;
  sessionAttempts: number;
  sessionBlocks: number;
  message: string;
  messageKind: string;
  stopped: boolean;
}

interface NetworkStats {
  height: string;
  difficultyBits: number;
  blocksUntilRetarget: number;
  retargetIntervalBlocks: number;
  lastRetargetAtNanos: number;
  targetBlockTimeNanos: number;
  nextHalvingHeight: string;
  currentReward: string;
  miningFeeE8s: string;
  lastBlockAtNanos: number | null;
}

interface DifficultyPoint {
  t: number;
  bits: number;
}

const DIFFICULTY_HISTORY_LIMIT = 60;

type PowerLevel = "low" | "high" | "max";
const POWER_PERCENT: Record<PowerLevel, number> = { low: 32, high: 75, max: 100 };
const POWER_STORAGE_KEY = "piko-mining-power";

type Token = "PIKO" | "ICP";

const DECIMALS_FACTOR = 10n ** 8n; // both PIKO and ICP use 8 decimals

function formatAmount(raw: string): string {
  try {
    const value = BigInt(raw);
    const whole = value / DECIMALS_FACTOR;
    const frac = value % DECIMALS_FACTOR;
    const fracStr = frac.toString().padStart(8, "0").replace(/0+$/, "");
    return fracStr.length > 0 ? `${whole}.${fracStr}` : whole.toString();
  } catch {
    return raw;
  }
}

function parseAmount(input: string): bigint | null {
  const trimmed = input.trim();
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return null;
  const [wholePart, fracPart = ""] = trimmed.split(".");
  if (fracPart.length > 8) return null;
  const paddedFrac = fracPart.padEnd(8, "0");
  try {
    return BigInt(wholePart) * DECIMALS_FACTOR + BigInt(paddedFrac || "0");
  } catch {
    return null;
  }
}

function formatHashrate(hashesPerSecond: number): string {
  if (!isFinite(hashesPerSecond) || hashesPerSecond <= 0) return "0 H/s";
  const units = ["H/s", "KH/s", "MH/s", "GH/s", "TH/s"];
  let value = hashesPerSecond;
  let i = 0;
  while (value >= 1000 && i < units.length - 1) {
    value /= 1000;
    i += 1;
  }
  return `${value.toFixed(value < 10 ? 2 : 1)} ${units[i]}`;
}

function formatCount(n: number): string {
  if (!isFinite(n) || n < 0) return "0";
  if (n < 100_000) return Math.round(n).toLocaleString();
  const units = ["", "K", "M", "B", "T"];
  let value = n;
  let i = 0;
  while (value >= 1000 && i < units.length - 1) {
    value /= 1000;
    i += 1;
  }
  return `${value.toFixed(value < 10 ? 2 : 1)}${units[i]}`;
}

function formatElapsed(sinceNanos: number, nowMillis: number): string {
  const seconds = Math.max(0, Math.round(nowMillis / 1000 - sinceNanos / 1e9));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s ago`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m ago`;
}

// No historical-difficulty API exists (mother only ever exposes the
// *current* value) -- this plots whatever this app has personally observed
// since it started running, a real if short-lived retargeting curve rather
// than a synthetic one.
function DifficultyChart({ points }: { points: DifficultyPoint[] }) {
  if (points.length < 2) {
    return <p className="hint">Collecting data -- the curve fills in as this app keeps running.</p>;
  }
  const width = 100;
  const height = 36;
  const bits = points.map((p) => p.bits);
  const min = Math.min(...bits);
  const max = Math.max(...bits);
  const span = Math.max(1, max - min);
  const coords = points.map((p, i) => {
    const x = (i / (points.length - 1)) * width;
    const y = height - ((p.bits - min) / span) * height;
    return `${x.toFixed(2)},${y.toFixed(2)}`;
  });
  return (
    <div>
      <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" className="difficulty-chart" role="img" aria-label="Difficulty over time">
        <polyline points={coords.join(" ")} fill="none" stroke="var(--accent)" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
      </svg>
      <div className="difficulty-chart-labels">
        <span>{min} bits</span>
        <span>{max} bits</span>
      </div>
    </div>
  );
}

function App() {
  const [principal, setPrincipal] = useState<string | null>(null);
  const [pikoBalance, setPikoBalance] = useState<string | null>(null);
  const [icpBalance, setIcpBalance] = useState<string | null>(null);
  const [allowance, setAllowance] = useState<string | null>(null);
  const [miningFee, setMiningFee] = useState<string | null>(null);
  const [approving, setApproving] = useState(false);
  const [approveBlocks, setApproveBlocks] = useState(20);
  const [mining, setMining] = useState(false);
  const [power, setPower] = useState<PowerLevel>(() => {
    try {
      const stored = localStorage.getItem(POWER_STORAGE_KEY);
      return stored === "low" || stored === "high" || stored === "max" ? stored : "max";
    } catch {
      return "max";
    }
  });
  const [hashrate, setHashrate] = useState(0);
  const [sessionAttempts, setSessionAttempts] = useState(0);
  const [sessionBlocks, setSessionBlocks] = useState(0);
  const [totalBlocks, setTotalBlocks] = useState<number | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [messageKind, setMessageKind] = useState<string>("");
  const [confettiTrigger, setConfettiTrigger] = useState(0);
  const [copied, setCopied] = useState(false);

  const [sendToken, setSendToken] = useState<Token>("PIKO");
  const [sendTo, setSendTo] = useState("");
  const [sendAmount, setSendAmount] = useState("");
  const [sending, setSending] = useState(false);
  const [sendStatus, setSendStatus] = useState<string | null>(null);

  const [networkStats, setNetworkStats] = useState<NetworkStats | null>(null);
  const [difficultyHistory, setDifficultyHistory] = useState<DifficultyPoint[]>([]);
  const [autostart, setAutostartState] = useState<boolean | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [gpuAdapterName, setGpuAdapterName] = useState<string | null>(null);
  const [gpuEnabled, setGpuEnabled] = useState(false);
  const [gpuError, setGpuError] = useState<string | null>(null);

  const [poolEnabled, setPoolEnabled] = useState(false);
  const [poolAllowance, setPoolAllowance] = useState<string | null>(null);
  const [approvingPool, setApprovingPool] = useState(false);
  const [poolApproveBlocks, setPoolApproveBlocks] = useState(20);
  const [poolShares, setPoolShares] = useState<number | null>(null);
  const [poolPendingReward, setPoolPendingReward] = useState<string | null>(null);
  const [poolRoundTotalShares, setPoolRoundTotalShares] = useState<number | null>(null);
  const [poolActiveMiners, setPoolActiveMiners] = useState<number | null>(null);
  const [poolActiveMinersNow, setPoolActiveMinersNow] = useState<number | null>(null);
  const [claiming, setClaiming] = useState(false);
  const [claimStatus, setClaimStatus] = useState<string | null>(null);

  useEffect(() => {
    const interval = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(interval);
  }, []);

  const refreshBalances = useCallback(async () => {
    try {
      const [piko, icp, allow, fee, totalWon] = await Promise.all([
        invoke<string>("get_piko_balance"),
        invoke<string>("get_icp_balance"),
        invoke<string>("get_icp_allowance"),
        invoke<string>("get_mining_fee"),
        invoke<string>("get_total_blocks_won"),
      ]);
      setPikoBalance(piko);
      setIcpBalance(icp);
      setAllowance(allow);
      setMiningFee(fee);
      setTotalBlocks(Number(totalWon));
    } catch (err) {
      console.error("Failed to refresh balances", err);
    }
  }, []);

  useEffect(() => {
    invoke<string>("get_principal").then(setPrincipal);
    invoke<boolean>("get_autostart").then(setAutostartState).catch(() => {});
    invoke<string | null>("gpu_adapter_name").then(setGpuAdapterName).catch(() => {});
    invoke<boolean>("get_gpu_enabled").then(setGpuEnabled).catch(() => {});
    invoke<boolean>("get_pool_enabled").then(setPoolEnabled).catch(() => {});
    refreshBalances();
    const interval = setInterval(refreshBalances, 10000);
    return () => clearInterval(interval);
  }, [refreshBalances]);

  async function handleToggleGpu() {
    const next = !gpuEnabled;
    try {
      await invoke("set_gpu_enabled", { enabled: next });
      setGpuEnabled(next);
      if (!next) setGpuError(null);
    } catch (err) {
      console.error("Failed to toggle GPU mining", err);
    }
  }

  async function handleTogglePool() {
    const next = !poolEnabled;
    try {
      await invoke("set_pool_enabled", { enabled: next });
      setPoolEnabled(next);
    } catch (err) {
      console.error("Failed to toggle pool mining", err);
    }
  }

  const refreshPoolStatus = useCallback(async () => {
    try {
      const [allow, [sharesStr, pendingReward], [totalSharesStr, activeMinersStr, activeMinersNowStr]] = await Promise.all([
        invoke<string>("get_pool_icp_allowance"),
        invoke<[string, string]>("get_my_pool_share"),
        invoke<[string, string, string]>("get_pool_stats"),
      ]);
      setPoolAllowance(allow);
      setPoolShares(Number(sharesStr));
      setPoolPendingReward(pendingReward);
      setPoolRoundTotalShares(Number(totalSharesStr));
      setPoolActiveMiners(Number(activeMinersStr));
      setPoolActiveMinersNow(Number(activeMinersNowStr));
    } catch (err) {
      console.error("Failed to refresh pool status", err);
    }
  }, []);

  useEffect(() => {
    if (!poolEnabled) return;
    refreshPoolStatus();
    const interval = setInterval(refreshPoolStatus, 10000);
    return () => clearInterval(interval);
  }, [poolEnabled, refreshPoolStatus]);

  async function handleApprovePool() {
    setApprovingPool(true);
    try {
      await invoke("approve_icp_for_pool", { blocks: Math.max(1, Math.trunc(poolApproveBlocks) || 1) });
      const allow = await invoke<string>("get_pool_icp_allowance");
      setPoolAllowance(allow);
    } catch (err) {
      setMessage(`Pool approval failed: ${err}`);
      setMessageKind("critical");
    } finally {
      setApprovingPool(false);
    }
  }

  async function handleClaimPoolReward() {
    setClaiming(true);
    setClaimStatus(null);
    try {
      await invoke("claim_pool_reward");
      setClaimStatus("Claimed.");
      await refreshPoolStatus();
      await refreshBalances();
    } catch (err) {
      setClaimStatus(`Claim failed: ${err}`);
    } finally {
      setClaiming(false);
    }
  }

  useEffect(() => {
    let cancelled = false;
    async function poll() {
      try {
        const stats = await invoke<NetworkStats>("get_network_stats");
        if (cancelled) return;
        setNetworkStats(stats);
        setDifficultyHistory((prev) => {
          const next = [...prev, { t: Date.now(), bits: stats.difficultyBits }];
          return next.length > DIFFICULTY_HISTORY_LIMIT ? next.slice(next.length - DIFFICULTY_HISTORY_LIMIT) : next;
        });
      } catch (err) {
        console.error("Failed to refresh network stats", err);
      }
    }
    poll();
    const interval = setInterval(poll, 15000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    try {
      localStorage.setItem(POWER_STORAGE_KEY, power);
    } catch {
      // best-effort only -- a blocked/full localStorage just means the
      // level won't survive a restart, not a functional problem
    }
    invoke("set_power_percent", { percent: POWER_PERCENT[power] }).catch((err) => {
      console.error("Failed to set power level", err);
    });
  }, [power]);

  async function handleToggleAutostart() {
    const next = !autostart;
    try {
      await invoke("set_autostart", { enabled: next });
      setAutostartState(next);
    } catch (err) {
      console.error("Failed to toggle autostart", err);
    }
  }

  async function handleQuit() {
    await invoke("quit_app");
  }

  useEffect(() => {
    const unlisten = listen<MiningProgress>("mining-progress", (event) => {
      const p = event.payload;
      setHashrate(p.hashrate);
      setSessionAttempts((n) => Math.max(n, p.sessionAttempts));
      setSessionBlocks(p.sessionBlocks);
      if (p.message) {
        setMessage(p.message);
        setMessageKind(p.messageKind);
        if (p.messageKind === "good") {
          refreshBalances();
          setConfettiTrigger((n) => n + 1);
        }
      }
      // The backend loop has actually exited (e.g. exhausted ICP
      // allowance) -- reflect that in the UI instead of leaving the Stop
      // button showing while nothing is hashing anymore.
      if (p.stopped) {
        setMining(false);
        setHashrate(0);
        refreshBalances();
      }
      // Cheap query, and the only way to notice a GPU search that was
      // enabled but failed to actually start (adapter/device request
      // failed at runtime) -- otherwise that failure is invisible, the
      // hashrate just silently matches CPU-only with no indication why.
      if (gpuEnabled) {
        invoke<string | null>("gpu_error").then(setGpuError).catch(() => {});
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, [refreshBalances, gpuEnabled]);

  async function handleCopyPrincipal() {
    if (!principal) return;
    await navigator.clipboard.writeText(principal);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  async function handleApprove() {
    setApproving(true);
    try {
      await invoke("approve_icp", { blocks: Math.max(1, Math.trunc(approveBlocks) || 1) });
      await refreshBalances();
    } catch (err) {
      setMessage(`Approval failed: ${err}`);
      setMessageKind("critical");
    } finally {
      setApproving(false);
    }
  }

  async function handleStart() {
    setSessionAttempts(0);
    setSessionBlocks(0);
    setMessage(null);
    setMessageKind("");
    setMining(true);
    await invoke("start_mining");
  }

  async function handleStop() {
    setMining(false);
    setHashrate(0);
    await invoke("stop_mining");
  }

  async function handleSend(event: React.FormEvent) {
    event.preventDefault();
    const amount = parseAmount(sendAmount);
    if (amount === null || amount <= 0n) {
      setSendStatus("Enter a valid amount.");
      return;
    }
    setSending(true);
    setSendStatus(null);
    try {
      await invoke("send_token", { token: sendToken, to: sendTo.trim(), amount: amount.toString() });
      setSendStatus(`Sent ${formatAmount(amount.toString())} ${sendToken}.`);
      setSendTo("");
      setSendAmount("");
      refreshBalances();
    } catch (err) {
      setSendStatus(`Failed: ${err}`);
    } finally {
      setSending(false);
    }
  }

  const costPerBlock = miningFee ? BigInt(miningFee) + 10_000n : null;
  const affordableBlocks =
    costPerBlock && allowance && icpBalance
      ? (() => {
          const allowanceBlocks = BigInt(allowance) / costPerBlock;
          const balanceBlocks = BigInt(icpBalance) / costPerBlock;
          return balanceBlocks < allowanceBlocks ? balanceBlocks : allowanceBlocks;
        })()
      : null;
  const canMine = affordableBlocks !== null && affordableBlocks > 0n;

  return (
    <main className="container">
      <Confetti trigger={confettiTrigger} />
      <header className="header">
        <img src="/piko-logo.svg" alt="" className="brand-logo" />
        <div>
          <div className="brand-name">PIKO Native Miner</div>
          <div className="brand-sub">Real hardware-accelerated hashing, no browser needed</div>
        </div>
        <div className="header-actions">
          {autostart !== null && (
            <label className="autostart-toggle">
              <input type="checkbox" checked={autostart} onChange={handleToggleAutostart} />
              Launch at login
            </label>
          )}
          <button type="button" className="button secondary small" onClick={handleQuit} title="Fully exit -- closing this window with X only hides it so mining keeps running in the background">
            Quit
          </button>
        </div>
      </header>

      {!principal ? (
        <p className="empty-state">Loading your local identity...</p>
      ) : (
        <div className="panels">
          <section className="block">
            <h2>Network</h2>
            <div className="stat-grid">
              <div className="stat-tile">
                <div className="stat-label">Chain height</div>
                <div className="stat-value">{networkStats ? Number(networkStats.height).toLocaleString() : "..."}</div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Difficulty</div>
                <div className="stat-value">{networkStats ? `${networkStats.difficultyBits} bits` : "..."}</div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Next retarget</div>
                <div className="stat-value">
                  {networkStats ? `${networkStats.blocksUntilRetarget} blocks` : "..."}
                </div>
              </div>
            </div>
            <div>
              <div className="stat-label">Difficulty over time (this session)</div>
              <DifficultyChart points={difficultyHistory} />
            </div>
            <div className="stat-grid">
              <div className="stat-tile">
                <div className="stat-label">Block reward</div>
                <div className="stat-value stat-value-row">
                  {networkStats ? (
                    <>
                      {formatAmount(networkStats.currentReward)}
                      <img src="/piko-logo.svg" alt="PIKO" className="token-icon" />
                    </>
                  ) : (
                    "..."
                  )}
                </div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Mining fee</div>
                <div className="stat-value">
                  {networkStats ? `${formatAmount(networkStats.miningFeeE8s)} ICP` : "..."}
                </div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Last block</div>
                <div className="stat-value">
                  {networkStats?.lastBlockAtNanos != null ? formatElapsed(networkStats.lastBlockAtNanos, now) : "..."}
                </div>
              </div>
            </div>
          </section>

          <section className="block">
            <h2>Wallet</h2>
            <div className="wallet-balances">
              <div className="wallet-balance-tile">
                <div className="stat-label token-label">
                  <img src="/piko-logo.svg" alt="" className="token-icon" />
                  PIKO
                </div>
                <div className="stat-value">{pikoBalance !== null ? formatAmount(pikoBalance) : "..."}</div>
              </div>
              <div className="wallet-balance-tile">
                <div className="stat-label token-label">
                  <img src="/icp-logo.svg" alt="" className="token-icon" />
                  ICP
                </div>
                <div className="stat-value">{icpBalance !== null ? formatAmount(icpBalance) : "..."}</div>
              </div>
            </div>

            <div>
              <div className="stat-label">Your address (receives both PIKO and ICP)</div>
              <div className="wallet-address-row">
                <code className="wallet-address">{principal}</code>
                <button type="button" className="button secondary small" onClick={handleCopyPrincipal}>
                  {copied ? "Copied" : "Copy"}
                </button>
              </div>
            </div>

            <form className="wallet-send" onSubmit={handleSend}>
              <div className="token-toggle" role="tablist" aria-label="Token">
                <button
                  type="button"
                  role="tab"
                  aria-selected={sendToken === "PIKO"}
                  className={`token-toggle-btn ${sendToken === "PIKO" ? "active" : ""}`}
                  onClick={() => setSendToken("PIKO")}
                >
                  <img src="/piko-logo.svg" alt="" className="token-icon" />
                  Send PIKO
                </button>
                <button
                  type="button"
                  role="tab"
                  aria-selected={sendToken === "ICP"}
                  className={`token-toggle-btn ${sendToken === "ICP" ? "active" : ""}`}
                  onClick={() => setSendToken("ICP")}
                >
                  <img src="/icp-logo.svg" alt="" className="token-icon" />
                  Send ICP
                </button>
              </div>
              <input
                className="input"
                placeholder="Recipient principal"
                value={sendTo}
                onChange={(e) => setSendTo(e.target.value)}
              />
              <div className="wallet-send-row">
                <input
                  className="input"
                  placeholder={`Amount (${sendToken})`}
                  value={sendAmount}
                  onChange={(e) => setSendAmount(e.target.value)}
                  inputMode="decimal"
                />
                <button type="submit" className="button" disabled={sending}>
                  {sending ? "Sending..." : `Send ${sendToken}`}
                </button>
              </div>
              {sendStatus && <p className="wallet-status">{sendStatus}</p>}
              <p className="hint">Transfers use the standard ICRC-1 ledger fee (0.0001 {sendToken}).</p>
            </form>
          </section>

          <section className="block panel-mine">
            <h2>Mine</h2>
            <p className="hint">
              Fund the wallet above with ICP, approve it below, then start mining -- your local
              principal signs real hashing proofs directly, no browser required.
            </p>
            <div className="approve-row">
              <label className="approve-blocks-label">
                Approve for
                <input
                  className="input approve-blocks-input"
                  type="number"
                  min={1}
                  step={1}
                  value={approveBlocks}
                  onChange={(e) => setApproveBlocks(Math.max(1, Math.trunc(Number(e.target.value)) || 1))}
                  disabled={approving}
                />
                blocks
              </label>
              <button className="button secondary" onClick={handleApprove} disabled={approving}>
                {approving ? "Approving..." : `Approve ${miningFee !== null ? formatAmount((BigInt(miningFee) + 10_000n).toString()) : "..."} ICP/block`}
              </button>
            </div>

            <div className="power-row">
              <span className="power-label">Power:</span>
              <div className="power-buttons">
                {(["low", "high", "max"] as const).map((level) => (
                  <button
                    key={level}
                    type="button"
                    className={`button small power-btn ${power === level ? "active" : ""}`}
                    onClick={() => setPower(level)}
                  >
                    {level === "low" ? "Low" : level === "high" ? "High" : "Max"}
                  </button>
                ))}
              </div>
              <span className="power-hint">
                {power === "max" ? "full CPU speed" : power === "high" ? "~75% CPU, cooler" : "~32% CPU, coolest"}
              </span>
            </div>

            <div className="power-row">
              <span className="power-label">GPU mining:</span>
              {gpuAdapterName ? (
                <>
                  <label className="gpu-toggle">
                    <input type="checkbox" checked={gpuEnabled} onChange={handleToggleGpu} />
                    Use {gpuAdapterName}
                  </label>
                  <span className="power-hint">alongside CPU threads, takes effect on next start</span>
                </>
              ) : (
                <span className="power-hint">
                  No compatible GPU detected -- mining will use CPU only. Make sure your graphics driver is
                  up to date (on Linux, an NVIDIA card needs the proprietary driver, not nouveau).
                </span>
              )}
            </div>
            {gpuEnabled && gpuError && (
              <div className="power-row">
                <span className="power-label" />
                <span className="mining-message critical">GPU mining failed to start: {gpuError} -- continuing on CPU only.</span>
              </div>
            )}

            <div className="power-row">
              <span className="power-label">Pool mining:</span>
              <label className="gpu-toggle">
                <input type="checkbox" checked={poolEnabled} onChange={handleTogglePool} />
                Share rewards via PikoPool
              </label>
              <span className="power-hint">proportional split with everyone else mining, takes effect on next start</span>
            </div>

            {poolEnabled && (
              <div>
                <div className="approve-row">
                  <label className="approve-blocks-label">
                    Approve pool for
                    <input
                      className="input approve-blocks-input"
                      type="number"
                      min={1}
                      step={1}
                      value={poolApproveBlocks}
                      onChange={(e) => setPoolApproveBlocks(Math.max(1, Math.trunc(Number(e.target.value)) || 1))}
                      disabled={approvingPool}
                    />
                    blocks
                  </label>
                  <button className="button secondary" onClick={handleApprovePool} disabled={approvingPool}>
                    {approvingPool ? "Approving..." : "Approve ICP for pool"}
                  </button>
                  <button
                    type="button"
                    className="button secondary"
                    onClick={handleClaimPoolReward}
                    disabled={claiming || !poolPendingReward || poolPendingReward === "0"}
                  >
                    {claiming ? "Claiming..." : "Claim pool reward"}
                  </button>
                </div>
                {claimStatus && <p className="wallet-status">{claimStatus}</p>}

                <div className="stat-grid">
                  <div className="stat-tile">
                    <div className="stat-label">Pool ICP allowance</div>
                    <div className="stat-value">{poolAllowance !== null ? formatAmount(poolAllowance) : "..."}</div>
                  </div>
                  <div className="stat-tile">
                    <div className="stat-label">Shares this round</div>
                    <div className="stat-value">{poolShares !== null ? formatCount(poolShares) : "..."}</div>
                  </div>
                  <div className="stat-tile" title="Distinct wallets credited a share since the pool's last win -- includes anyone who mined earlier this round and then stopped, not just who's mining this instant.">
                    <div className="stat-label">Active miners (this round)</div>
                    <div className="stat-value">
                      {poolActiveMiners !== null ? formatCount(poolActiveMiners) : "..."}
                      {poolShares !== null && poolRoundTotalShares !== null && poolRoundTotalShares > 0 && (
                        <span className="stat-value-suffix"> ({((poolShares / poolRoundTotalShares) * 100).toFixed(1)}% ours)</span>
                      )}
                    </div>
                  </div>
                  <div className="stat-tile" title="Distinct wallets that have pinged the pool in the last ~60 seconds -- who's actually mining right now, not just who has contributed since the last win.">
                    <div className="stat-label">Mining right now</div>
                    <div className="stat-value">{poolActiveMinersNow !== null ? formatCount(poolActiveMinersNow) : "..."}</div>
                  </div>
                  <div className="stat-tile">
                    <div className="stat-label">Pending pool reward</div>
                    <div className="stat-value stat-value-row">
                      {poolPendingReward !== null ? formatAmount(poolPendingReward) : "..."}
                      <img src="/piko-logo.svg" alt="PIKO" className="token-icon" />
                    </div>
                  </div>
                </div>
              </div>
            )}

            <div className="stat-grid">
              <div className="stat-tile">
                <div className="stat-label">ICP allowance</div>
                <div className="stat-value">
                  {allowance !== null ? formatAmount(allowance) : "..."}
                  {affordableBlocks !== null && (
                    <span className="stat-value-suffix"> (~{affordableBlocks.toString()} blocks)</span>
                  )}
                </div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Hashrate</div>
                <div className={`stat-value ${mining ? "hot" : ""}`}>{formatHashrate(hashrate)}</div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Attempts this session</div>
                <div className="stat-value">{formatCount(sessionAttempts)}</div>
              </div>
              <div className="stat-tile">
                <div className="stat-label">Blocks won this session</div>
                <div className="stat-value">{sessionBlocks}</div>
              </div>
              <div className="stat-tile" title="From mother's own lifetime leaderboard, which only counts blocks submitted under your own principal -- a pool win is submitted by PikoPool's principal instead, so it's never included here even though you still get paid your share.">
                <div className="stat-label">Blocks won solo (lifetime)</div>
                <div className="stat-value">{totalBlocks !== null ? formatCount(totalBlocks) : "..."}</div>
              </div>
            </div>

            {!canMine && !mining && (
              <p className="hint warning">
                {icpBalance === "0" ? "Fund your wallet with ICP above to start mining." : "Approve ICP above to start mining."}
              </p>
            )}

            {mining ? (
              <button className="button secondary big" onClick={handleStop}>
                Stop mining
              </button>
            ) : (
              <button className="button button-cta big" onClick={handleStart} disabled={!canMine}>
                Start mining
              </button>
            )}

            {message && <div className={`mining-message ${messageKind}`}>{message}</div>}
          </section>
        </div>
      )}
    </main>
  );
}

export default App;
