// ===== sps-core/src/build/formula/mod.rs =====
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use sps_common::config::Config;
use sps_common::error::{Result, SpsError};
use sps_common::model::formula::Formula;
use tracing::{debug, error};

// Declare submodules
pub mod bottle;
pub mod link;
pub mod macho;
pub mod source;

/// Download formula resources from the internet asynchronously.
pub async fn download_formula(
    formula: &Formula,
    config: &Config,
    client: &reqwest::Client,
) -> Result<PathBuf> {
    if has_bottle_for_current_platform(formula) {
        bottle::download_bottle(formula, config, client).await
    } else {
        Err(SpsError::Generic(format!(
            "No bottle available for {} on this platform",
            formula.name()
        )))
    }
}

/// Checks if a suitable bottle exists for the current platform, considering fallbacks.
pub fn has_bottle_for_current_platform(formula: &Formula) -> bool {
    let result = crate::build::formula::bottle::get_bottle_for_platform(formula);
    debug!(
        "has_bottle_for_current_platform check for '{}': {:?}",
        formula.name(),
        result.is_ok()
    );
    if let Err(e) = &result {
        debug!("Reason for no bottle: {}", e);
    }
    result.is_ok()
}

// *** Updated get_current_platform function ***
fn get_current_platform() -> String {
    if cfg!(target_os = "macos") {
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else if std::env::consts::ARCH == "x86_64" {
            "x86_64"
        } else {
            std::env::consts::ARCH
        };

        debug!("Attempting to determine macOS version using /usr/bin/sw_vers -productVersion");
        match Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                debug!("sw_vers status: {}", output.status);
                if !output.status.success() || !stderr.trim().is_empty() {
                    debug!("sw_vers stdout:\n{}", stdout);
                    if !stderr.trim().is_empty() {
                        debug!("sw_vers stderr:\n{}", stderr);
                    }
                }

                if output.status.success() {
                    let version_str = stdout.trim();
                    if !version_str.is_empty() {
                        debug!("Extracted version string: {}", version_str);
                        let os_name = match version_str.split('.').next() {
                            Some("15") => "sequoia",
                            Some("14") => "sonoma",
                            Some("13") => "ventura",
                            Some("12") => "monterey",
                            Some("11") => "big_sur",
                            Some("10") => match version_str.split('.').nth(1) {
                                Some("15") => "catalina",
                                Some("14") => "mojave",
                                _ => {
                                    debug!(
                                        "Unrecognized legacy macOS 10.x version: {}",
                                        version_str
                                    );
                                    "unknown_macos"
                                }
                            },
                            _ => {
                                debug!("Unrecognized macOS major version: {}", version_str);
                                "unknown_macos"
                            }
                        };

                        if os_name != "unknown_macos" {
                            let platform_tag = if arch == "arm64" {
                                format!("{arch}_{os_name}")
                            } else {
                                os_name.to_string()
                            };
                            debug!("Determined platform tag: {}", platform_tag);
                            return platform_tag;
                        }
                    } else {
                        error!("sw_vers -productVersion output was empty.");
                    }
                } else {
                    error!(
                        "sw_vers -productVersion command failed with status: {}. Stderr: {}",
                        output.status,
                        stderr.trim()
                    );
                }
            }
            Err(e) => {
                error!("Failed to execute /usr/bin/sw_vers -productVersion: {}", e);
            }
        }

        error!("!!! FAILED TO DETECT MACOS VERSION VIA SW_VERS !!!");
        debug!("Using UNRELIABLE fallback platform detection. Bottle selection may be incorrect.");
        if arch == "arm64" {
            debug!("Falling back to platform tag: arm64_monterey");
            "arm64_monterey".to_string()
        } else {
            debug!("Falling back to platform tag: monterey");
            "monterey".to_string()
        }
    } else if cfg!(target_os = "linux") {
        if std::env::consts::ARCH == "aarch64" {
            "arm64_linux".to_string()
        } else if std::env::consts::ARCH == "x86_64" {
            "x86_64_linux".to_string()
        } else {
            "unknown".to_string()
        }
    } else {
        debug!(
            "Could not determine platform tag for OS: {}",
            std::env::consts::OS
        );
        "unknown".to_string()
    }
}

// REMOVED: get_cellar_path (now in Config)

// --- get_formula_cellar_path uses Config ---
// Parameter changed from formula: &Formula to formula_name: &str
// Parameter changed from config: &Config to cellar_path: &Path for consistency where Config isn't
// fully available If Config *is* available, call config.formula_cellar_dir(formula.name()) instead.
// **Keeping original signature for now where Config might not be easily passed**
pub fn get_formula_cellar_path(formula: &Formula, config: &Config) -> PathBuf {
    // Use Config method
    config.formula_cellar_dir(formula.name())
}

// --- write_receipt (unchanged) ---
pub fn write_receipt(formula: &Formula, install_dir: &Path) -> Result<()> {
    let receipt_path = install_dir.join("INSTALL_RECEIPT.json");
    let receipt_file = File::create(&receipt_path);
    let mut receipt_file = match receipt_file {
        Ok(file) => file,
        Err(e) => {
            error!(
                "Failed to create receipt file at {}: {}",
                receipt_path.display(),
                e
            );
            return Err(SpsError::Io(std::sync::Arc::new(e)));
        }
    };

    let resources_result = formula.resources();
    let resources_installed = match resources_result {
        Ok(res) => res.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
        Err(_) => {
            debug!(
                "Could not retrieve resources for formula {} when writing receipt.",
                formula.name
            );
            vec![]
        }
    };

    let timestamp = chrono::Utc::now().to_rfc3339();

    let receipt = serde_json::json!({
        "name": formula.name, "version": formula.version_str_full(), "time": timestamp,
        "source": { "type": "api", "url": formula.url, },
        "built_on": {
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "platform_tag": get_current_platform(),
         },
        "resources_installed": resources_installed,
    });

    let receipt_json = match serde_json::to_string_pretty(&receipt) {
        Ok(json) => json,
        Err(e) => {
            error!(
                "Failed to serialize receipt JSON for {}: {}",
                formula.name, e
            );
            return Err(SpsError::Json(std::sync::Arc::new(e)));
        }
    };

    if let Err(e) = receipt_file.write_all(receipt_json.as_bytes()) {
        error!("Failed to write receipt file for {}: {}", formula.name, e);
        return Err(SpsError::Io(std::sync::Arc::new(e)));
    }

    Ok(())
}

// --- Re-exports (unchanged) ---
pub use bottle::install_bottle;
pub use link::link_formula_artifacts;
