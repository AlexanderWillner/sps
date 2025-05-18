// ===== sps-core/src/build/cask/artifacts/screen_saver.rs =====

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use sps_common::config::Config;
use sps_common::error::Result;
use sps_common::model::artifact::InstalledArtifact;
use sps_common::model::cask::Cask;
use tracing::debug;

use crate::install::cask::helpers::remove_path_robustly;

/// Installs `screen_saver` bundles from the staging area into
/// `~/Library/Screen Savers`, then symlinks them into the Caskroom.
///
/// Mirrors Homebrew’s `ScreenSaver < Moved` pattern.
pub fn install_screen_saver(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("screen_saver").and_then(|v| v.as_array()) {
                    // Target directory for user screen savers
                    let dest_dir = config.home_dir().join("Library").join("Screen Savers");
                    fs::create_dir_all(&dest_dir)?;

                    for entry in entries {
                        if let Some(bundle_name) = entry.as_str() {
                            let src = stage_path.join(bundle_name);
                            if !src.exists() {
                                debug!(
                                    "Screen saver '{}' not found in staging; skipping",
                                    bundle_name
                                );
                                continue;
                            }

                            let dest = dest_dir.join(bundle_name);
                            if dest.exists() {
                                let _ = remove_path_robustly(&dest, config, true);
                            }

                            debug!(
                                "Installing screen saver '{}' → '{}',",
                                src.display(),
                                dest.display()
                            );
                            // Try move, fallback to copy
                            let status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record moved screen saver
                            installed.push(InstalledArtifact::MovedResource { path: dest.clone() });

                            // Symlink into Caskroom for reference
                            let link = cask_version_install_path.join(bundle_name);
                            let _ = remove_path_robustly(&link, config, true);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest.clone(),
                            });
                        }
                    }
                    break; // single stanza
                }
            }
        }
    }

    Ok(installed)
}
