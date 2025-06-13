// sps-core/src/build/formula/macho.rs
// Contains Mach-O specific patching logic for bottle relocation.
// Updated to use MachOFatFile32 and MachOFatFile64 for FAT binary parsing.
// Refactored to separate immutable analysis from mutable patching to fix borrow checker errors.

use std::collections::HashMap;
use std::fs;
use std::io::Write; // Keep for write_patched_buffer
use std::path::Path;
use std::process::{Command as StdCommand, Stdio}; // Keep for codesign

// --- Imports needed for Mach-O patching (macOS only) ---
#[cfg(target_os = "macos")]
use object::{
    self,
    macho::{MachHeader32, MachHeader64}, // Keep for Mach-O parsing
    read::macho::{
        FatArch,
        LoadCommandVariant, // Correct import path
        MachHeader,
        MachOFatFile32,
        MachOFatFile64, // Core Mach-O types + FAT types
        MachOFile,
    },
    Endianness,
    FileKind,
    ReadRef,
};
use sps_common::error::{Result, SpsError};
use tempfile::NamedTempFile;
use tracing::{debug, error};

// --- Platform‑specific constants for Mach‑O magic detection ---
#[cfg(target_os = "macos")]
const MH_MAGIC: u32 = 0xfeedface;
#[cfg(target_os = "macos")]
const MH_MAGIC_64: u32 = 0xfeedfacf;
#[cfg(target_os = "macos")]
const MACHO_HEADER32_SIZE: usize = 28;
#[cfg(target_os = "macos")]
const MACHO_HEADER64_SIZE: usize = 32;

/// Core patch data for **one** string replacement location inside a Mach‑O file.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct PatchInfo {
    absolute_offset: usize, // Offset in the entire file buffer
    allocated_len: usize,   // How much space was allocated for this string
    new_path: String,       // The new string to write
}

/// Container for paths that couldn't be patched due to length constraints
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct SkippedPath {
    pub old_path: String,
    pub new_path: String,
}

/// Main entry point for Mach‑O path patching (macOS only).
/// Returns `Ok(true)` if patches were applied, `Ok(false)` if no patches needed.
/// Returns a tuple: (patched: bool, skipped_paths: Vec<SkippedPath>)
#[cfg(target_os = "macos")]
pub fn patch_macho_file(
    path: &Path,
    replacements: &HashMap<String, String>,
) -> Result<(bool, Vec<SkippedPath>)> {
    patch_macho_file_macos(path, replacements)
}

/// Container for paths that couldn't be patched due to length constraints
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone)]
pub struct SkippedPath {
    pub old_path: String,
    pub new_path: String,
}

/// No‑op stub for non‑macOS platforms.
#[cfg(not(target_os = "macos"))]
pub fn patch_macho_file(
    _path: &Path,
    _replacements: &HashMap<String, String>,
) -> Result<(bool, Vec<SkippedPath>)> {
    Ok((false, Vec::new()))
}

/// **macOS implementation**: Tries to patch Mach‑O files by replacing placeholders.
#[cfg(target_os = "macos")]
fn patch_macho_file_macos(
    path: &Path,
    replacements: &HashMap<String, String>,
) -> Result<(bool, Vec<SkippedPath>)> {
    debug!("Processing potential Mach-O file: {}", path.display());

    // 1) Load the entire file into memory
    let buffer = match fs::read(path) {
        Ok(data) => data,
        Err(e) => {
            debug!("Failed to read {}: {}", path.display(), e);
            return Ok((false, Vec::new()));
        }
    };
    if buffer.is_empty() {
        debug!("Empty file: {}", path.display());
        return Ok((false, Vec::new()));
    }

    // 2) Identify the file type
    let kind = match object::FileKind::parse(&*buffer) {
        Ok(k) => k,
        Err(_e) => {
            debug!("Not an object file: {}", path.display());
            return Ok((false, Vec::new()));
        }
    };

    // 3) **Analysis phase**: collect patches + skipped paths
    let (patches, skipped_paths) = collect_macho_patches(&buffer, kind, replacements, path)?;

    if patches.is_empty() {
        if skipped_paths.is_empty() {
            debug!("No patches needed for {}", path.display());
        } else {
            debug!(
                "No patches applied for {} ({} paths skipped due to length)",
                path.display(),
                skipped_paths.len()
            );
        }
        return Ok((false, skipped_paths));
    }

    // 4) Clone buffer and apply all patches atomically
    let mut patched_buffer = buffer;
    for patch in &patches {
        patch_path_in_buffer(
            &mut patched_buffer,
            patch.absolute_offset,
            patch.allocated_len,
            &patch.new_path,
            path,
        )?;
    }

    // 5) Write atomically
    write_patched_buffer(path, &patched_buffer)?;
    debug!("Wrote patched Mach-O: {}", path.display());

    // 6) Re‑sign on Apple Silicon
    #[cfg(target_arch = "aarch64")]
    {
        resign_binary(path)?;
        debug!("Re‑signed patched binary: {}", path.display());
    }

    Ok((true, skipped_paths))
}

