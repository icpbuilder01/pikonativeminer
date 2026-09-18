// Local identity management: a fresh Ed25519 keypair generated once on
// first run and persisted to the OS's own per-app data directory, reused
// on every later launch. This is the same underlying mechanism as an
// Internet Identity delegation -- a real keypair signing real calls -- just
// generated and held locally instead of behind a passkey/recovery-phrase
// flow, since this app has no browser to run Internet Identity's own login
// page in.
use anyhow::{Context, Result};
use ic_agent::identity::BasicIdentity;
use rand::RngCore;
use std::fs;
use std::path::PathBuf;

fn key_file_path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("could not determine the OS data directory")?
        .join("PikoNativeMiner");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("identity.key"))
}

/// Loads the existing local identity, or generates and persists a new one
/// on first run. The raw 32-byte Ed25519 private key is the only secret
/// this app ever holds -- never transmitted anywhere, only used locally to
/// sign calls to `mother`/the ICP ledger.
pub fn load_or_create_identity() -> Result<BasicIdentity> {
    let path = key_file_path()?;
    let key_bytes: [u8; 32] = if path.exists() {
        let raw = fs::read(&path).context("failed to read the saved identity key")?;
        raw.try_into()
            .map_err(|_| anyhow::anyhow!("saved identity key file is corrupted (wrong length)"))?
    } else {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        fs::write(&path, key).context("failed to save a newly generated identity key")?;
        key
    };
    // Re-applied on every load, not just at creation -- an install from
    // before this restriction existed would otherwise keep its
    // world-readable key forever, since this function's `if path.exists()`
    // branch never touches permissions on its own.
    restrict_to_owner(&path)?;
    Ok(BasicIdentity::from_raw_key(&key_bytes))
}

// This file holds the raw private key controlling the wallet -- on Unix,
// fs::write() otherwise leaves it at the umask's default (typically
// world-readable), letting any other local account read it. Windows'
// per-user profile ACLs already restrict this directory to the owner, so
// there's nothing equivalent to tighten there.
#[cfg(unix)]
fn restrict_to_owner(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("failed to restrict the identity key file's permissions")
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &PathBuf) -> Result<()> {
    Ok(())
}
