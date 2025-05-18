// ===== sps-core/src/build/extract.rs =====

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Seek};
use std::path::{Component, Path, PathBuf};

use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use sps_common::error::{Result, SpsError};
use tar::Archive;
use tracing::{debug, error};
use xz2::read::XzDecoder;
use zip::read::ZipArchive;

#[cfg(target_os = "macos")]
use crate::utils::xattr;

pub(crate) fn infer_archive_root_dir(
    archive_path: &Path,
    archive_type: &str,
) -> Result<Option<PathBuf>> {
    tracing::debug!(
        "Inferring root directory for archive: {}",
        archive_path.display()
    );
    let file = File::open(archive_path).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed to open archive {}: {}", archive_path.display(), e),
        )))
    })?;

    match archive_type {
        "zip" => infer_zip_root(file, archive_path),
        "gz" | "tgz" => {
            let decompressed = GzDecoder::new(file);
            infer_tar_root(decompressed, archive_path)
        }
        "bz2" | "tbz" | "tbz2" => {
            let decompressed = BzDecoder::new(file);
            infer_tar_root(decompressed, archive_path)
        }
        "xz" | "txz" => {
            let decompressed = XzDecoder::new(file);
            infer_tar_root(decompressed, archive_path)
        }
        "tar" => infer_tar_root(file, archive_path),
        _ => Err(SpsError::Generic(format!(
            "Cannot infer root dir for unsupported archive type '{}' in {}",
            archive_type,
            archive_path.display()
        ))),
    }
}