/// ASCII magic for the start of a static `ar` archive  (`!<arch>\n`)
#[cfg(target_os = "macos")]
const AR_MAGIC: &[u8; 8] = b"!<arch>\n";

/// Examine a buffer (Mach‑O or FAT) and return every patch we must apply + skipped paths.
#[cfg(target_os = "macos")]
fn collect_macho_patches(
    buffer: &[u8],
    kind: FileKind,
    replacements: &HashMap<String, String>,
    path_for_log: &Path,
) -> Result<(Vec<PatchInfo>, Vec<SkippedPath>)> {
    let mut patches = Vec::<PatchInfo>::new();
    let mut skipped_paths = Vec::<SkippedPath>::new();

    match kind {
        /* ---------------------------------------------------------- */
        FileKind::MachO32 => {
            let m = MachOFile::<MachHeader32<Endianness>, _>::parse(buffer)?;
            let (mut p, mut s) =
                find_patches_in_commands(&m, 0, MACHO_HEADER32_SIZE, replacements, path_for_log)?;
            patches.append(&mut p);
            skipped_paths.append(&mut s);
        }
        /* ---------------------------------------------------------- */
        FileKind::MachO64 => {
            let m = MachOFile::<MachHeader64<Endianness>, _>::parse(buffer)?;
            let (mut p, mut s) =
                find_patches_in_commands(&m, 0, MACHO_HEADER64_SIZE, replacements, path_for_log)?;
            patches.append(&mut p);
            skipped_paths.append(&mut s);
        }
        /* ---------------------------------------------------------- */
        FileKind::MachOFat32 => {
            let fat = MachOFatFile32::parse(buffer)?;
            for (idx, arch) in fat.arches().iter().enumerate() {
                let (off, sz) = arch.file_range();
                let slice = &buffer[off as usize..(off + sz) as usize];

                /* short‑circuit: static .a archive inside FAT ---------- */
                if slice.starts_with(AR_MAGIC) {
                    debug!("[slice {}] static archive – skipped", idx);
                    continue;
                }

                /* decide 32 / 64 by magic ------------------------------ */
                if slice.len() >= 4 {
                    let magic = u32::from_le_bytes(slice[0..4].try_into().unwrap());
                    if magic == MH_MAGIC_64 {
                        if let Ok(m) = MachOFile::<MachHeader64<Endianness>, _>::parse(slice) {
                            let (mut p, mut s) = find_patches_in_commands(
                                &m,
                                off as usize,
                                MACHO_HEADER64_SIZE,
                                replacements,
                                path_for_log,
                            )?;
                            patches.append(&mut p);
                            skipped_paths.append(&mut s);
                        }
                    } else if magic == MH_MAGIC {
                        if let Ok(m) = MachOFile::<MachHeader32<Endianness>, _>::parse(slice) {
                            let (mut p, mut s) = find_patches_in_commands(
                                &m,
                                off as usize,
                                MACHO_HEADER32_SIZE,
                                replacements,
                                path_for_log,
                            )?;
                            patches.append(&mut p);
                            skipped_paths.append(&mut s);
                        }
                    }
                }
            }
        }
        /* ---------------------------------------------------------- */
        FileKind::MachOFat64 => {
            let fat = MachOFatFile64::parse(buffer)?;
            for (idx, arch) in fat.arches().iter().enumerate() {
                let (off, sz) = arch.file_range();
                let slice = &buffer[off as usize..(off + sz) as usize];

                if slice.starts_with(AR_MAGIC) {
                    debug!("[slice {}] static archive – skipped", idx);
                    continue;
                }

                if slice.len() >= 4 {
                    let magic = u32::from_le_bytes(slice[0..4].try_into().unwrap());
                    if magic == MH_MAGIC_64 {
                        if let Ok(m) = MachOFile::<MachHeader64<Endianness>, _>::parse(slice) {
                            let (mut p, mut s) = find_patches_in_commands(
                                &m,
                                off as usize,
                                MACHO_HEADER64_SIZE,
                                replacements,
                                path_for_log,
                            )?;
                            patches.append(&mut p);
                            skipped_paths.append(&mut s);
                        }
                    } else if magic == MH_MAGIC {
                        if let Ok(m) = MachOFile::<MachHeader32<Endianness>, _>::parse(slice) {
                            let (mut p, mut s) = find_patches_in_commands(
                                &m,
                                off as usize,
                                MACHO_HEADER32_SIZE,
                                replacements,
                                path_for_log,
                            )?;
                            patches.append(&mut p);
                            skipped_paths.append(&mut s);
                        }
                    }
                }
            }
        }
        /* ---------------------------------------------------------- */
        _ => { /* archives & unknown kinds are ignored */ }
    }

    Ok((patches, skipped_paths))
}

