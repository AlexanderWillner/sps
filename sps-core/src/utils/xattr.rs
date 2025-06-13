use std::path::Path;
use std::process::Command;

use anyhow::Context;
use sps_common::error::{Result, SpsError};
use tracing::{debug, error};
use uuid::Uuid;
use xattr;

// Helper to get current timestamp as hex
fn get_timestamp_hex() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default() // Defaults to 0 if time is before UNIX_EPOCH
        .as_secs();
    format!("{secs:x}")
}

// Helper to generate a UUID as hex string
fn get_uuid_hex() -> String {
    Uuid::new_v4().as_hyphenated().to_string().to_uppercase()
}

/// true  → file **has** a com.apple.quarantine attribute  
/// false → attribute missing
pub fn has_quarantine_attribute(path: &Path) -> anyhow::Result<bool> {
    // The `xattr` crate has both path-level and FileExt APIs.
    // Path-level is simpler here.
    match xattr::get(path, "com.apple.quarantine") {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("checking xattr on {}", path.display())),
    }
}

/// Apply our standard quarantine only *if* none exists already.
///
/// `agent` should be the same string you currently pass to
/// `set_quarantine_attribute()` – usually the cask token.
pub fn ensure_quarantine_attribute(path: &Path, agent: &str) -> anyhow::Result<()> {
    if has_quarantine_attribute(path)? {
        // Already quarantined (or the user cleared it and we respect that) → done
        return Ok(());
    }
    set_quarantine_attribute(path, agent)
        .with_context(|| format!("adding quarantine to {}", path.display()))
}

/// Sets the 'com.apple.quarantine' extended attribute on a file or directory.
/// Uses flags commonly seen for user-initiated downloads (0081).
/// Logs errors assertively, as failure is critical for correct behavior.
pub fn set_quarantine_attribute(path: &Path, agent_name: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        debug!(
            "Not on macOS, skipping quarantine attribute for {}",
            path.display()
        );
        return Ok(());
    }

    if !path.exists() {
        error!(
            "Cannot set quarantine attribute, path does not exist: {}",
            path.display()
        );
        return Err(SpsError::NotFound(format!(
            "Path not found for setting quarantine attribute: {}",
            path.display()
        )));
    }

    let timestamp_hex = get_timestamp_hex();
    let uuid_hex = get_uuid_hex();
    // Use "0181" to disable translocation and quarantine mirroring (Homebrew-style).
    // Format: "flags;timestamp_hex;agent_name;uuid_hex"
    let quarantine_value = format!("0181;{timestamp_hex};{agent_name};{uuid_hex}");

    debug!(
        "Setting quarantine attribute on {}: value='{}'",
        path.display(),
        quarantine_value
    );

    let output = Command::new("xattr")
        .arg("-w")
        .arg("com.apple.quarantine")
        .arg(&quarantine_value)
        .arg(path.as_os_str())
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                debug!(
                    "Successfully set quarantine attribute for {}",
                    path.display()
                );
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                error!( // Changed from warn to error as this is critical for the bug
                    "Failed to set quarantine attribute for {} (status: {}): {}. This may lead to data loss on reinstall or Gatekeeper issues.",
                    path.display(),
                    out.status,
                    stderr.trim()
                );
                // Return an error because failure to set this is likely to cause the reported bug
                Err(SpsError::Generic(format!(
                    "Failed to set com.apple.quarantine on {}: {}",
                    path.display(),
                    stderr.trim()
                )))
            }
        }
        Err(e) => {
            error!(
                "Failed to execute xattr command for {}: {}. Quarantine attribute not set.",
                path.display(),
                e
            );
            Err(SpsError::Io(std::sync::Arc::new(e)))
        }
    }
}