fn infer_tar_root<R: Read>(reader: R, archive_path_for_log: &Path) -> Result<Option<PathBuf>> {
    let mut archive = Archive::new(reader);
    let mut unique_roots = HashSet::new();
    let mut non_empty_entry_found = false;
    let mut first_component_name: Option<PathBuf> = None;

    for entry_result in archive.entries()? {
        let entry = entry_result.map_err(|e| {
            SpsError::Generic(format!(
                "Error reading TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;
        let path = entry
            .path()
            .map_err(|e| {
                SpsError::Generic(format!(
                    "Invalid path in TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

        if path.components().next().is_none() {
            continue;
        }

        if let Some(first_comp) = path.components().next() {
            if let Component::Normal(name) = first_comp {
                non_empty_entry_found = true;
                let current_root = PathBuf::from(name);
                if first_component_name.is_none() {
                    first_component_name = Some(current_root.clone());
                }
                unique_roots.insert(current_root);

                if unique_roots.len() > 1 {
                    tracing::debug!(
                        "Multiple top-level items found in TAR {}, cannot infer single root.",
                        archive_path_for_log.display()
                    );
                    return Ok(None);
                }
            } else {
                tracing::debug!(
                    "Non-standard top-level component ({:?}) found in TAR {}, cannot infer single root.",
                    first_comp,
                    archive_path_for_log.display()
                );
                return Ok(None);
            }
        } else {
            tracing::debug!(
                "Empty or unusual path found in TAR {}, cannot infer single root.",
                archive_path_for_log.display()
            );
            return Ok(None);
        }
    }

    if unique_roots.len() == 1 && non_empty_entry_found {
        let inferred_root = first_component_name.unwrap();
        tracing::debug!(
            "Inferred single root directory in TAR {}: {}",
            archive_path_for_log.display(),
            inferred_root.display()
        );
        Ok(Some(inferred_root))
    } else if !non_empty_entry_found {
        tracing::warn!(
            "TAR archive {} appears to be empty or contain only metadata.",
            archive_path_for_log.display()
        );
        Ok(None)
    } else {
        tracing::debug!(
            "No single common root directory found in TAR {}. unique_roots count: {}",
            archive_path_for_log.display(),
            unique_roots.len()
        );
        Ok(None)
    }
}

fn infer_zip_root<R: Read + Seek>(
    reader: R,
    archive_path_for_log: &Path,
) -> Result<Option<PathBuf>> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SpsError::Generic(format!(
            "Failed to open ZIP {}: {}",
            archive_path_for_log.display(),
            e
        ))
    })?;
    let mut unique_roots = HashSet::new();
    let mut non_empty_entry_found = false;
    let mut first_component_name: Option<PathBuf> = None;

    for i in 0..archive.len() {
        let file = archive.by_index_raw(i).map_err(|e| {
            SpsError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i,
                archive_path_for_log.display(),
                e
            ))
        })?;

        let path_str = file.name();
        let path = PathBuf::from(path_str);

        if path.components().next().is_none() {
            continue;
        }

        if let Some(first_comp) = path.components().next() {
            if let Component::Normal(name) = first_comp {
                non_empty_entry_found = true;
                let current_root = PathBuf::from(name);
                if first_component_name.is_none() {
                    first_component_name = Some(current_root.clone());
                }
                unique_roots.insert(current_root);

                if unique_roots.len() > 1 {
                    tracing::debug!(
                        "Multiple top-level items found in ZIP {}, cannot infer single root.",
                        archive_path_for_log.display()
                    );
                    return Ok(None);
                }
            } else {
                tracing::debug!(
                    "Non-standard top-level component ({:?}) found in ZIP {}, cannot infer single root.",
                    first_comp,
                    archive_path_for_log.display()
                );
                return Ok(None);
            }
        } else {
            tracing::debug!(
                "Empty or unusual path ('{}') found in ZIP {}, cannot infer single root.",
                path_str,
                archive_path_for_log.display()
            );
            return Ok(None);
        }
    }

    if unique_roots.len() == 1 && non_empty_entry_found {
        let inferred_root = first_component_name.unwrap();
        tracing::debug!(
            "Inferred single root directory in ZIP {}: {}",
            archive_path_for_log.display(),
            inferred_root.display()
        );
        Ok(Some(inferred_root))
    } else if !non_empty_entry_found {
        tracing::warn!(
            "ZIP archive {} appears to be empty or contain only metadata.",
            archive_path_for_log.display()
        );
        Ok(None)
    } else {
        tracing::debug!(
            "No single common root directory found in ZIP {}. unique_roots count: {}",
            archive_path_for_log.display(),
            unique_roots.len()
        );
        Ok(None)
    }
}

// --- Begin revised extraction helpers ---

/// Helper: Quarantine any .app bundles in a directory (macOS only)
#[cfg(target_os = "macos")]
pub fn quarantine_extracted_apps_in_stage(stage_dir: &Path, agent_name: &str) -> Result<()> {
    use std::fs;

    use tracing::{debug, warn};
    debug!(
        "Searching for .app bundles in {} to apply quarantine.",
        stage_dir.display()
    );
    if stage_dir.is_dir() {
        for entry_result in fs::read_dir(stage_dir)? {
            let entry = entry_result?;
            let entry_path = entry.path();
            if entry_path.is_dir() && entry_path.extension().is_some_and(|ext| ext == "app") {
                debug!(
                    "Found app bundle in stage: {}. Applying quarantine.",
                    entry_path.display()
                );
                if let Err(e) = xattr::set_quarantine_attribute(&entry_path, agent_name) {
                    warn!(
                        "Failed to set quarantine attribute on staged app {}: {}. Installation will continue.",
                        entry_path.display(),
                        e
                    );
                }
            }
        }
    }
    Ok(())
}

/// Extract an archive file (zip/tar/tar.gz/...) to a target directory, stripping a number of
/// leading path components.
pub fn extract_archive(
    archive_path: &Path,
    target_dir: &Path,
    strip_components: usize,
    archive_type: &str,
) -> Result<()> {
    debug!(
        "Extracting archive '{}' (type: {}) to '{}' (strip_components={}) using native Rust crates.",
        archive_path.display(),
        archive_type,
        target_dir.display(),
        strip_components
    );

    fs::create_dir_all(target_dir).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to create target directory {}: {}",
                target_dir.display(),
                e
            ),
        )))
    })?;

    let file = File::open(archive_path).map_err(|e| {
        SpsError::Io(std::sync::Arc::new(std::io::Error::new(
            e.kind(),
            format!("Failed to open archive {}: {}", archive_path.display(), e),
        )))
    })?;

    let result = match archive_type {
        "zip" => extract_zip_archive(file, target_dir, strip_components, archive_path),
        "gz" | "tgz" => {
            let tar = GzDecoder::new(file);
            extract_tar_archive(tar, target_dir, strip_components, archive_path)
        }
        "bz2" | "tbz" | "tbz2" => {
            let tar = BzDecoder::new(file);
            extract_tar_archive(tar, target_dir, strip_components, archive_path)
        }
        "xz" | "txz" => {
            let tar = XzDecoder::new(file);
            extract_tar_archive(tar, target_dir, strip_components, archive_path)
        }
        "tar" => extract_tar_archive(file, target_dir, strip_components, archive_path),
        _ => Err(SpsError::Generic(format!(
            "Unsupported archive type provided for extraction: '{}' for file {}",
            archive_type,
            archive_path.display()
        ))),
    };
    // After extraction, quarantine any .app bundles in the stage (macOS only)
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = quarantine_extracted_apps_in_stage(target_dir, "sps-extractor") {
            tracing::warn!(
                "Error during post-extraction quarantine scan for {}: {}",
                archive_path.display(),
                e
            );
        }
    }
    result
}