/// Iterates through load commands of a parsed MachOFile (slice) and returns
/// patch details + skipped paths.
#[cfg(target_os = "macos")]
fn find_patches_in_commands<'data, Mach, R>(
    macho_file: &MachOFile<'data, Mach, R>,
    slice_base_offset: usize,
    header_size: usize,
    replacements: &HashMap<String, String>,
    file_path_for_log: &Path,
) -> Result<(Vec<PatchInfo>, Vec<SkippedPath>)>
where
    Mach: MachHeader,
    R: ReadRef<'data>,
{
    let endian = macho_file.endian();
    let mut patches = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut cur_off = header_size;

    let mut it = macho_file.macho_load_commands()?;
    while let Some(cmd) = it.next()? {
        let cmd_size = cmd.cmdsize() as usize;
        let cmd_offset = cur_off; // offset *inside this slice*
        cur_off += cmd_size;

        let variant = match cmd.variant() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Malformed load‑command in {}: {}; skipping",
                    file_path_for_log.display(),
                    e
                );
                continue;
            }
        };

        // — which commands carry path strings we might want? —
        let path_info: Option<(u32, &[u8])> = match variant {
            LoadCommandVariant::Dylib(d) | LoadCommandVariant::IdDylib(d) => cmd
                .string(endian, d.dylib.name)
                .ok()
                .map(|bytes| (d.dylib.name.offset.get(endian), bytes)),
            LoadCommandVariant::Rpath(r) => cmd
                .string(endian, r.path)
                .ok()
                .map(|bytes| (r.path.offset.get(endian), bytes)),
            _ => None,
        };

        if let Some((offset_in_cmd, bytes)) = path_info {
            if let Ok(old_path) = std::str::from_utf8(bytes) {
                if let Some(new_path) = find_and_replace_placeholders(old_path, replacements) {
                    let allocated = cmd_size.saturating_sub(offset_in_cmd as usize);

                    if new_path.len() + 1 > allocated {
                        // would overflow – add to skipped paths instead of throwing
                        tracing::debug!(
                            "Skip patch (too long): '{}' → '{}' (alloc {} B) in {}",
                            old_path,
                            new_path,
                            allocated,
                            file_path_for_log.display()
                        );
                        skipped_paths.push(SkippedPath {
                            old_path: old_path.to_string(),
                            new_path: new_path.clone(),
                        });
                        continue;
                    }

                    patches.push(PatchInfo {
                        absolute_offset: slice_base_offset + cmd_offset + offset_in_cmd as usize,
                        allocated_len: allocated,
                        new_path,
                    });
                }
            }
        }
    }
    Ok((patches, skipped_paths))
}

/// Helper to replace placeholders in a string based on the replacements map.
/// Returns `Some(String)` with replacements if any were made, `None` otherwise.
fn find_and_replace_placeholders(
    current_path: &str,
    replacements: &HashMap<String, String>,
) -> Option<String> {
    let mut new_path = current_path.to_string();
    let mut path_modified = false;
    // Iterate through all placeholder/replacement pairs
    for (placeholder, replacement) in replacements {
        // Check if the current path string contains the placeholder
        if new_path.contains(placeholder) {
            // Replace all occurrences of the placeholder
            new_path = new_path.replace(placeholder, replacement);
            path_modified = true; // Mark that a change was made
            debug!(
                "   Replaced '{}' with '{}' -> '{}'",
                placeholder, replacement, new_path
            );
        }
    }
    // Return the modified string only if changes occurred
    if path_modified {
        Some(new_path)
    } else {
        None
    }
}

