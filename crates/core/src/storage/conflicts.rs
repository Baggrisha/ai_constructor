//! Immutable, content-addressed copies of committed project versions.
//!
//! An index is the exact manifest bytes and is installed only after every blob
//! has been independently copied and verified. Retained live inodes are not
//! immutable and must never be used as baseline blobs.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{atomic, file_bytes, hash, safe_path, scan, sync_dir, Manifest};
use crate::{CoreError, Result};

const BASE: &str = ".aic-local/baseline/v1";
const CHUNK: usize = 64 * 1024;

#[cfg(test)]
thread_local! {
    static BASELINE_INTERRUPT: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn baseline_interrupt(point: u8) -> Result<()> {
    BASELINE_INTERRUPT.with(|slot| {
        if slot.get() == Some(point) {
            slot.set(None);
            Err(CoreError::io("Simulated baseline publication interruption"))
        } else {
            Ok(())
        }
    })
}

fn check_hash(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(CoreError::invalid("Invalid baseline digest"));
    }
    Ok(())
}

// Sync each newly created directory entry before an index can be published.
fn ensure_directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    let mut name = String::new();
    for part in Path::new(relative).components() {
        let part = match part {
            std::path::Component::Normal(part) => part,
            _ => return Err(CoreError::invalid("Invalid baseline directory")),
        };
        if !name.is_empty() {
            name.push('/');
        }
        name.push_str(
            part.to_str()
                .ok_or_else(|| CoreError::invalid("Invalid baseline directory"))?,
        );
        current.push(part);
        safe_path(root, &name)?;
        match fs::create_dir(&current) {
            Ok(()) => {
                sync_dir(
                    current
                        .parent()
                        .ok_or_else(|| CoreError::invalid("Invalid baseline directory"))?,
                )?;
                sync_dir(&current)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&current)?.file_type().is_dir() {
                    return Err(CoreError::invalid("Baseline directory is not a directory"));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(current)
}

fn directories(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let blobs = ensure_directory(root, &format!("{BASE}/blobs"))?;
    let manifests = ensure_directory(root, &format!("{BASE}/manifests"))?;
    Ok((blobs, manifests))
}

fn index_path(root: &Path, bytes: &[u8]) -> Result<PathBuf> {
    safe_path(root, &format!("{BASE}/manifests/{}.json", hash(bytes)))
}

fn blob_path(root: &Path, digest: &str) -> Result<PathBuf> {
    check_hash(digest)?;
    safe_path(root, &format!("{BASE}/blobs/{digest}"))
}

fn checked_file(path: &Path) -> Result<File> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(CoreError::invalid("Baseline source is not a regular file"));
    }
    Ok(File::open(path)?)
}

fn read_chunks(path: &Path) -> Result<Vec<u8>> {
    let mut file = checked_file(path)?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; CHUNK];
    loop {
        super::control::check()?;
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}

fn verify_reader(mut reader: impl Read, expected: &str) -> Result<()> {
    let mut digest = Sha256::new();
    let mut buf = [0u8; CHUNK];
    loop {
        super::control::check()?;
        let count = reader.read(&mut buf)?;
        if count == 0 {
            break;
        }
        digest.update(&buf[..count]);
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err(CoreError::conflict("Baseline blob hash mismatch"));
    }
    Ok(())
}

fn verify_blob(root: &Path, digest: &str) -> Result<bool> {
    let path = blob_path(root, digest)?;
    match checked_file(&path) {
        Ok(file) => {
            verify_reader(file, digest)?;
            Ok(true)
        }
        Err(error) if !path.exists() && error.code == "IO_ERROR" => Ok(false),
        Err(error) => Err(error),
    }
}

