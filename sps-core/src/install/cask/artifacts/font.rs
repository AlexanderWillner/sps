// ===== sps-core/src/build/cask/artifacts/font.rs =====

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

/// Implements the `font` stanza by moving each declared
/// font file or directory from the staging area into
/// `~/Library/Fonts`, then symlinking it in the Caskroom.
///
/// Mirrors Homebrew’s `Dictionary < Moved` and `Colorpicker < Moved` pattern.
pub fn install_font(
    cask: &Cask,
    stage_path: &Path,
    cask_version_install_path: &Path,
    config: &Config,
) -> Result<Vec<InstalledArtifact>> {
    let mut installed = Vec::new();

    // Look for "font" entries in the JSON artifacts
    if let Some(artifacts_def) = &cask.artifacts {
        for art in artifacts_def {
            if let Some(obj) = art.as_object() {
                if let Some(entries) = obj.get("font").and_then(|v| v.as_array()) {
                    // Target directory for user fonts
                    let dest_dir = config.home_dir().join("Library").join("Fonts");
                    fs::create_dir_all(&dest_dir)?;

                    for entry in entries {
                        if let Some(name) = entry.as_str() {
                            let src = stage_path.join(name);
                            if !src.exists() {
                                debug!("Font '{}' not found in staging; skipping", name);
                                continue;
                            }

                            let dest = dest_dir.join(name);
                            if dest.exists() {
                                let _ = remove_path_robustly(&dest, config, true);
                            }

                            debug!("Installing font '{}' → '{}'", src.display(), dest.display());
                            // Try move, fallback to copy
                            let status = Command::new("mv").arg(&src).arg(&dest).status()?;
                            if !status.success() {
                                Command::new("cp").arg("-R").arg(&src).arg(&dest).status()?;
                            }

                            // Record moved font
                            installed.push(InstalledArtifact::MovedResource { path: dest.clone() });

                            // Symlink into Caskroom for reference
                            let link = cask_version_install_path.join(name);
                            let _ = remove_path_robustly(&link, config, true);
                            symlink(&dest, &link)?;
                            installed.push(InstalledArtifact::CaskroomLink {
                                link_path: link,
                                target_path: dest.clone(),
                            });
                        }
                    }
                    break; // single font stanza per cask
                }
            }
        }
    }

    Ok(installed)
}