/// Extract a tar archive (possibly decompressed) to a target directory, stripping leading path
/// components.
fn extract_tar_archive<R: Read>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = Archive::new(reader);
    archive.set_preserve_permissions(true);
    archive.set_unpack_xattrs(true);
    archive.set_overwrite(false);

    debug!(
        "Starting TAR extraction for {}",
        archive_path_for_log.display()
    );

    for entry_result in archive.entries()? {
        let mut entry = entry_result.map_err(|e| {
            SpsError::Generic(format!(
                "Error reading TAR entry from {}: {}",
                archive_path_for_log.display(),
                e
            ))
        })?;

        let original_path: PathBuf = entry
            .path()
            .map_err(|e| {
                SpsError::Generic(format!(
                    "Invalid path in TAR entry from {}: {}",
                    archive_path_for_log.display(),
                    e
                ))
            })?
            .into_owned();

        let stripped: Vec<_> = original_path.components().skip(strip_components).collect();
        if stripped.is_empty() {
            debug!(
                "Skipping entry due to strip_components: {:?}",
                original_path
            );
            continue;
        }

        let mut target_path = target_dir.to_path_buf();
        for comp in stripped {
            match comp {
                Component::Normal(p) => target_path.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    error!(
                        "Unsafe '..' in TAR path {} after stripping in {}",
                        original_path.display(),
                        archive_path_for_log.display()
                    );
                    return Err(SpsError::Generic(format!(
                        "Unsafe '..' component in {}",
                        original_path.display()
                    )));
                }
                Component::Prefix(_) | Component::RootDir => {
                    error!(
                        "Disallowed component {:?} in TAR path {}",
                        comp,
                        original_path.display()
                    );
                    return Err(SpsError::Generic(format!(
                        "Disallowed component in {}",
                        original_path.display()
                    )));
                }
            }
        }
        if !target_path.starts_with(target_dir) {
            error!(
                "Path traversal {} -> {} detected in {}",
                original_path.display(),
                target_path.display(),
                archive_path_for_log.display()
            );
            return Err(SpsError::Generic(format!(
                "Path traversal detected in {}",
                archive_path_for_log.display()
            )));
        }

        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    SpsError::Io(std::sync::Arc::new(io::Error::new(
                        e.kind(),
                        format!("Failed create parent dir {}: {}", parent.display(), e),
                    )))
                })?;
            }
        }

        match entry.unpack(&target_path) {
            Ok(_) => debug!("Unpacked TAR entry to: {}", target_path.display()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                debug!(
                    "Entry exists, skipping unpack {}: {}",
                    target_path.display(),
                    e
                );
            }
            Err(e) => {
                error!(
                    "Failed to unpack {:?} to {}: {}",
                    original_path,
                    target_path.display(),
                    e
                );
                return Err(SpsError::Generic(format!(
                    "Failed unpack {original_path:?}: {e}"
                )));
            }
        }
    }
    debug!(
        "Finished TAR extraction for {}",
        archive_path_for_log.display()
    );
    // After extraction, quarantine any .app bundles in the stage (macOS only)
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = quarantine_extracted_apps_in_stage(target_dir, "sps-tar-extractor") {
            tracing::warn!(
                "Error during post-tar extraction quarantine scan for {}: {}",
                archive_path_for_log.display(),
                e
            );
        }
    }
    Ok(())
}