fn store_blob(root: &Path, expected: &str, mut reader: impl Read) -> Result<()> {
    check_hash(expected)?;
    if verify_blob(root, expected)? {
        return Ok(());
    }
    let (blobs, _) = directories(root)?;
    let destination = blob_path(root, expected)?;
    let temp = blobs.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let mut digest = Sha256::new();
        let mut buf = [0u8; CHUNK];
        loop {
            super::control::check()?;
            let count = reader.read(&mut buf)?;
            if count == 0 {
                break;
            }
            digest.update(&buf[..count]);
            output.write_all(&buf[..count])?;
        }
        if format!("{:x}", digest.finalize()) != expected {
            return Err(CoreError::conflict("Baseline source changed while copying"));
        }
        output.sync_all()?;
        drop(output);
        match atomic::move_no_replace(&temp, &destination) {
            Ok(()) => sync_dir(&blobs)?,
            Err(error) if destination.exists() => {
                // A second writer may have published the same digest. Never trust
                // its filename without checking the bytes independently.
                if !verify_blob(root, expected)? {
                    return Err(error.into());
                }
                fs::remove_file(&temp)?;
                sync_dir(&blobs)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(test)]
        baseline_interrupt(1)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn validate_manifest(bytes: &[u8], manifest: &Manifest) -> Result<()> {
    let parsed: Manifest = serde_json::from_slice(bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid baseline manifest: {e}")))?;
    // Compare exact serialization-independent fields, including the file map;
    // the exact original byte string remains the index key and value.
    if parsed.schema_version != manifest.schema_version
        || parsed.project_id != manifest.project_id
        || parsed.name != manifest.name
        || parsed.revision != manifest.revision
        || parsed.storage_generation != manifest.storage_generation
        || parsed.snapshot_hash != manifest.snapshot_hash
        || parsed.files != manifest.files
    {
        return Err(CoreError::invalid(
            "Baseline manifest does not match its bytes",
        ));
    }
    for (path, digest) in &manifest.files {
        if path.is_empty()
            || Path::new(path)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(CoreError::invalid("Unsafe baseline path"));
        }
        check_hash(digest)?;
    }
    Ok(())
}

/// Resolution rewrites the manifest; it must not discard fields it cannot model.
pub(super) fn require_supported_manifest(bytes: &[u8], manifest: &Manifest) -> Result<()> {
    if manifest.schema_version != 1 || super::manifest_has_unknown_fields(bytes)? {
        return Err(CoreError::new(
            "UNSUPPORTED_SCHEMA",
            "Project manifest contains an unsupported schema or unknown fields",
        ));
    }
    Ok(())
}

fn publish_index(root: &Path, bytes: &[u8], manifest: &Manifest) -> Result<()> {
    validate_manifest(bytes, manifest)?;
    for digest in manifest.files.values() {
        if !verify_blob(root, digest)? {
            return Err(CoreError::new(
                "BASELINE_UNAVAILABLE",
                "A baseline blob is unavailable",
            ));
        }
    }
    let (_, manifests) = directories(root)?;
    let destination = index_path(root, bytes)?;
    if destination.exists() {
        if fs::read(&destination)? != bytes {
            return Err(CoreError::conflict("Baseline manifest index changed"));
        }
        return Ok(());
    }
    let temp = manifests.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        #[cfg(test)]
        baseline_interrupt(2)?;
        match atomic::move_no_replace(&temp, &destination) {
            Ok(()) => sync_dir(&manifests)?,
            Err(error) if destination.exists() => {
                if fs::read(&destination)? != bytes {
                    return Err(error.into());
                }
                fs::remove_file(&temp)?;
                sync_dir(&manifests)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(test)]
        baseline_interrupt(3)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// An index counts only when its exact manifest and every blob verify.
pub(super) fn baseline_available(root: &Path, bytes: &[u8], manifest: &Manifest) -> Result<bool> {
    validate_manifest(bytes, manifest)?;
    let index = index_path(root, bytes)?;
    match fs::symlink_metadata(&index) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Err(CoreError::invalid("Baseline index is not a regular file")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    if fs::read(index)? != bytes {
        return Err(CoreError::conflict(
            "Baseline index content does not match its name",
        ));
    }
    for digest in manifest.files.values() {
        if !verify_blob(root, digest)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Bootstrap old clean projects, but never infer old contents from a dirty tree.
/// Caller holds the project lock.
pub(super) fn bootstrap(root: &Path, bytes: &[u8], manifest: &Manifest) -> Result<bool> {
    if baseline_available(root, bytes, manifest)? {
        return Ok(true);
    }
    if file_bytes(root, "project.json")?.as_deref() != Some(bytes) || scan(root)? != manifest.files
    {
        return Ok(false);
    }
    for (path, digest) in &manifest.files {
        super::control::check()?;
        if !verify_blob(root, digest)? {
            store_blob(root, digest, checked_file(&safe_path(root, path)?)?)?;
        }
    }
    if file_bytes(root, "project.json")?.as_deref() != Some(bytes) || scan(root)? != manifest.files
    {
        return Err(CoreError::conflict(
            "Project changed while building its baseline",
        ));
    }
    publish_index(root, bytes, manifest)?;
    Ok(true)
}

/// Return a path's confirmed historical bytes, never the current live inode.
pub(super) fn baseline_file(
    root: &Path,
    bytes: &[u8],
    manifest: &Manifest,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    validate_manifest(bytes, manifest)?;
    let Some(digest) = manifest.files.get(path) else {
        return Ok(None);
    };
    // A verified individual blob remains useful even when other old blobs are
    // unavailable and a complete manifest index could not be published.
    if !verify_blob(root, digest)? {
        return Ok(None);
    }
    let data = read_chunks(&blob_path(root, digest)?)?;
    if hash(&data) != *digest {
        return Err(CoreError::conflict("Baseline blob changed while reading"));
    }
    Ok(Some(data))
}

pub(super) fn baseline_document(
    root: &Path,
    bytes: &[u8],
    manifest: &Manifest,
) -> Result<Option<Vec<u8>>> {
    baseline_file(root, bytes, manifest, "document.json")
}

/// Prepare both immutable versions before the active journal is installed.
/// For an explicit external-resolution transaction the old manifest may be
/// unavailable; only the verified target is required, and no old index is forged.
pub(super) fn prepare_baselines(
    root: &Path,
    old_bytes: &[u8],
    old: &Manifest,
    observed: &BTreeMap<String, String>,
    new_bytes: &[u8],
    new: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    validate_manifest(new_bytes, new)?;
    if observed == &old.files && !bootstrap(root, old_bytes, old)? {
        return Err(CoreError::new(
            "BASELINE_UNAVAILABLE",
            "Cannot establish the previous baseline",
        ));
    }
    for (path, digest) in &new.files {
        super::control::check()?;
        if verify_blob(root, digest)? {
            continue;
        }
        if let Some(bytes) = changes.get(path) {
            store_blob(root, digest, Cursor::new(bytes))?;
        } else if observed.get(path) == Some(digest) {
            store_blob(root, digest, checked_file(&safe_path(root, path)?)?)?;
        } else {
            return Err(CoreError::new(
                "BASELINE_UNAVAILABLE",
                format!("No verified bytes for {path}"),
            ));
        }
    }
    publish_index(root, new_bytes, new)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum Version {
    Absent,
    Blob {
        hash: String,
        size: u64,
        #[serde(rename = "archiveRelativePath")]
        archive_relative_path: String,
    },
    Unavailable {
        #[serde(rename = "expectedHash")]
        expected_hash: String,
    },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConflictPath {
    path: String,
    base: Version,
    local: Version,
    disk: Version,
    choices: Vec<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetainedSource {
    path: String,
    source: String,
    hash: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalConflict {
    id: String,
    reason: String,
    kind: String,
    base_manifest_hash: String,
    observed_disk_token: String,
    paths: Vec<ConflictPath>,
    blocked_paths: Vec<String>,
    choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retained_source: Option<RetainedSource>,
}

/// Includes marker absence and all tracked path names/hashes; no timestamp or
/// revision number is used as a substitute for the actual bytes.
pub(super) fn disk_token(
    marker: Option<&[u8]>,
    observed: &BTreeMap<String, String>,
) -> Result<String> {
    let encoded = serde_json::to_vec(&(marker.map(hash), observed))
        .map_err(|e| CoreError::invalid(e.to_string()))?;
    Ok(hash(&encoded))
}

fn copy_archive(
    staging: &Path,
    id: &str,
    name: &str,
    expected: &str,
    mut input: impl Read,
) -> Result<Version> {
    let destination = staging.join(name);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; CHUNK];
    loop {
        super::control::check()?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| CoreError::invalid("Conflict copy is too large"))?;
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err(CoreError::conflict(
            "A conflict version changed while it was copied",
        ));
    }
    output.sync_all()?;
    Ok(Version::Blob {
        hash: expected.to_owned(),
        size,
        archive_relative_path: format!("conflicts/{id}/{name}"),
    })
}

fn archived_base(
    root: &Path,
    staging: &Path,
    id: &str,
    name: &str,
    expected: Option<&String>,
) -> Result<Version> {
    match expected {
        None => Ok(Version::Absent),
        Some(expected) if verify_blob(root, expected)? => copy_archive(
            staging,
            id,
            name,
            expected,
            checked_file(&blob_path(root, expected)?)?,
        ),
        Some(expected) => Ok(Version::Unavailable {
            expected_hash: expected.clone(),
        }),
    }
}

fn archived_bytes(staging: &Path, id: &str, name: &str, bytes: Option<&[u8]>) -> Result<Version> {
    match bytes {
        None => Ok(Version::Absent),
        Some(bytes) => copy_archive(staging, id, name, &hash(bytes), Cursor::new(bytes)),
    }
}

fn archived_disk(
    root: &Path,
    staging: &Path,
    id: &str,
    name: &str,
    path: &str,
    expected: Option<&String>,
) -> Result<Version> {
    match expected {
        None => Ok(Version::Absent),
        Some(expected) => copy_archive(
            staging,
            id,
            name,
            expected,
            checked_file(&safe_path(root, path)?)?,
        ),
    }
}

fn available(version: &Version) -> bool {
    !matches!(version, Version::Unavailable { .. })
}

fn read_archive(root: &Path, id: &str) -> Result<ExternalConflict> {
    check_hash(id)?;
    let metadata = safe_path(root, &format!("conflicts/{id}/metadata.json"))?;
    if !fs::symlink_metadata(&metadata)?.file_type().is_file() {
        return Err(CoreError::invalid("Invalid conflict metadata"));
    }
    let conflict: ExternalConflict = serde_json::from_slice(&fs::read(metadata)?)
        .map_err(|e| CoreError::invalid(format!("Invalid conflict metadata: {e}")))?;
    if conflict.id != id
        || !matches!(conflict.kind.as_str(), "externalChange" | "retainedWrite")
        || (conflict.kind == "retainedWrite") != conflict.retained_source.is_some()
    {
        return Err(CoreError::invalid(
            "Conflict metadata does not match its directory",
        ));
    }
    Ok(conflict)
}

fn retained_hash(path: &Path) -> Result<String> {
    let mut file = checked_file(path)?;
    let mut digest = Sha256::new();
    let mut chunk = [0u8; CHUNK];
    loop {
        super::control::check()?;
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        digest.update(&chunk[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn retained_ack_path(root: &Path, source: &RetainedSource) -> Result<PathBuf> {
    let key = serde_json::to_vec(&(&source.source, &source.hash))
        .map_err(|e| CoreError::invalid(e.to_string()))?;
    safe_path(
        root,
        &format!(".aic-local/retained-acks/{}.json", hash(&key)),
    )
}

fn retained_acknowledged(root: &Path, source: &RetainedSource) -> Result<bool> {
    let path = retained_ack_path(root, source)?;
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {
            let stored: RetainedSource = serde_json::from_slice(&fs::read(path)?).map_err(|e| {
                CoreError::invalid(format!("Invalid retained acknowledgement: {e}"))
            })?;
            Ok(stored.source == source.source
                && stored.hash == source.hash
                && stored.path == source.path)
        }
        Ok(_) => Err(CoreError::invalid("Invalid retained acknowledgement")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn acknowledge_retained(root: &Path, source: &RetainedSource) -> Result<()> {
    let directory = ensure_directory(root, ".aic-local/retained-acks")?;
    let destination = retained_ack_path(root, source)?;
    if retained_acknowledged(root, source)? {
        return Ok(());
    }
    let temp = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        super::durable_write(&temp, &super::json(source)?)?;
        match atomic::move_no_replace(&temp, &destination) {
            Ok(()) => sync_dir(&directory)?,
            Err(e) if destination.exists() => {
                if !retained_acknowledged(root, source)? {
                    return Err(e.into());
                }
                fs::remove_file(&temp)?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn retained_changes(root: &Path) -> Result<Vec<RetainedSource>> {
    let directory = safe_path(root, ".aic-local/retained")?;
    let transactions = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    for transaction in transactions {
        let transaction = transaction?;
        if !transaction.file_type()?.is_dir() {
            continue;
        }
        let tx_name = transaction
            .file_name()
            .to_str()
            .ok_or_else(|| CoreError::invalid("Non-UTF-8 retained transaction"))?
            .to_owned();
        let journal_path = safe_path(root, &format!(".aic-local/retained/{tx_name}/journal.json"))?;
        let journal: super::Journal = match fs::read(journal_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::invalid(format!("Invalid retained journal: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let mut inspect =
            |dir_name: String, path: &str, old: Option<&str>, new: Option<&str>| -> Result<()> {
                safe_path(root, path)?;
                let relative = format!(".aic-local/retained/{tx_name}/{dir_name}");
                let slots = match fs::read_dir(safe_path(root, &relative)?) {
                    Ok(slots) => slots,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(e.into()),
                };
                for slot in slots {
                    let slot = slot?;
                    if !slot.file_type()?.is_file() {
                        return Err(CoreError::invalid("Invalid retained inode"));
                    }
                    let name = slot
                        .file_name()
                        .to_str()
                        .ok_or_else(|| CoreError::invalid("Non-UTF-8 retained inode"))?
                        .to_owned();
                    let source_path = format!("{relative}/{name}");
                    let digest = retained_hash(&safe_path(root, &source_path)?)?;
                    if Some(digest.as_str()) != old && Some(digest.as_str()) != new {
                        let source = RetainedSource {
                            path: path.into(),
                            source: source_path,
                            hash: digest,
                        };
                        if !retained_acknowledged(root, &source)? {
                            result.push(source);
                        }
                    }
                }
                Ok(())
            };
        for (index, entry) in journal.entries.iter().enumerate() {
            inspect(
                format!("entry-{index}"),
                &entry.path,
                entry.old_hash.as_deref(),
                entry.new_hash.as_deref(),
            )?;
        }
        let old_marker = journal
            .observed_marker
            .as_ref()
            .map(|marker| marker.hash.as_deref())
            .unwrap_or(Some(journal.old_manifest_hash.as_str()));
        inspect(
            "manifest".into(),
            "project.json",
            old_marker,
            Some(&journal.new_manifest_hash),
        )?;
    }
    result.sort_by(|a, b| a.source.cmp(&b.source).then(a.hash.cmp(&b.hash)));
    Ok(result)
}

fn matching_archives(root: &Path, base_hash: &str, token: &str) -> Result<Vec<serde_json::Value>> {
    let directory = safe_path(root, "conflicts")?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if check_hash(&id).is_err() {
            continue;
        }
        let metadata = safe_path(root, &format!("conflicts/{id}/metadata.json"))?;
        let bytes = fs::read(metadata)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid conflict metadata: {e}")))?;
        if !matches!(
            value.get("kind").and_then(serde_json::Value::as_str),
            Some("externalChange" | "retainedWrite")
        ) {
            continue;
        }
        let archive = read_archive(root, &id)?;
        if let Some(source) = &archive.retained_source {
            if retained_acknowledged(root, source)? {
                continue;
            }
            let source_path = safe_path(root, &source.source)?;
            if !source_path.is_file() || retained_hash(&source_path)? != source.hash {
                continue;
            }
        }
        if archive.base_manifest_hash == base_hash && archive.observed_disk_token == token {
            result.push(
                serde_json::to_value(archive).map_err(|e| CoreError::invalid(e.to_string()))?,
            );
        }
    }
    result.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(result)
}

/// Called with both the state mutex and the project write lock held.
pub(super) fn external_diagnostics(
    root: &Path,
    state: &crate::State,
    observed: &BTreeMap<String, String>,
    marker: Option<&[u8]>,
) -> Result<(String, Vec<serde_json::Value>)> {
    let token = disk_token(marker, observed)?;
    capture_external(root, state, observed, marker, false, None)?;
    for source in retained_changes(root)? {
        capture_external(root, state, observed, marker, true, Some(&source))?;
    }
    Ok((
        token.clone(),
        matching_archives(root, &hash(&state.manifest_bytes), &token)?,
    ))
}

/// A failed save may leave L in memory even if an external writer has already
/// returned disk to B. Persist L once rather than guessing that it was saved.
pub(super) fn archive_failed(
    root: &Path,
    state: &crate::State,
    observed: &BTreeMap<String, String>,
    marker: Option<&[u8]>,
) -> Result<()> {
    capture_external(root, state, observed, marker, true, None)
}

// The completed archive is immutable; incomplete staging is never selectable.
fn capture_external(
    root: &Path,
    state: &crate::State,
    observed: &BTreeMap<String, String>,
    marker: Option<&[u8]>,
    force: bool,
    retained: Option<&RetainedSource>,
) -> Result<()> {
    let base: Manifest = serde_json::from_slice(&state.manifest_bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid session manifest: {e}")))?;
    let token = disk_token(marker, observed)?;
    if !force && observed == &base.files && marker == Some(state.manifest_bytes.as_slice()) {
        return Ok(());
    }

    let local_document = if state.dirty {
        Some(super::json(&state.document)?)
    } else {
        None
    };
    let mut local: BTreeMap<&str, &[u8]> = state
        .pending
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    if let Some(bytes) = &local_document {
        local.insert("document.json", bytes);
    }
    let local_marker = if state.manifest.name == base.name {
        state.manifest_bytes.clone()
    } else {
        let mut next = base.clone();
        next.name = state.manifest.name.clone();
        super::json(&next)?
    };
    let mut paths: std::collections::BTreeSet<String> =
        base.files.keys().chain(observed.keys()).cloned().collect();
    paths.extend(local.keys().map(|path| (*path).to_owned()));
    paths.insert("project.json".into());
    if let Some(source) = retained {
        paths.insert(source.path.clone());
    }
    let mut descriptions = Vec::new();
    for path in paths {
        let b = if path == "project.json" {
            Some(hash(&state.manifest_bytes))
        } else {
            base.files.get(&path).cloned()
        };
        let l = if path == "project.json" {
            Some(hash(&local_marker))
        } else {
            local
                .get(path.as_str())
                .map(|v| hash(v))
                .or_else(|| b.clone())
        };
        let d = if retained.is_some_and(|source| source.path == path) {
            retained.map(|source| source.hash.clone())
        } else if path == "project.json" {
            marker.map(hash)
        } else {
            observed.get(&path).cloned()
        };
        if b != l || b != d {
            descriptions.push((path, b, l, d));
        }
    }
    if descriptions.is_empty() {
        return Ok(());
    }
    let id_data =
        serde_json::to_vec(&(hash(&state.manifest_bytes), &token, &descriptions, retained))
            .map_err(|e| CoreError::invalid(e.to_string()))?;
    let id = hash(&id_data);
    let final_dir = safe_path(root, &format!("conflicts/{id}"))?;
    if final_dir.exists() {
        let existing = read_archive(root, &id)?;
        if existing.base_manifest_hash != hash(&state.manifest_bytes)
            || existing.observed_disk_token != token
        {
            return Err(CoreError::conflict(
                "Existing conflict archive does not match its content",
            ));
        }
        return Ok(());
    }

    let local_dir = safe_path(root, ".aic-local")?;
    let staging = local_dir.join(format!("conflict-{}.tmp", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    sync_dir(&local_dir)?;
    let result = (|| -> Result<ExternalConflict> {
        // Every archive contains the exact confirmed manifest even when the
        // external marker itself was deleted or became invalid.
        super::durable_write(&staging.join("base-manifest"), &state.manifest_bytes)?;
        let mut files = Vec::new();
        let mut blocked = Vec::new();
        for (index, (path, b, _l, d)) in descriptions.iter().enumerate() {
            super::control::check()?;
            let base_version = if path == "project.json" {
                archived_bytes(
                    &staging,
                    &id,
                    &format!("base-{index}"),
                    Some(&state.manifest_bytes),
                )?
            } else {
                archived_base(root, &staging, &id, &format!("base-{index}"), b.as_ref())?
            };
            let local_version = if path == "project.json" {
                archived_bytes(
                    &staging,
                    &id,
                    &format!("local-{index}"),
                    Some(&local_marker),
                )?
            } else if let Some(bytes) = local.get(path.as_str()) {
                archived_bytes(&staging, &id, &format!("local-{index}"), Some(bytes))?
            } else {
                archived_base(root, &staging, &id, &format!("local-{index}"), b.as_ref())?
            };
            let disk_version = if let Some(source) = retained.filter(|source| source.path == *path)
            {
                copy_archive(
                    &staging,
                    &id,
                    &format!("disk-{index}"),
                    &source.hash,
                    checked_file(&safe_path(root, &source.source)?)?,
                )?
            } else if path == "project.json" {
                archived_bytes(&staging, &id, &format!("disk-{index}"), marker)?
            } else {
                archived_disk(
                    root,
                    &staging,
                    &id,
                    &format!("disk-{index}"),
                    path,
                    d.as_ref(),
                )?
            };
            let mut choices = Vec::new();
            for (name, value) in [
                ("base", &base_version),
                ("local", &local_version),
                ("disk", &disk_version),
            ] {
                if available(value) && !(path == "project.json" && matches!(value, Version::Absent))
                {
                    choices.push(name.to_owned());
                }
            }
            if path.starts_with("src/generated/") && b != d {
                blocked.push(path.clone());
            }
            files.push(ConflictPath {
                path: path.clone(),
                base: base_version,
                local: local_version,
                disk: disk_version,
                choices,
            });
        }
        let archive = ExternalConflict {
            id: id.clone(),
            reason: if retained.is_some() {
                "External write to a retained file".into()
            } else {
                "Project files changed outside the editor".into()
            },
            kind: if retained.is_some() {
                "retainedWrite".into()
            } else {
                "externalChange".into()
            },
            base_manifest_hash: hash(&state.manifest_bytes),
            observed_disk_token: token.clone(),
            paths: files,
            blocked_paths: blocked,
            choices: vec!["base".into(), "local".into(), "disk".into()],
            retained_source: retained.cloned(),
        };
        super::durable_write(&staging.join("metadata.json"), &super::json(&archive)?)?;
        sync_dir(&staging)?;
        if file_bytes(root, "project.json")?.as_deref() != marker || scan(root)? != *observed {
            return Err(CoreError::conflict(
                "Project changed while archiving external versions",
            ));
        }
        if let Some(source) = retained {
            if retained_hash(&safe_path(root, &source.source)?)? != source.hash {
                return Err(CoreError::conflict("Retained file changed while archiving"));
            }
        }
        super::control::begin_commit()?;
        let conflicts = safe_path(root, "conflicts")?;
        match fs::create_dir(&conflicts) {
            Ok(()) => sync_dir(root)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&conflicts)?.file_type().is_dir() {
                    return Err(CoreError::invalid("Conflict directory is not a directory"));
                }
            }
            Err(e) => return Err(e.into()),
        }
        match atomic::move_no_replace(&staging, &final_dir) {
            Ok(()) => {
                sync_dir(&local_dir)?;
                sync_dir(&conflicts)?;
            }
            Err(_) if final_dir.exists() => {
                let existing = read_archive(root, &id)?;
                if existing.base_manifest_hash != archive.base_manifest_hash
                    || existing.observed_disk_token != archive.observed_disk_token
                {
                    return Err(CoreError::conflict(
                        "Existing conflict archive is inconsistent",
                    ));
                }
                fs::remove_dir_all(&staging)?;
            }
            Err(e) => return Err(e.into()),
        }
        Ok(archive)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GeneratedBlock {
    path: String,
    hash: String,
}

const GENERATED_BLOCKS: &str = "history/generated-blocks/";

pub(crate) fn generated_block(root: &Path, path: &str, digest: &str) -> Result<(String, Vec<u8>)> {
    if !path.starts_with("src/generated/") {
        return Err(CoreError::invalid("Invalid generated source path"));
    }
    safe_path(root, path)?;
    check_hash(digest)?;
    let name = format!("{GENERATED_BLOCKS}{}-{digest}.json", hash(path.as_bytes()));
    let bytes = super::json(&GeneratedBlock {
        path: path.into(),
        hash: digest.into(),
    })?;
    Ok((name, bytes))
}

pub(super) fn legacy_generated_blocks(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let directory = safe_path(root, ".aic-local/blocked-generated")?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    let mut blocks = BTreeMap::new();
    for entry in entries {
        super::control::check()?;
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err(CoreError::invalid("Invalid generated source block"));
        }
        let block: GeneratedBlock = serde_json::from_slice(&fs::read(entry.path())?)
            .map_err(|e| CoreError::invalid(format!("Invalid generated source block: {e}")))?;
        let (portable_path, bytes) = generated_block(root, &block.path, &block.hash)?;
        if portable_path.strip_prefix(GENERATED_BLOCKS) != Some(name.as_str()) {
            return Err(CoreError::invalid("Invalid generated source block"));
        }
        blocks.insert(portable_path, bytes);
    }
    Ok(blocks)
}

/// The exact externally supplied bytes remain protected until explicitly resolved.
pub(super) fn blocked_generated(root: &Path, manifest: &Manifest) -> Result<Vec<String>> {
    let mut paths = std::collections::BTreeSet::new();
    let mut blocks = legacy_generated_blocks(root)?;
    for (path, expected) in &manifest.files {
        if !path.starts_with(GENERATED_BLOCKS) {
            continue;
        }
        let bytes =
            match file_bytes(root, path)? {
                Some(bytes) if hash(&bytes) == *expected => bytes,
                _ => baseline_file(root, &super::json(manifest)?, manifest, path)?.ok_or_else(
                    || CoreError::conflict(format!("Changed generated source block {path}")),
                )?,
            };
        blocks.insert(path.clone(), bytes);
    }
    for (path, bytes) in blocks {
        let block: GeneratedBlock = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid generated source block: {e}")))?;
        let (expected, _) = generated_block(root, &block.path, &block.hash)?;
        if path != expected {
            return Err(CoreError::invalid("Invalid generated source block"));
        }
        if manifest.files.get(&block.path) == Some(&block.hash) {
            paths.insert(block.path);
        }
    }
    Ok(paths.into_iter().collect())
}

fn unique_baseline(root: &Path) -> Result<Option<super::Loaded>> {
    let directory = safe_path(root, &format!("{BASE}/manifests"))?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut found: Option<super::Loaded> = None;
    for entry in entries {
        super::control::check()?;
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(digest) = name.strip_suffix(".json") else {
            continue;
        };
        if check_hash(digest).is_err() {
            continue;
        }
        let path = safe_path(root, &format!("{BASE}/manifests/{name}"))?;
        let bytes = fs::read(path)?;
        if hash(&bytes) != digest {
            return Err(CoreError::conflict("Baseline index has changed"));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid baseline index: {e}")))?;
        if !baseline_available(root, &bytes, &manifest)? {
            continue;
        }
        let Some(document) = baseline_document(root, &bytes, &manifest)? else {
            continue;
        };
        if let Some(existing) = &found {
            if existing.manifest_bytes != bytes {
                return Err(CoreError::new(
                    "BASELINE_UNAVAILABLE",
                    "Several verified baselines exist; choose a recovery copy",
                ));
            }
        } else {
            found = Some(super::Loaded {
                manifest, manifest_bytes: bytes, document_bytes: document,
                read_only_reason: Some("Project files were changed outside the editor; open read-only to avoid overwriting them".into()),
            });
        }
    }
    Ok(found)
}

/// Recover the single unambiguous trusted base whose immutable conflict archive
/// captured the currently observed marker and tree. Never guess among bases.
pub(super) fn archived_base_for_disk(
    root: &Path,
    observed: &BTreeMap<String, String>,
    marker: Option<&[u8]>,
) -> Result<Option<super::Loaded>> {
    let token = disk_token(marker, observed)?;
    let conflicts = safe_path(root, "conflicts")?;
    let entries = match fs::read_dir(conflicts) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return unique_baseline(root),
        Err(e) => return Err(e.into()),
    };
    let mut found: Option<super::Loaded> = None;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if check_hash(&id).is_err() {
            continue;
        }
        let metadata = safe_path(root, &format!("conflicts/{id}/metadata.json"))?;
        let value: serde_json::Value = serde_json::from_slice(&fs::read(metadata)?)
            .map_err(|e| CoreError::invalid(format!("Invalid conflict metadata: {e}")))?;
        if value.get("kind").and_then(serde_json::Value::as_str) != Some("externalChange") {
            continue;
        }
        let archive = read_archive(root, &id)?;
        if archive.observed_disk_token != token {
            continue;
        }
        let bytes = fs::read(safe_path(root, &format!("conflicts/{id}/base-manifest"))?)?;
        if hash(&bytes) != archive.base_manifest_hash {
            return Err(CoreError::conflict("Archived base manifest has changed"));
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid archived base manifest: {e}")))?;
        let document = match baseline_document(root, &bytes, &manifest)? {
            Some(bytes) => bytes,
            None if observed.get("document.json") == manifest.files.get("document.json") => {
                let Some(document) = file_bytes(root, "document.json")? else {
                    continue;
                };
                if Some(hash(&document)) != manifest.files.get("document.json").cloned() {
                    continue;
                }
                document
            }
            None => continue,
        };
        if let Some(existing) = &found {
            if existing.manifest_bytes != bytes {
                return Err(CoreError::new(
                    "BASELINE_UNAVAILABLE",
                    "Several archived project baselines match the disk; choose a recovery copy",
                ));
            }
        } else {
            found = Some(super::Loaded {
                manifest, manifest_bytes: bytes, document_bytes: document,
                read_only_reason: Some("Project files were changed outside the editor; open read-only to avoid overwriting them".into()),
            });
        }
    }
    match found {
        Some(loaded) => Ok(Some(loaded)),
        None => unique_baseline(root),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolutionRequest {
    schema_version: u32,
    conflict_id: String,
    expected_disk_token: String,
    expected_local_generation: u64,
    selections: Vec<Selection>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    path: String,
    take: String,
}

fn version_hash(version: &Version) -> Option<&str> {
    match version {
        Version::Absent => None,
        Version::Blob { hash, .. } => Some(hash),
        Version::Unavailable { expected_hash } => Some(expected_hash),
    }
}

fn archived_version(
    root: &Path,
    id: &str,
    index: usize,
    take: &str,
    version: &Version,
) -> Result<Option<Vec<u8>>> {
    match version {
        Version::Absent => Ok(None),
        Version::Unavailable { .. } => Err(CoreError::new(
            "BASELINE_UNAVAILABLE",
            "The selected historical bytes are unavailable",
        )),
        Version::Blob {
            hash: expected,
            size,
            archive_relative_path,
        } => {
            let expected_path = format!("conflicts/{id}/{take}-{index}");
            if archive_relative_path != &expected_path {
                return Err(CoreError::invalid("Invalid conflict archive path"));
            }
            check_hash(expected)?;
            let path = safe_path(root, archive_relative_path)?;
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() || metadata.len() != *size {
                return Err(CoreError::conflict(
                    "Conflict archive copy is missing or changed",
                ));
            }
            let bytes = read_chunks(&path)?;
            if hash(&bytes) != *expected {
                return Err(CoreError::conflict(
                    "Conflict archive copy is missing or changed",
                ));
            }
            Ok(Some(bytes))
        }
    }
}

fn candidate_bytes<'a>(
    root: &Path,
    files: &BTreeMap<String, String>,
    changes: &'a BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<Option<std::borrow::Cow<'a, [u8]>>> {
    let Some(expected) = files.get(path) else {
        return Ok(None);
    };
    let bytes = if let Some(bytes) = changes.get(path) {
        std::borrow::Cow::Borrowed(bytes.as_slice())
    } else {
        std::borrow::Cow::Owned(
            file_bytes(root, path)?
                .ok_or_else(|| CoreError::conflict(format!("Missing selected file {path}")))?,
        )
    };
    if hash(&bytes) != *expected {
        return Err(CoreError::conflict(format!(
            "Selected file changed: {path}"
        )));
    }
    Ok(Some(bytes))
}

impl crate::ProjectSession {
    /// Select versions from one previously completed immutable conflict archive.
    /// The bridge scopes one OperationControl around this entire call.
    pub fn resolve_external_conflict(&self, request_json: &str) -> Result<String> {
        let request: ResolutionRequest = serde_json::from_str(request_json)
            .map_err(|e| CoreError::invalid(format!("Invalid resolution request: {e}")))?;
        if request.schema_version != 1 {
            return Err(CoreError::new(
                "UNSUPPORTED_SCHEMA",
                "Unsupported conflict resolution schema",
            ));
        }
        check_hash(&request.conflict_id)?;
        let mut state = self.state()?;
        if request.expected_local_generation != state.local_generation {
            return Err(CoreError::conflict(
                "Local changes no longer match the conflict",
            ));
        }
        let base: Manifest = serde_json::from_slice(&state.manifest_bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid session manifest: {e}")))?;
        require_supported_manifest(&state.manifest_bytes, &base)?;
        let mut choices = BTreeMap::new();
        for selection in request.selections {
            if !matches!(selection.take.as_str(), "base" | "local" | "disk")
                || choices.insert(selection.path, selection.take).is_some()
            {
                return Err(CoreError::invalid(
                    "Invalid or duplicate conflict selection",
                ));
            }
        }

        let (archive, observed, marker) = {
            let _lock = super::lock(&self.path)?;
            let observed = scan(&self.path)?;
            let marker = file_bytes(&self.path, "project.json")?;
            let token = disk_token(marker.as_deref(), &observed)?;
            if token != request.expected_disk_token {
                return Err(CoreError::conflict(
                    "Disk changed since the conflict was inspected",
                ));
            }
            let archive = read_archive(&self.path, &request.conflict_id)?;
            if archive.base_manifest_hash != hash(&state.manifest_bytes)
                || archive.observed_disk_token != token
            {
                return Err(CoreError::conflict(
                    "Conflict no longer matches this project",
                ));
            }
            if let Some(source) = &archive.retained_source {
                if !source.source.starts_with(".aic-local/retained/") {
                    return Err(CoreError::invalid("Invalid retained source"));
                }
                check_hash(&source.hash)?;
                if retained_acknowledged(&self.path, source)?
                    || retained_hash(&safe_path(&self.path, &source.source)?)? != source.hash
                {
                    return Err(CoreError::conflict(
                        "Retained file changed since it was inspected",
                    ));
                }
            }
            (archive, observed, marker)
        };

        if choices.len() != archive.paths.len() {
            return Err(CoreError::invalid(
                "Choose one version for every conflict path",
            ));
        }
        let mut described = std::collections::BTreeSet::new();
        let mut descriptions = Vec::new();
        for entry in &archive.paths {
            safe_path(&self.path, &entry.path)?;
            if !described.insert(entry.path.clone())
                || !(entry.path == "project.json"
                    || entry.path == "document.json"
                    || entry.path == ".gitignore"
                    || [
                        "src/",
                        "assets/",
                        "chats/",
                        "agent/",
                        "history/",
                        "checkpoints/",
                    ]
                    .iter()
                    .any(|prefix| entry.path.starts_with(prefix)))
            {
                return Err(CoreError::invalid("Invalid path in conflict archive"));
            }
            let expected_base = if entry.path == "project.json" {
                Some(hash(&state.manifest_bytes))
            } else {
                base.files.get(&entry.path).cloned()
            };
            let expected_disk = if let Some(source) = archive
                .retained_source
                .as_ref()
                .filter(|source| source.path == entry.path)
            {
                Some(source.hash.clone())
            } else if entry.path == "project.json" {
                marker.as_deref().map(hash)
            } else {
                observed.get(&entry.path).cloned()
            };
            if version_hash(&entry.base) != expected_base.as_deref()
                || version_hash(&entry.disk) != expected_disk.as_deref()
            {
                return Err(CoreError::conflict(
                    "Conflict versions do not match the baseline and disk",
                ));
            }
            descriptions.push((
                entry.path.clone(),
                expected_base,
                version_hash(&entry.local).map(str::to_owned),
                expected_disk,
            ));
        }
        let identity = serde_json::to_vec(&(
            hash(&state.manifest_bytes),
            &request.expected_disk_token,
            &descriptions,
            archive.retained_source.as_ref(),
        ))
        .map_err(|e| CoreError::invalid(e.to_string()))?;
        if hash(&identity) != archive.id {
            return Err(CoreError::conflict(
                "Conflict archive contents do not match its identity",
            ));
        }
        if let Some(source) = &archive.retained_source {
            if !described.contains(&source.path) {
                return Err(CoreError::invalid(
                    "Conflict does not include the retained path",
                ));
            }
        }
        for path in base.files.keys().chain(observed.keys()) {
            if base.files.get(path) != observed.get(path) && !described.contains(path) {
                return Err(CoreError::conflict(
                    "Conflict does not include every external change",
                ));
            }
        }
        if marker.as_deref() != Some(state.manifest_bytes.as_slice())
            && !described.contains("project.json")
        {
            return Err(CoreError::conflict(
                "Conflict does not include the changed project marker",
            ));
        }

        // A previous archive can recover pending L after restart, but it must
        // never silently replace newer in-memory edits in an active session.
        if state.dirty || !state.pending.is_empty() {
            for entry in &archive.paths {
                let current = if entry.path == "project.json" {
                    let mut local = base.clone();
                    local.name = state.manifest.name.clone();
                    if local.name == base.name {
                        hash(&state.manifest_bytes)
                    } else {
                        hash(&super::json(&local)?)
                    }
                } else if entry.path == "document.json" && state.dirty {
                    hash(&super::json(&state.document)?)
                } else if let Some(bytes) = state.pending.get(&entry.path) {
                    hash(bytes)
                } else if let Some(hash) = base.files.get(&entry.path) {
                    hash.clone()
                } else {
                    String::new()
                };
                if version_hash(&entry.local).unwrap_or("") != current {
                    return Err(CoreError::conflict(
                        "Local changes differ from the selected conflict archive",
                    ));
                }
            }
            for (path, bytes) in &state.pending {
                if base.files.get(path).map(String::as_str) != Some(hash(bytes).as_str())
                    && !described.contains(path)
                {
                    return Err(CoreError::conflict(
                        "Conflict does not include all pending changes",
                    ));
                }
            }
        }

        let mut files = observed.clone();
        let mut changes = BTreeMap::new();
        let mut deletions = std::collections::BTreeSet::new();
        let mut next_manifest = base.clone();
        let mut generated_blocks = Vec::new();
        for (index, entry) in archive.paths.iter().enumerate() {
            super::control::check()?;
            let take = choices
                .get(&entry.path)
                .ok_or_else(|| CoreError::invalid("Missing conflict selection"))?;
            if !entry.choices.contains(take) {
                return Err(CoreError::new(
                    "BASELINE_UNAVAILABLE",
                    "The selected version is unavailable",
                ));
            }
            let version = match take.as_str() {
                "base" => &entry.base,
                "local" => &entry.local,
                "disk" => &entry.disk,
                _ => return Err(CoreError::invalid("Invalid conflict selection")),
            };
            if !available(version) {
                return Err(CoreError::new(
                    "BASELINE_UNAVAILABLE",
                    "The selected version is unavailable",
                ));
            }
            if entry.path == "project.json" {
                let bytes = archived_version(&self.path, &archive.id, index, take, version)?
                    .ok_or_else(|| CoreError::invalid("Project manifest cannot be deleted"))?;
                let selected: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
                    CoreError::invalid(format!("Invalid selected project manifest: {e}"))
                })?;
                require_supported_manifest(&bytes, &selected)?;
                if selected.project_id != base.project_id || selected.name.trim().is_empty() {
                    return Err(CoreError::invalid(
                        "Selected project manifest belongs to a different project or has no name",
                    ));
                }
                next_manifest.name = selected.name;
                continue;
            }
            let selected_hash = version_hash(version);
            if entry.path.starts_with("src/generated/")
                && selected_hash == version_hash(&entry.disk)
                && selected_hash != version_hash(&entry.base)
            {
                if let Some(digest) = selected_hash {
                    generated_blocks.push((entry.path.clone(), digest.to_owned()));
                }
            }
            if selected_hash == observed.get(&entry.path).map(String::as_str) {
                continue;
            }
            match archived_version(&self.path, &archive.id, index, take, version)? {
                Some(bytes) => {
                    files.insert(entry.path.clone(), hash(&bytes));
                    changes.insert(entry.path.clone(), bytes);
                }
                None => {
                    files.remove(&entry.path);
                    deletions.insert(entry.path.clone());
                }
            }
        }

        let document_bytes = candidate_bytes(&self.path, &files, &changes, "document.json")?
            .ok_or_else(|| CoreError::invalid("Project document cannot be deleted"))?;
        let mut document: serde_json::Value = serde_json::from_slice(&document_bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid selected document: {e}")))?;
        drop(document_bytes);
        let mut model: crate::model::Document =
            serde_json::from_value(document.clone()).map_err(|e| {
                CoreError::new(
                    "UNSUPPORTED_SCHEMA",
                    format!("Selected document is not supported: {e}"),
                )
            })?;
        model.validate()?;
        if !model.editable() || model.project_id != base.project_id {
            return Err(CoreError::new(
                "UNSUPPORTED_SCHEMA",
                "Selected document is not an editable document for this project",
            ));
        }
        let before = state.model()?;
        before.validate()?;
        if before.project_id != base.project_id {
            return Err(CoreError::invalid(
                "Local document belongs to another project",
            ));
        }
        if model.file_history_version.is_none() && before.file_history_version.is_some() {
            model.file_history_version = before.file_history_version;
            document =
                serde_json::to_value(&model).map_err(|e| CoreError::invalid(e.to_string()))?;
        }
        let local_dependencies_changed = state.pending.iter().any(|(path, bytes)| {
            (path.starts_with("src/") || path.starts_with("assets/"))
                && files.get(path).map(String::as_str) != Some(hash(bytes).as_str())
        });
        let design_changed = document != state.document
            || local_dependencies_changed
            || base.files.keys().chain(files.keys()).any(|path| {
                (path == "document.json" || path.starts_with("src/") || path.starts_with("assets/"))
                    && base.files.get(path) != files.get(path)
            });
        if design_changed {
            let revision = base
                .revision
                .max(model.revision)
                .max(before.revision)
                .max(state.manifest.revision)
                .checked_add(1)
                .ok_or_else(|| CoreError::invalid("Document revision overflow"))?;
            document["revision"] = serde_json::json!(revision);
            model = serde_json::from_value(document.clone())
                .map_err(|e| CoreError::invalid(e.to_string()))?;
            model.validate()?;
            let bytes = super::json(&document)?;
            files.insert("document.json".into(), hash(&bytes));
            changes.insert("document.json".into(), bytes);
        } else if model.revision != base.revision {
            return Err(CoreError::conflict(
                "Document revision does not match the baseline",
            ));
        }
        model.validate_files(&files)?;
        next_manifest.revision = model.revision;
        super::services::validate_candidate(&self.path, &files, &changes, base.project_id)?;

        let mut history = match candidate_bytes(&self.path, &files, &changes, "history/state.json")?
        {
            Some(bytes) => {
                let history: crate::History = serde_json::from_slice(&bytes)
                    .map_err(|e| CoreError::invalid(format!("Invalid selected history: {e}")))?;
                for item in history.undo.iter().chain(history.redo.iter()) {
                    item.validate()?;
                    if item.project_id != base.project_id {
                        return Err(CoreError::invalid("History belongs to another project"));
                    }
                }
                history
            }
            None => crate::History::default(),
        };
        if design_changed {
            let transaction_id = Uuid::new_v4();
            history.undo.push(before.clone());
            history.redo.clear();
            history.redo_files.clear();
            history.transaction_ids.insert(transaction_id);
            let dependencies_changed = local_dependencies_changed
                || base.files.keys().chain(files.keys()).any(|path| {
                    (path.starts_with("src/") || path.starts_with("assets/"))
                        && base.files.get(path) != files.get(path)
                });
            if dependencies_changed {
                // Undo stores documents, not external files. Do not promise it
                // can restore dependencies that were replaced or unavailable.
                history.file_restore_barrier = Some(history.undo.len());
            }
            let history_bytes = super::json(&history)?;
            files.insert("history/state.json".into(), hash(&history_bytes));
            changes.insert("history/state.json".into(), history_bytes);
            deletions.remove("history/state.json");
            let transaction_path = format!("history/transactions/{transaction_id}.json");
            let transaction = super::json(&serde_json::json!({
                "transactionId": transaction_id,
                "kind": "externalResolution",
                "conflictId": archive.id,
                "before": before,
                "after": model,
            }))?;
            files.insert(transaction_path.clone(), hash(&transaction));
            changes.insert(transaction_path, transaction);
        }
        for (path, digest) in generated_blocks {
            let (block_path, bytes) = generated_block(&self.path, &path, &digest)?;
            files.insert(block_path.clone(), hash(&bytes));
            deletions.remove(&block_path);
            changes.insert(block_path, bytes);
        }
        for path in blocked_generated(&self.path, &base)? {
            if files.get(&path) == base.files.get(&path) {
                let digest = base
                    .files
                    .get(&path)
                    .ok_or_else(|| CoreError::invalid("Missing blocked generated file"))?;
                let (record, bytes) = generated_block(&self.path, &path, digest)?;
                if files.get(&record) != Some(&hash(&bytes)) {
                    return Err(CoreError::conflict(
                        "Cannot remove protection while the generated file is unchanged",
                    ));
                }
            }
        }
        if let Some(source) = &archive.retained_source {
            if retained_hash(&safe_path(&self.path, &source.source)?)? != source.hash {
                return Err(CoreError::conflict(
                    "Retained file changed before resolution",
                ));
            }
        }
        let next_generation = state
            .local_generation
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Local state generation overflow"))?;
        let committed = super::commit_resolution(
            &self.path,
            &state.manifest_bytes,
            &next_manifest,
            &changes,
            &deletions,
            &observed,
            marker.as_deref(),
        )?;
        self.after_commit(&state.manifest_bytes, &committed, state.manifest.project_id);
        state.local_generation = next_generation;
        state.manifest =
            serde_json::from_slice(&committed).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.manifest_bytes = committed;
        state.document = document;
        state.history = history;
        state.pending.clear();
        state.dirty = false;
        state.read_only_reason = None;
        let acknowledgement_error = archive
            .retained_source
            .as_ref()
            .and_then(|source| acknowledge_retained(&self.path, source).err())
            .map(|error| error.to_string());
        let mut snapshot: serde_json::Value = serde_json::from_str(&state.snapshot()?)
            .map_err(|e| CoreError::invalid(e.to_string()))?;
        drop(state);
        let diagnostics = self.diagnostics_json().map_err(|error| {
            CoreError::new(
                error.code,
                format!(
                    "Resolution was saved, but diagnostics refresh failed: {}",
                    error.message
                ),
            )
        })?;
        snapshot["diagnostics"] = serde_json::from_str(&diagnostics).map_err(|e| {
            CoreError::invalid(format!(
                "Resolution was saved, but diagnostics are invalid: {e}"
            ))
        })?;
        if let Some(error) = acknowledgement_error {
            snapshot["diagnostics"]["retainedAcknowledgementError"] = serde_json::json!(error);
        }
        serde_json::to_string(&snapshot).map_err(|e| CoreError::invalid(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::io::{Seek, SeekFrom, Write};

    fn snapshot(session: &crate::ProjectSession) -> Value {
        serde_json::from_str(&session.snapshot_json().unwrap()).unwrap()
    }

    fn edit_document(session: &crate::ProjectSession, name: &str) {
        let current = snapshot(session);
        let root = &current["document"]["pages"][0]["rootId"];
        let transaction = json!({
            "transactionId": Uuid::new_v4(),
            "baseRevision": current["document"]["revision"],
            "baseFingerprint": current["fingerprint"],
            "origin": "user",
            "scopeNodeIds": [root],
            "operations": [{"type":"setProperty", "nodeId":root, "path":"/name", "value":name}]
        });
        session.apply_transaction(&transaction.to_string()).unwrap();
    }

    fn diagnostics(session: &crate::ProjectSession) -> Value {
        serde_json::from_str(&session.diagnostics_json().unwrap()).unwrap()
    }

    fn resolution(diagnostics: &Value, kind: &str, disk_path: &str) -> Value {
        let conflict = diagnostics["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["kind"] == kind)
            .expect("typed conflict");
        let selections: Vec<Value> = conflict["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let path = entry["path"].as_str().unwrap();
                let take = if path == disk_path { "disk" } else { "local" };
                assert!(entry["choices"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|choice| choice == take));
                json!({"path":path, "take":take})
            })
            .collect();
        json!({
            "schemaVersion": 1,
            "conflictId": conflict["id"],
            "expectedDiskToken": diagnostics["observedDiskToken"],
            "expectedLocalGeneration": diagnostics["localGeneration"],
            "selections": selections,
        })
    }

    #[test]
    fn future_or_unknown_base_manifest_stays_unchanged_during_external_resolution() {
        for schema in [1, 2] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("project");
            drop(crate::create_project(&root, "Example").unwrap());
            let mut future: Value =
                serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
            future["schemaVersion"] = json!(schema);
            future["futureData"] = json!({"keep": [1, 2, 3]});
            let original = super::super::json(&future).unwrap();
            fs::write(root.join("project.json"), &original).unwrap();
            let session = crate::open_project(&root).unwrap();
            if schema == 2 {
                assert_eq!(snapshot(&session)["readOnly"], true);
            }
            fs::write(root.join(".gitignore"), b"future external edit\n").unwrap();
            let info = diagnostics(&session);
            let request = resolution(&info, "externalChange", ".gitignore");
            assert_eq!(
                session
                    .resolve_external_conflict(&request.to_string())
                    .unwrap_err()
                    .code,
                "UNSUPPORTED_SCHEMA"
            );
            assert_eq!(fs::read(root.join("project.json")).unwrap(), original);
            assert_eq!(
                fs::read(root.join(".gitignore")).unwrap(),
                b"future external edit\n"
            );
            if schema == 2 {
                assert_eq!(snapshot(&session)["readOnly"], true);
            }
        }
    }

    #[test]
    fn selected_future_or_unknown_disk_manifest_is_not_rewritten() {
        for schema in [1, 2] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("project");
            let session = crate::create_project(&root, "Example").unwrap();
            let mut future: Value =
                serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
            future["schemaVersion"] = json!(schema);
            future["futureData"] = json!({"keep": [1, 2, 3]});
            let original = super::super::json(&future).unwrap();
            fs::write(root.join("project.json"), &original).unwrap();
            let info = diagnostics(&session);
            let request = resolution(&info, "externalChange", "project.json");
            assert_eq!(
                session
                    .resolve_external_conflict(&request.to_string())
                    .unwrap_err()
                    .code,
                "UNSUPPORTED_SCHEMA"
            );
            assert_eq!(fs::read(root.join("project.json")).unwrap(), original);
        }
    }

    #[test]
    fn legacy_dirty_project_never_creates_a_false_old_baseline() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        drop(crate::create_project(&root, "Example").unwrap());
        let marker = fs::read(root.join("project.json")).unwrap();
        let manifest: Manifest = serde_json::from_slice(&marker).unwrap();
        fs::remove_dir_all(root.join(BASE)).unwrap();
        fs::write(root.join(".gitignore"), b"external-only\n").unwrap();
        let _lock = super::super::lock(&root).unwrap();
        assert!(!bootstrap(&root, &marker, &manifest).unwrap());
        assert!(!index_path(&root, &marker).unwrap().exists());
        assert_eq!(
            baseline_file(&root, &marker, &manifest, ".gitignore").unwrap(),
            None
        );
        assert_eq!(
            fs::read(root.join(".gitignore")).unwrap(),
            b"external-only\n"
        );
    }

    #[test]
    fn baseline_copies_are_independent_from_live_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        drop(crate::create_project(&root, "Example").unwrap());
        let marker = fs::read(root.join("project.json")).unwrap();
        let manifest: Manifest = serde_json::from_slice(&marker).unwrap();
        let old = fs::read(root.join("document.json")).unwrap();
        assert!(baseline_available(&root, &marker, &manifest).unwrap());
        fs::write(root.join("document.json"), b"external overwrite").unwrap();
        assert_eq!(
            baseline_document(&root, &marker, &manifest).unwrap(),
            Some(old)
        );
    }

    #[test]
    fn interruption_before_during_and_after_target_index_never_publishes_root() {
        for point in 1..=3 {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("project");
            let session = crate::create_project(&root, "Example").unwrap();
            let old_marker = fs::read(root.join("project.json")).unwrap();
            let old_document = fs::read(root.join("document.json")).unwrap();
            let old_manifest: Manifest = serde_json::from_slice(&old_marker).unwrap();
            edit_document(&session, "Pending");
            BASELINE_INTERRUPT.with(|slot| slot.set(Some(point)));
            assert!(session.save().is_err(), "point {point}");
            assert_eq!(
                fs::read(root.join("project.json")).unwrap(),
                old_marker,
                "point {point}"
            );
            assert_eq!(
                fs::read(root.join("document.json")).unwrap(),
                old_document,
                "point {point}"
            );
            assert!(
                baseline_available(&root, &old_marker, &old_manifest).unwrap(),
                "point {point}"
            );
            drop(session);
            let reopened = crate::open_project(&root).unwrap();
            assert_eq!(snapshot(&reopened)["document"]["nodes"][0]["name"], "Frame");
        }
    }

    #[test]
    fn archived_local_and_external_versions_are_selected_without_data_loss() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let session = crate::create_project(&root, "Example").unwrap();
        let old_marker = fs::read(root.join("project.json")).unwrap();
        let old_manifest: Manifest = serde_json::from_slice(&old_marker).unwrap();
        let old_document = fs::read(root.join("document.json")).unwrap();
        edit_document(&session, "Local pending");
        fs::write(root.join(".gitignore"), b"external content\n").unwrap();
        let info = diagnostics(&session);
        let conflict = info["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["kind"] == "externalChange")
            .unwrap();
        let doc = conflict["paths"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == "document.json")
            .unwrap();
        let local_path = doc["local"]["archiveRelativePath"].as_str().unwrap();
        assert!(String::from_utf8(fs::read(root.join(local_path)).unwrap())
            .unwrap()
            .contains("Local pending"));
        let request = resolution(&info, "externalChange", ".gitignore");
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        assert_eq!(
            fs::read(root.join(".gitignore")).unwrap(),
            b"external content\n"
        );
        assert_eq!(
            snapshot(&session)["document"]["nodes"][0]["name"],
            "Local pending"
        );
        assert_eq!(
            baseline_document(&root, &old_marker, &old_manifest).unwrap(),
            Some(old_document)
        );
        drop(session);
        let reopened = crate::open_project(&root).unwrap();
        assert_eq!(
            snapshot(&reopened)["document"]["nodes"][0]["name"],
            "Local pending"
        );
        assert_eq!(
            fs::read(root.join(".gitignore")).unwrap(),
            b"external content\n"
        );
    }

    #[test]
    fn stale_disk_or_local_generation_rejects_the_choice() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let session = crate::create_project(&root, "Example").unwrap();
        edit_document(&session, "First local");
        fs::write(root.join(".gitignore"), b"first external\n").unwrap();
        let info = diagnostics(&session);
        let request = resolution(&info, "externalChange", ".gitignore");
        fs::write(root.join(".gitignore"), b"new external\n").unwrap();
        assert_eq!(
            session
                .resolve_external_conflict(&request.to_string())
                .unwrap_err()
                .code,
            "REVISION_CONFLICT"
        );
        assert_eq!(
            fs::read(root.join(".gitignore")).unwrap(),
            b"new external\n"
        );
        let info = diagnostics(&session);
        let request = resolution(&info, "externalChange", ".gitignore");
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        let after_resolution = snapshot(&session);
        assert_eq!(
            session
                .resolve_external_conflict(&request.to_string())
                .unwrap_err()
                .code,
            "REVISION_CONFLICT"
        );
        assert_eq!(snapshot(&session), after_resolution);
    }

    #[test]
    fn choosing_base_or_disk_never_regresses_local_revision_or_loses_undo() {
        for take_disk in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("project");
            let session = crate::create_project(&root, "Example").unwrap();
            let mut disk: Value =
                serde_json::from_slice(&fs::read(root.join("document.json")).unwrap()).unwrap();
            edit_document(&session, "Local one");
            edit_document(&session, "Local two");
            let local_revision = snapshot(&session)["document"]["revision"].as_u64().unwrap();
            assert!(local_revision >= 2);
            disk["nodes"][0]["name"] = json!("External");
            fs::write(
                root.join("document.json"),
                super::super::json(&disk).unwrap(),
            )
            .unwrap();
            let info = diagnostics(&session);
            let conflict = info["conflicts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|value| value["kind"] == "externalChange")
                .unwrap();
            let selections: Vec<Value> = conflict["paths"].as_array().unwrap().iter().map(|entry| {
                let path = entry["path"].as_str().unwrap();
                json!({"path":path, "take": if take_disk && path == "document.json" {"disk"} else {"base"}})
            }).collect();
            let request = json!({
                "schemaVersion": 1,
                "conflictId": conflict["id"],
                "expectedDiskToken": info["observedDiskToken"],
                "expectedLocalGeneration": info["localGeneration"],
                "selections": selections,
            });
            session
                .resolve_external_conflict(&request.to_string())
                .unwrap();
            let resolved = snapshot(&session);
            assert!(resolved["document"]["revision"].as_u64().unwrap() > local_revision);
            assert_eq!(
                resolved["document"]["nodes"][0]["name"],
                if take_disk { "External" } else { "Frame" }
            );
            session.undo().unwrap();
            assert_eq!(
                snapshot(&session)["document"]["nodes"][0]["name"],
                "Local two"
            );
        }
    }

    #[test]
    fn interrupted_external_resolution_restores_disk_and_keeps_local_archive() {
        for point in 0..=1 {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("project");
            let session = crate::create_project(&root, "Example").unwrap();
            edit_document(&session, "Unsaved L");
            fs::write(root.join(".gitignore"), b"external D\n").unwrap();
            let info = diagnostics(&session);
            let request = resolution(&info, "externalChange", ".gitignore");
            let id = request["conflictId"].as_str().unwrap().to_owned();
            super::super::INTERRUPT_AFTER.with(|slot| slot.set(Some(point)));
            assert!(session
                .resolve_external_conflict(&request.to_string())
                .is_err());
            drop(session);
            let reopened = crate::open_project(&root).unwrap();
            assert_eq!(fs::read(root.join(".gitignore")).unwrap(), b"external D\n");
            let archive = read_archive(&root, &id).unwrap();
            let (index, file) = archive
                .paths
                .iter()
                .enumerate()
                .find(|(_, file)| file.path == "document.json")
                .unwrap();
            let local = archived_version(&root, &id, index, "local", &file.local)
                .unwrap()
                .unwrap();
            assert!(String::from_utf8(local).unwrap().contains("Unsaved L"));
            assert_eq!(snapshot(&reopened)["readOnly"], true);
        }
    }

    #[test]
    fn generated_external_source_stays_blocked_after_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let session = crate::create_project(&root, "Example").unwrap();
        let checkpoint = session.create_checkpoint().unwrap();
        fs::write(root.join("src/generated/external.tsx"), b"external source").unwrap();
        let info = diagnostics(&session);
        let request = resolution(&info, "externalChange", "src/generated/external.tsx");
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        drop(session);
        let reopened = crate::open_project(&root).unwrap();
        let info = diagnostics(&reopened);
        assert!(info["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
        let portable = temporary.path().join("portable");
        reopened.create_portable_snapshot(&portable).unwrap();
        assert!(!portable.join(".aic-local").exists());
        let portable_session = crate::open_project(&portable).unwrap();
        assert!(diagnostics(&portable_session)["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
        assert!(portable_session.restore_checkpoint(&checkpoint).is_err());
        assert_eq!(
            fs::read(portable.join("src/generated/external.tsx")).unwrap(),
            b"external source"
        );
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["-c", "core.excludesFile=/dev/null"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        let tracked = git(&["ls-files", "-z"]);
        let copy = temporary.path().join("git-only");
        fs::create_dir(&copy).unwrap();
        let mut copied_block = false;
        for path in std::str::from_utf8(&tracked)
            .unwrap()
            .split('\0')
            .filter(|path| !path.is_empty())
        {
            copied_block |= path.starts_with("history/generated-blocks/");
            let destination = copy.join(path);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(root.join(path), destination).unwrap();
        }
        assert!(copied_block);
        assert!(!copy.join(".aic-local").exists());
        let git_copy = crate::open_project(&copy).unwrap();
        assert!(diagnostics(&git_copy)["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
        assert!(git_copy.restore_checkpoint(&checkpoint).is_err());
        let duplicate_path = temporary.path().join("duplicate");
        let duplicate = reopened.duplicate_to(&duplicate_path, "Copy").unwrap();
        let copy_info = diagnostics(&duplicate);
        assert!(copy_info["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
        assert!(duplicate.restore_checkpoint(&checkpoint).is_err());
        assert_eq!(
            fs::read(duplicate_path.join("src/generated/external.tsx")).unwrap(),
            b"external source"
        );
        drop(duplicate);
        fs::remove_dir_all(duplicate_path.join(".aic-local")).unwrap();
        let duplicate = crate::open_project(&duplicate_path).unwrap();
        assert!(diagnostics(&duplicate)["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
        assert!(duplicate.restore_checkpoint(&checkpoint).is_err());
        let (record, _) = generated_block(
            &duplicate_path,
            "src/generated/external.tsx",
            &hash(b"external source"),
        )
        .unwrap();
        fs::remove_file(duplicate_path.join(&record)).unwrap();
        let info = diagnostics(&duplicate);
        assert!(info["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
        let request = resolution(&info, "externalChange", &record);
        assert!(duplicate
            .resolve_external_conflict(&request.to_string())
            .is_err());
        assert_eq!(
            fs::read(duplicate_path.join("src/generated/external.tsx")).unwrap(),
            b"external source"
        );
        let state = reopened.state().unwrap();
        let changes =
            BTreeMap::from([("src/generated/external.tsx".into(), b"replacement".to_vec())]);
        assert!(
            super::super::commit(&root, &state.manifest_bytes, &state.manifest, &changes).is_err()
        );
        assert_eq!(
            fs::read(root.join("src/generated/external.tsx")).unwrap(),
            b"external source"
        );
    }

    #[test]
    fn legacy_generated_blocks_become_portable_on_open() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let session = crate::create_project(&root, "Example").unwrap();
        fs::write(root.join("src/generated/external.tsx"), b"external source").unwrap();
        let info = diagnostics(&session);
        let request = resolution(&info, "externalChange", "src/generated/external.tsx");
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        drop(session);
        let name = format!(
            "{}-{}.json",
            hash(b"src/generated/external.tsx"),
            hash(b"external source")
        );
        let relative = format!("history/generated-blocks/{name}");
        let portable_block = root.join(&relative);
        let legacy = root.join(".aic-local/blocked-generated");
        fs::create_dir_all(&legacy).unwrap();
        fs::rename(&portable_block, legacy.join(&name)).unwrap();
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
        manifest.files.remove(&relative).unwrap();
        fs::write(
            root.join("project.json"),
            super::super::json(&manifest).unwrap(),
        )
        .unwrap();
        let reopened = crate::open_project(&root).unwrap();
        assert!(portable_block.is_file());
        drop(reopened);
        fs::remove_dir_all(root.join(".aic-local")).unwrap();
        let reopened = crate::open_project(&root).unwrap();
        assert!(diagnostics(&reopened)["blockedGenerated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "src/generated/external.tsx"));
    }

    #[test]
    fn late_write_to_retained_descriptor_is_diagnosed_and_selectable() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let session = crate::create_project(&root, "Example").unwrap();
        let mut old = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("document.json"))
            .unwrap();
        let mut external: Value =
            serde_json::from_slice(&fs::read(root.join("document.json")).unwrap()).unwrap();
        edit_document(&session, "Committed");
        session.save().unwrap();
        external["nodes"][0]["name"] = json!("Late external");
        let bytes = super::super::json(&external).unwrap();
        old.seek(SeekFrom::Start(0)).unwrap();
        old.set_len(0).unwrap();
        old.write_all(&bytes).unwrap();
        old.sync_all().unwrap();
        let info = diagnostics(&session);
        assert_eq!(info["readOnly"], true);
        assert_eq!(info["manifestChanged"], false);
        assert!(info["files"].as_array().unwrap().is_empty());
        let portable = temporary.path().join("portable");
        assert_eq!(
            session
                .create_portable_snapshot(&portable)
                .unwrap_err()
                .code,
            "REVISION_CONFLICT"
        );
        assert!(!portable.exists());
        let request = resolution(&info, "retainedWrite", "document.json");
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        assert_eq!(
            snapshot(&session)["document"]["nodes"][0]["name"],
            "Late external"
        );
        session.create_portable_snapshot(&portable).unwrap();
        let id = request["conflictId"].as_str().unwrap();
        let archive = format!("conflicts/{id}/metadata.json");
        assert_eq!(
            fs::read(portable.join(&archive)).unwrap(),
            fs::read(root.join(&archive)).unwrap()
        );
        drop(session);
        let reopened = crate::open_project(&root).unwrap();
        assert_eq!(
            snapshot(&reopened)["document"]["nodes"][0]["name"],
            "Late external"
        );
        assert_eq!(diagnostics(&reopened)["readOnly"], false);
        assert!(diagnostics(&reopened)["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|conflict| conflict["kind"] != "retainedWrite"));
    }

    #[test]
    fn missing_marker_uses_only_an_unambiguous_verified_baseline() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        drop(crate::create_project(&root, "Example").unwrap());
        fs::remove_file(root.join("project.json")).unwrap();
        let session = crate::open_project(&root).unwrap();
        assert_eq!(snapshot(&session)["readOnly"], true);
        let info = diagnostics(&session);
        let request = resolution(&info, "externalChange", "not-a-project-path");
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        assert!(root.join("project.json").is_file());
        assert_eq!(snapshot(&session)["readOnly"], false);
    }

    #[test]
    fn missing_marker_with_multiple_baselines_is_not_guessed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("project");
        let session = crate::create_project(&root, "Example").unwrap();
        edit_document(&session, "Second baseline");
        session.save().unwrap();
        drop(session);
        fs::remove_file(root.join("project.json")).unwrap();
        let error = match crate::open_project(&root) {
            Ok(_) => panic!("ambiguous baselines must not be guessed"),
            Err(error) => error,
        };
        assert_eq!(error.code, "BASELINE_UNAVAILABLE");
        assert!(!root.join("project.json").exists());
    }
}
