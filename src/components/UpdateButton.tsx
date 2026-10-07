import { useEffect, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

const CHECK_EVERY_MS = 6 * 60 * 60_000;

// Checks the latest GitHub Release for a newer signed build and, on click,
// downloads + installs it in place and restarts the app -- no uninstall /
// re-download needed. Renders nothing while the app is up to date.
export function UpdateButton() {
  const [update, setUpdate] = useState<Update | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    const run = () =>
      check()
        .then(setUpdate)
        .catch((err) => console.error("Update check failed", err));
    run();
    const id = setInterval(run, CHECK_EVERY_MS);
    return () => clearInterval(id);
  }, []);

  if (!update) return null;

  async function handleUpdate() {
    if (!update) return;
    if (!window.confirm(`Install PikoNativeMiner v${update.version} now? Mining stops and the app restarts.`)) return;
    let total = 0;
    let done = 0;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
          setStatus("Downloading...");
        } else if (event.event === "Progress") {
          done += event.data.chunkLength;
          if (total > 0) setStatus(`Downloading ${Math.round((done / total) * 100)}%`);
        } else if (event.event === "Finished") {
          setStatus("Installing...");
        }
      });
      await relaunch();
    } catch (err) {
      console.error("Update failed", err);
      setStatus("Update failed -- try again, or download it from GitHub.");
    }
  }

  return (
    <button
      type="button"
      className="button small"
      onClick={handleUpdate}
      disabled={status !== null && !status.startsWith("Update failed")}
      title={update.body ?? undefined}
    >
      {status ?? `Update to v${update.version}`}
    </button>
  );
}