/// Extract a zip archive to a target directory, stripping leading path components.
fn extract_zip_archive<R: Read + Seek>(
    reader: R,
    target_dir: &Path,
    strip_components: usize,
    archive_path_for_log: &Path,
) -> Result<()> {
    let mut archive = ZipArchive::new(reader).map_err(|e| {
        SpsError::Generic(format!(
            "Failed to open ZIP {}: {}",
            archive_path_for_log.display(),
            e
        ))
    })?;
    debug!(
        "Starting ZIP extraction for {}",
        archive_path_for_log.display()
    );

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            SpsError::Generic(format!(
                "Error reading ZIP index {} in {}: {}",
                i,
                archive_path_for_log.display(),
                e
            ))
        })?;

        let original = match file.enclosed_name() {
            Some(p) => p.to_path_buf(),
            None => {
                debug!("Skipping unsafe ZIP name {}", file.name());
                continue;
            }
        };
        let stripped: Vec<_> = original.components().skip(strip_components).collect();
        if stripped.is_empty() {
            debug!("Skipping ZIP {} due to strip", original.display());
            continue;
        }

        let mut target_path = target_dir.to_path_buf();
        for comp in stripped {
            match comp {
                Component::Normal(p) => target_path.push(p),
                Component::CurDir => {}
                Component::ParentDir => {
                    error!("Unsafe '..' in ZIP {} after strip", original.display());
                    return Err(SpsError::Generic(format!(
                        "Unsafe '..' in ZIP {}",
                        original.display()
                    )));
                }
                Component::Prefix(_) | Component::RootDir => {
                    error!("Disallowed comp {:?} in ZIP {}", comp, original.display());
                    return Err(SpsError::Generic(format!(
                        "Disallowed comp in ZIP {}",
                        original.display()
                    )));
                }
            }
        }
        if !target_path.starts_with(target_dir) {
            error!(
                "ZIP traversal {} -> {}",
                original.display(),
                target_path.display()
            );
            return Err(SpsError::Generic(format!(
                "ZIP traversal in {}",
                archive_path_for_log.display()
            )));
        }

        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).map_err(|e| {
                    SpsError::Io(std::sync::Arc::new(io::Error::new(
                        e.kind(),
                        format!("Failed create dir {}: {}", parent.display(), e),
                    )))
                })?;
            }
        }

        if file.is_dir() {
            if !target_path.exists() {
                fs::create_dir_all(&target_path).map_err(|e| {
                    SpsError::Io(std::sync::Arc::new(io::Error::new(
                        e.kind(),
                        format!("Failed create dir {}: {}", target_path.display(), e),
                    )))
                })?;
            }
        } else if file.is_symlink() {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            let link_target = PathBuf::from(String::from_utf8_lossy(&buf).to_string());
            #[cfg(unix)]
            {
                if target_path.exists() || target_path.symlink_metadata().is_ok() {
                    let _ = fs::remove_file(&target_path);
                }
                std::os::unix::fs::symlink(&link_target, &target_path).map_err(|e| {
                    debug!(
                        "Failed to create symlink {} -> {}: {}",
                        target_path.display(),
                        link_target.display(),
                        e
                    );
                    SpsError::Io(std::sync::Arc::new(e))
                })?;
            }
            #[cfg(not(unix))]
            {
                debug!(
                    "Cannot create symlink on non-unix system: {} -> {}",
                    target_path.display(),
                    link_target.display()
                );
            }
        } else {
            if target_path.exists() {
                let _ = fs::remove_file(&target_path);
            }
            let mut out_file = File::create(&target_path).map_err(|e| {
                SpsError::Io(std::sync::Arc::new(io::Error::new(
                    e.kind(),
                    format!("Failed create file {}: {}", target_path.display(), e),
                )))
            })?;
            io::copy(&mut file, &mut out_file)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = file.unix_mode() {
                if !file.is_symlink() {
                    fs::set_permissions(&target_path, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }
    debug!(
        "Finished ZIP extraction for {}",
        archive_path_for_log.display()
    );
    // After extraction, quarantine any .app bundles in the stage (macOS only)
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = quarantine_extracted_apps_in_stage(target_dir, "sps-zip-extractor") {
            tracing::warn!(
                "Error during post-zip extraction quarantine scan for {}: {}",
                archive_path_for_log.display(),
                e
            );
        }
    }
    Ok(())
}