/// Write a new (null‑padded) path into the mutable buffer.  
/// Assumes the caller already verified the length.
#[cfg(target_os = "macos")]
fn patch_path_in_buffer(
    buf: &mut [u8],
    abs_off: usize,
    alloc_len: usize,
    new_path: &str,
    file: &Path,
) -> Result<()> {
    if new_path.len() + 1 > alloc_len || abs_off + alloc_len > buf.len() {
        // should never happen – just log & skip
        tracing::debug!(
            "Patch skipped (bounds) at {} in {}",
            abs_off,
            file.display()
        );
        return Ok(());
    }

    // null‑padded copy
    buf[abs_off..abs_off + new_path.len()].copy_from_slice(new_path.as_bytes());
    buf[abs_off + new_path.len()..abs_off + alloc_len].fill(0);

    Ok(())
}

/// Writes the patched buffer to the original path atomically using a temporary file.
#[cfg(target_os = "macos")]
fn write_patched_buffer(original_path: &Path, buffer: &[u8]) -> Result<()> {
    // Get the directory containing the original file
    let dir = original_path.parent().ok_or_else(|| {
        SpsError::Generic(format!(
            "Cannot get parent directory for {}",
            original_path.display()
        ))
    })?;
    // Ensure the directory exists
    fs::create_dir_all(dir).map_err(|e| SpsError::Io(std::sync::Arc::new(e)))?;

    // Create a named temporary file in the same directory to facilitate atomic rename
    let mut temp_file = NamedTempFile::new_in(dir)?;
    debug!(
        "    Writing patched buffer ({} bytes) to temporary file: {:?}",
        buffer.len(),
        temp_file.path()
    );
    // Write the entire modified buffer to the temporary file
    temp_file.write_all(buffer)?;
    // Ensure data is flushed to the OS buffer
    temp_file.flush()?;
    // Attempt to sync data to the disk
    temp_file.as_file().sync_all()?; // Ensure data is physically written

    // Atomically replace the original file with the temporary file
    // persist() renames the temp file over the original path.
    temp_file.persist(original_path).map_err(|e| {
        // If persist fails, the temporary file might still exist.
        // The error 'e' contains both the temp file and the underlying IO error.
        error!(
            "    Failed to persist/rename temporary file over {}: {}",
            original_path.display(),
            e.error // Log the underlying IO error
        );
        // Return the IO error wrapped in our error type
        SpsError::Io(std::sync::Arc::new(e.error))
    })?;
    debug!(
        "    Atomically replaced {} with patched version",
        original_path.display()
    );
    Ok(())
}

/// Re-signs the binary using the `codesign` command-line tool.
/// This is typically necessary on Apple Silicon (aarch64) after modifying executables.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn resign_binary(path: &Path) -> Result<()> {
    // Suppressed: debug!("Re-signing patched binary: {}", path.display());
    let status = StdCommand::new("codesign")
        .args([
            "-s",
            "-",
            "--force",
            "--preserve-metadata=identifier,entitlements",
        ])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status() // Execute the command and get its exit status
        .map_err(|e| {
            error!(
                "    Failed to execute codesign command for {}: {}",
                path.display(),
                e
            );
            SpsError::Io(std::sync::Arc::new(e))
        })?;
    if status.success() {
        // Suppressed: debug!("Successfully re-signed {}", path.display());
        Ok(())
    } else {
        error!(
            "    codesign command failed for {} with status: {}",
            path.display(),
            status
        );
        Err(SpsError::CodesignError(format!(
            "Failed to re-sign patched binary {}, it may not be executable. Exit status: {}",
            path.display(),
            status
        )))
    }
}

// No-op stub for resigning on non-Apple Silicon macOS (e.g., x86_64)
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
fn resign_binary(_path: &Path) -> Result<()> {
    // No re-signing typically needed on Intel Macs after ad-hoc patching
    Ok(())
}

// No-op stub for resigning Innovations on non-macOS platforms
#[cfg(not(target_os = "macos"))]
fn resign_binary(_path: &Path) -> Result<()> {
    // Resigning is a macOS concept
    Ok(())
}
