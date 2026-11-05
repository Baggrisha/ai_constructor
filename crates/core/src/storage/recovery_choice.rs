//! Explicit whole-snapshot recovery for an active journal with an unknown marker.
//!
//! The original journal stays active while a durable choice is installed beside
//! it. There is therefore no interval in which a crash can hide both journals.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    atomic, conflicts, control, durable_write, file_bytes, file_hash, hash, hash_file,
    json as encode, lock, replace_with, safe_path, scan, sync_dir, unexpected_retained,
    verify_backup, Entry, Journal, Loaded, Manifest, ObservedMarker,
};
use crate::{CoreError, ProjectSession, Result};

#[cfg(test)]
thread_local! {
    static BEFORE_CHOICE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema_version: u32,
    conflict_id: String,
    expected_disk_token: String,
    expected_local_generation: u64,
    take: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotFile {
    path: String,
    hash: String,
    archive_relative_path: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    available: bool,
    manifest_hash: String,
    revision: u64,
    storage_generation: u64,
    manifest_path: String,
    files: Vec<SnapshotFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshots {
    old: Snapshot,
    new: Snapshot,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Archive {
    schema_version: u32,
    id: String,
    kind: String,
    reason: String,
    journal_id: String,
    journal_hash: String,
    observed_disk_token: String,
    observed_marker_hash: Option<String>,
    observed_files: BTreeMap<String, String>,
    choices: Vec<String>,
    snapshots: Snapshots,
    blocked_paths: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    schema_version: u32,
    conflict_id: String,
    journal_hash: String,
    take: String,
}

fn digest_id(
    journal_id: &str,
    journal_hash: &str,
    disk_token: &str,
    old: bool,
    new: bool,
) -> Result<String> {
    let bytes = serde_json::to_vec(&(journal_id, journal_hash, disk_token, old, new))
        .map_err(|e| CoreError::invalid(e.to_string()))?;
    Ok(hash(&bytes))
}

fn is_digest(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn read_regular(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = safe_path(root, relative)?;
    if !fs::symlink_metadata(&path)?.file_type().is_file() {
        return Err(CoreError::invalid("Recovery copy is not a regular file"));
    }
    file_bytes(root, relative)?.ok_or_else(|| CoreError::conflict("Recovery copy disappeared"))
}

fn checked_tx(root: &Path, id: &str) -> Result<PathBuf> {
    if Uuid::parse_str(id).is_err() {
        return Err(CoreError::invalid("Invalid recovery journal id"));
    }
    let dir = safe_path(root, &format!(".aic-local/transactions/{id}"))?;
    if !fs::symlink_metadata(&dir)?.file_type().is_dir() {
        return Err(CoreError::invalid(
            "Recovery journal directory is not a directory",
        ));
    }
    Ok(dir)
}

type ActiveJournal = (String, PathBuf, Vec<u8>, Journal);

fn active_journal(root: &Path) -> Result<Option<ActiveJournal>> {
    let directory = safe_path(root, ".aic-local/transactions")?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut active = None;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let journal = entry.path().join("journal.json");
        if !journal.exists() {
            continue;
        }
        let dir = checked_tx(root, &id)?;
        let bytes = read_regular(root, &format!(".aic-local/transactions/{id}/journal.json"))?;
        let journal: Journal = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid recovery journal: {e}")))?;
        if active.is_some() {
            return Err(CoreError::conflict("Multiple active recovery journals"));
        }
        active = Some((id, dir, bytes, journal));
    }
    Ok(active)
}

fn manifests(dir: &Path, journal: &Journal) -> Result<[(Vec<u8>, Manifest); 2]> {
    for name in ["old-manifest", "new-manifest"] {
        if !fs::symlink_metadata(dir.join(name))?.file_type().is_file() {
            return Err(CoreError::invalid(
                "Recovery manifest copy is not a regular file",
            ));
        }
    }
    let old_bytes = verify_backup(dir, "old-manifest", &journal.old_manifest_hash)?;
    let new_bytes = verify_backup(dir, "new-manifest", &journal.new_manifest_hash)?;
    let old: Manifest =
        serde_json::from_slice(&old_bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
    let new: Manifest =
        serde_json::from_slice(&new_bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
    if old.project_id != new.project_id
        || old.revision != journal.old_revision
        || new.revision != journal.new_revision
        || old.storage_generation != journal.old_generation
        || new.storage_generation != journal.new_generation
    {
        return Err(CoreError::invalid(
            "Recovery journal manifest metadata does not match",
        ));
    }
    Ok([(old_bytes, old), (new_bytes, new)])
}

fn source_file(
    root: &Path,
    dir: &Path,
    journal: &Journal,
    bytes: &[u8],
    manifest: &Manifest,
    file: (&str, &str),
    observed: &BTreeMap<String, String>,
) -> Result<Option<Vec<u8>>> {
    let (path, expected) = file;
    for entry in journal.entries.iter().filter(|entry| entry.path == path) {
        for (name, digest) in [
            (&entry.old_copy, &entry.old_hash),
            (&entry.new_copy, &entry.new_hash),
        ] {
            if digest.as_deref() != Some(expected) {
                continue;
            }
            if let Some(name) = name {
                if Path::new(name).components().count() != 1
                    || !matches!(
                        Path::new(name).components().next(),
                        Some(Component::Normal(_))
                    )
                {
                    return Err(CoreError::invalid("Invalid journal copy path"));
                }
                let copy = dir.join(name);
                if !fs::symlink_metadata(&copy)?.file_type().is_file() {
                    return Err(CoreError::invalid("Journal copy is not a regular file"));
                }
                return Ok(Some(verify_backup(dir, name, expected)?));
            }
        }
    }
    if let Some(copy) = conflicts::baseline_file(root, bytes, manifest, path)? {
        return Ok(Some(copy));
    }
    if observed.get(path).map(String::as_str) == Some(expected) {
        let copy = file_bytes(root, path)?;
        if copy.as_deref().map(hash).as_deref() == Some(expected) {
            return Ok(copy);
        }
        return Err(CoreError::conflict(
            "Project changed while archiving recovery snapshots",
        ));
    }
    Ok(None)
}

fn validate_tracked_path(root: &Path, path: &str, digest: &str) -> Result<()> {
    let allowed = path == "document.json"
        || path == ".gitignore"
        || [
            "src/",
            "assets/",
            "chats/",
            "agent/",
            "history/",
            "checkpoints/",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix));
    if !allowed || path.contains('\\') || !is_digest(digest) {
        return Err(CoreError::invalid(
            "Invalid recovery snapshot path or digest",
        ));
    }
    safe_path(root, path)?;
    Ok(())
}

fn valid_snapshot(manifest_bytes: &[u8], manifest: &Manifest, document: &[u8]) -> Result<()> {
    conflicts::require_supported_manifest(manifest_bytes, manifest)?;
    if super::fingerprint(document, &manifest.files) != manifest.snapshot_hash {
        return Err(CoreError::invalid(
            "Recovery snapshot manifest or fingerprint is invalid",
        ));
    }
    let value: Value =
        serde_json::from_slice(document).map_err(|e| CoreError::invalid(e.to_string()))?;
    let model: crate::model::Document = serde_json::from_value(value)
        .map_err(|e| CoreError::invalid(format!("Invalid recovery document: {e}")))?;
    model.validate()?;
    model.validate_files(&manifest.files)?;
    if model.project_id != manifest.project_id || model.revision != manifest.revision {
        return Err(CoreError::invalid(
            "Recovery document does not match its manifest",
        ));
    }
    Ok(())
}

fn stage_snapshot(
    root: &Path,
    stage: &Path,
    dir: &Path,
    journal: &Journal,
    name: &str,
    manifest: (&[u8], &Manifest),
    observed: &BTreeMap<String, String>,
) -> Result<Snapshot> {
    let (bytes, manifest) = manifest;
    durable_write(&stage.join(format!("{name}-manifest")), bytes)?;
    let mut available = true;
    let mut document = None;
    let mut files = Vec::new();
    for (index, (path, digest)) in manifest.files.iter().enumerate() {
        control::check()?;
        validate_tracked_path(root, path, digest)?;
        let copy = source_file(
            root,
            dir,
            journal,
            bytes,
            manifest,
            (path, digest),
            observed,
        )?;
        let archive_relative_path = if let Some(copy) = copy {
            if hash(&copy) != *digest {
                return Err(CoreError::conflict("Recovery snapshot bytes changed"));
            }
            let file = format!("{name}-{index}");
            durable_write(&stage.join(&file), &copy)?;
            if path == "document.json" {
                document = Some(copy);
            }
            Some(file)
        } else {
            available = false;
            None
        };
        files.push(SnapshotFile {
            path: path.clone(),
            hash: digest.clone(),
            archive_relative_path,
        });
    }
    if document
        .as_deref()
        .is_none_or(|document| valid_snapshot(bytes, manifest, document).is_err())
    {
        available = false;
    }
    Ok(Snapshot {
        available,
        manifest_hash: hash(bytes),
        revision: manifest.revision,
        storage_generation: manifest.storage_generation,
        manifest_path: format!("{name}-manifest"),
        files,
    })
}

fn finish_snapshot_paths(id: &str, snapshot: &mut Snapshot) {
    snapshot.manifest_path = format!("conflicts/{id}/{}", snapshot.manifest_path);
    for file in &mut snapshot.files {
        if let Some(path) = &mut file.archive_relative_path {
            *path = format!("conflicts/{id}/{path}");
        }
    }
}

fn load_archive(root: &Path, id: &str) -> Result<Archive> {
    if !is_digest(id) {
        return Err(CoreError::invalid("Invalid recovery conflict id"));
    }
    let bytes = read_regular(root, &format!("conflicts/{id}/metadata.json"))?;
    let archive: Archive = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid recovery archive: {e}")))?;
    let token = serde_json::to_vec(&(&archive.observed_marker_hash, &archive.observed_files))
        .map(|bytes| hash(&bytes))
        .map_err(|e| CoreError::invalid(e.to_string()))?;
    if archive.schema_version != 1
        || archive.kind != "recovery"
        || archive.id != id
        || token != archive.observed_disk_token
        || digest_id(
            &archive.journal_id,
            &archive.journal_hash,
            &token,
            archive.snapshots.old.available,
            archive.snapshots.new.available,
        )? != id
    {
        return Err(CoreError::conflict("Recovery archive identity changed"));
    }
    Ok(archive)
}

fn verify_observed_archive(root: &Path, archive: &Archive) -> Result<()> {
    for (index, expected) in archive.observed_files.values().enumerate() {
        control::check()?;
        let relative = format!("conflicts/{}/disk-{index}", archive.id);
        let path = safe_path(root, &relative)?;
        if !fs::symlink_metadata(&path)?.file_type().is_file() || hash_file(&path)? != *expected {
            return Err(CoreError::conflict("Archived external data changed"));
        }
    }
    if hash(&read_regular(
        root,
        &format!("conflicts/{}/journal-original", archive.id),
    )?) != archive.journal_hash
    {
        return Err(CoreError::conflict("Archived recovery journal changed"));
    }
    if let Some(expected) = &archive.observed_marker_hash {
        let bytes = read_regular(root, &format!("conflicts/{}/disk-manifest", archive.id))?;
        if hash(&bytes) != *expected {
            return Err(CoreError::conflict("Archived external marker changed"));
        }
    }
    Ok(())
}

fn verify_snapshot(
    root: &Path,
    id: &str,
    name: &str,
    snapshot: &Snapshot,
) -> Result<(Vec<u8>, Manifest)> {
    if !snapshot.available {
        return Err(CoreError::new(
            "BASELINE_UNAVAILABLE",
            "The selected complete snapshot is unavailable",
        ));
    }
    if snapshot.manifest_path != format!("conflicts/{id}/{name}-manifest") {
        return Err(CoreError::invalid("Invalid recovery manifest path"));
    }
    let bytes = read_regular(root, &snapshot.manifest_path)?;
    if hash(&bytes) != snapshot.manifest_hash {
        return Err(CoreError::conflict("Recovery manifest changed"));
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
    if manifest.revision != snapshot.revision
        || manifest.storage_generation != snapshot.storage_generation
        || manifest.files.len() != snapshot.files.len()
    {
        return Err(CoreError::conflict("Recovery snapshot metadata changed"));
    }
    for (path, digest) in &manifest.files {
        validate_tracked_path(root, path, digest)?;
    }
    let mut document = None;
    for (index, ((expected_path, expected_hash), file)) in
        manifest.files.iter().zip(&snapshot.files).enumerate()
    {
        control::check()?;
        if expected_path != &file.path
            || expected_hash != &file.hash
            || file.archive_relative_path.as_deref()
                != Some(format!("conflicts/{id}/{name}-{index}").as_str())
        {
            return Err(CoreError::conflict("Recovery snapshot file list changed"));
        }
        let relative = file.archive_relative_path.as_ref().expect("checked above");
        let path = safe_path(root, relative)?;
        if !fs::symlink_metadata(&path)?.file_type().is_file() || hash_file(&path)? != file.hash {
            return Err(CoreError::conflict("Recovery snapshot copy changed"));
        }
        if file.path == "document.json" {
            document = Some(read_regular(root, relative)?);
        }
    }
    valid_snapshot(
        &bytes,
        &manifest,
        document
            .as_deref()
            .ok_or_else(|| CoreError::invalid("Missing recovery document"))?,
    )?;
    Ok((bytes, manifest))
}

fn verify_available_snapshots(root: &Path, archive: &Archive) -> Result<()> {
    if archive.snapshots.old.available {
        verify_snapshot(root, &archive.id, "old", &archive.snapshots.old)?;
    }
    if archive.snapshots.new.available {
        verify_snapshot(root, &archive.id, "new", &archive.snapshots.new)?;
    }
    Ok(())
}

/// Called while the session and project locks are held.
pub(super) fn diagnostics(
    root: &Path,
    observed: &BTreeMap<String, String>,
    marker: Option<&[u8]>,
    token: &str,
) -> Result<Vec<Value>> {
    let Some((txid, dir, journal_bytes, journal)) = active_journal(root)? else {
        return Ok(Vec::new());
    };
    let old_marker = journal
        .observed_marker
        .as_ref()
        .map(|m| m.hash.as_deref())
        .unwrap_or(Some(journal.old_manifest_hash.as_str()));
    let marker_hash = marker.map(hash);
    let marker_known = marker_hash.is_some()
        && (marker_hash.as_deref() == old_marker
            || marker_hash.as_deref() == Some(journal.new_manifest_hash.as_str()));
    let pairs = manifests(&dir, &journal)?;
    let local = safe_path(root, ".aic-local")?;
    let stage = local.join(format!("recovery-{}.tmp", Uuid::new_v4()));
    fs::create_dir(&stage)?;
    sync_dir(&local)?;
    let result = (|| -> Result<Archive> {
        durable_write(&stage.join("journal-original"), &journal_bytes)?;
        let mut old = stage_snapshot(
            root,
            &stage,
            &dir,
            &journal,
            "old",
            (&pairs[0].0, &pairs[0].1),
            observed,
        )?;
        let mut new = stage_snapshot(
            root,
            &stage,
            &dir,
            &journal,
            "new",
            (&pairs[1].0, &pairs[1].1),
            observed,
        )?;
        // Preserve the complete observed tree independently of live inodes.
        for (index, (path, expected)) in observed.iter().enumerate() {
            control::check()?;
            let bytes = file_bytes(root, path)?
                .ok_or_else(|| CoreError::conflict("Observed file disappeared"))?;
            if hash(&bytes) != *expected {
                return Err(CoreError::conflict("Observed file changed while archiving"));
            }
            durable_write(&stage.join(format!("disk-{index}")), &bytes)?;
        }
        if let Some(bytes) = marker {
            durable_write(&stage.join("disk-manifest"), bytes)?;
        }
        let journal_hash = hash(&journal_bytes);
        let id = digest_id(&txid, &journal_hash, token, old.available, new.available)?;
        finish_snapshot_paths(&id, &mut old);
        finish_snapshot_paths(&id, &mut new);
        let mut choices = Vec::new();
        if old.available {
            choices.push("old".to_owned());
        }
        if new.available {
            choices.push("new".to_owned());
        }
        let archive = Archive {
            schema_version: 1,
            id: id.clone(),
            kind: "recovery".into(),
            reason: if marker_known {
                "External changes conflict with the active journal".into()
            } else {
                "The active journal has an unknown commit marker".into()
            },
            journal_id: txid.clone(),
            journal_hash,
            observed_disk_token: token.into(),
            observed_marker_hash: marker_hash.clone(),
            observed_files: observed.clone(),
            choices,
            snapshots: Snapshots { old, new },
            blocked_paths: Vec::new(),
        };
        durable_write(&stage.join("metadata.json"), &encode(&archive)?)?;
        sync_dir(&stage)?;
        if scan(root)? != *observed
            || file_bytes(root, "project.json")?.as_deref() != marker
            || read_regular(
                root,
                &format!(".aic-local/transactions/{txid}/journal.json"),
            )? != journal_bytes
        {
            return Err(CoreError::conflict(
                "Project changed while archiving recovery snapshots",
            ));
        }
        let conflicts_dir = safe_path(root, "conflicts")?;
        match fs::create_dir(&conflicts_dir) {
            Ok(()) => sync_dir(root)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&conflicts_dir)?.file_type().is_dir() {
                    return Err(CoreError::invalid("Conflict directory is not a directory"));
                }
            }
            Err(e) => return Err(e.into()),
        }
        let destination = safe_path(root, &format!("conflicts/{id}"))?;
        match atomic::move_no_replace(&stage, &destination) {
            Ok(()) => {
                sync_dir(&local)?;
                sync_dir(&conflicts_dir)?;
            }
            Err(_) if destination.exists() => {
                let existing = load_archive(root, &id)?;
                if existing.journal_id != txid || existing.journal_hash != archive.journal_hash {
                    return Err(CoreError::conflict("Existing recovery archive changed"));
                }
                fs::remove_dir_all(&stage)?;
                return Ok(existing);
            }
            Err(e) => return Err(e.into()),
        }
        Ok(archive)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    let archive = result?;
    verify_observed_archive(root, &archive)?;
    verify_available_snapshots(root, &archive)?;
    Ok(vec![
        serde_json::to_value(archive).map_err(|e| CoreError::invalid(e.to_string()))?
    ])
}

fn selected<'a>(archive: &'a Archive, take: &str) -> Result<&'a Snapshot> {
    match take {
        "old" => Ok(&archive.snapshots.old),
        "new" => Ok(&archive.snapshots.new),
        _ => Err(CoreError::invalid(
            "Select one complete old or new snapshot",
        )),
    }
}

fn retained_choice(
    root: &Path,
    archive: &Archive,
    journal: &Journal,
    manifest: &Manifest,
    take: &str,
    marker_hash: &str,
) -> Result<PathBuf> {
    let local = safe_path(root, ".aic-local/retained")?;
    fs::create_dir_all(&local)?;
    let dir = safe_path(
        root,
        &format!(".aic-local/retained/choice-{}-{take}", archive.id),
    )?;
    match fs::create_dir(&dir) {
        Ok(()) => sync_dir(&local)?,
        Err(e)
            if e.kind() == std::io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(&dir)?.file_type().is_dir() => {}
        Err(e) => return Err(e.into()),
    }
    let paths: BTreeSet<_> = archive
        .observed_files
        .keys()
        .chain(manifest.files.keys())
        .cloned()
        .collect();
    let entries = paths
        .into_iter()
        .map(|path| Entry {
            old_hash: archive.observed_files.get(&path).cloned(),
            new_hash: manifest.files.get(&path).cloned(),
            path,
            old_copy: None,
            new_copy: None,
        })
        .collect();
    let record = Journal {
        old_manifest_hash: journal.old_manifest_hash.clone(),
        new_manifest_hash: marker_hash.into(),
        old_revision: journal.old_revision,
        new_revision: manifest.revision,
        old_generation: journal.old_generation,
        new_generation: manifest.storage_generation,
        observed_files: Some(archive.observed_files.clone()),
        observed_marker: Some(ObservedMarker {
            hash: archive.observed_marker_hash.clone(),
        }),
        entries,
    };
    let bytes = encode(&record)?;
    let marker = dir.join("journal.json");
    let publish = match fs::symlink_metadata(&marker) {
        Ok(meta) if meta.file_type().is_file() => {
            let existing = fs::read(&marker)?;
            let same_record = serde_json::from_slice::<Value>(&existing).is_ok_and(|value| {
                serde_json::to_value(&record).is_ok_and(|expected| value == expected)
            });
            if existing == bytes || same_record {
                // An old in-place writer may have completed JSON without
                // making it durable before the process died.
                fs::File::open(&marker)?.sync_all()?;
                sync_dir(&dir)?;
                false
            } else if bytes.starts_with(&existing)
                && serde_json::from_slice::<Journal>(&existing).is_err()
            {
                // Older versions wrote the canonical journal in place. Keep
                // its exact interrupted bytes before installing a new record.
                fs::File::open(&marker)?.sync_all()?;
                let preserved = dir.join(format!("journal-incomplete-{}", Uuid::new_v4()));
                atomic::move_no_replace(&marker, &preserved)?;
                sync_dir(&dir)?;
                true
            } else {
                return Err(CoreError::conflict("Recovery retention journal changed"));
            }
        }
        Ok(_) => {
            return Err(CoreError::invalid(
                "Recovery retention journal is not a regular file",
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
    };
    if publish {
        // A partial retention record must never be mistaken for a published
        // journal by retained_changes or by a later recovery attempt.
        let pending = dir.join(format!("journal-{}.pending", Uuid::new_v4()));
        durable_write(&pending, &bytes)?;
        sync_dir(&dir)?;
        #[cfg(test)]
        super::interrupt_at(4000)?;
        atomic::move_no_replace(&pending, &marker)?;
        sync_dir(&dir)?;
        #[cfg(test)]
        super::interrupt_at(4001)?;
    }
    Ok(dir)
}

fn readonly(
    root: &Path,
    dir: &Path,
    journal: &Journal,
    archive: &Archive,
    reason: &str,
) -> Result<Option<Loaded>> {
    super::preserve_conflict(root, dir, journal, reason)?;
    for (name, snapshot) in [
        ("old", &archive.snapshots.old),
        ("new", &archive.snapshots.new),
    ] {
        if !snapshot.available {
            continue;
        }
        let (bytes, manifest) = verify_snapshot(root, &archive.id, name, snapshot)?;
        let document = snapshot
            .files
            .iter()
            .find(|entry| entry.path == "document.json")
            .and_then(|entry| entry.archive_relative_path.as_deref())
            .ok_or_else(|| CoreError::invalid("Missing recovery document copy"))?;
        return Ok(Some(Loaded {
            manifest,
            manifest_bytes: bytes,
            document_bytes: read_regular(root, document)?,
            read_only_reason: Some(reason.into()),
        }));
    }
    Err(CoreError::new(
        "BASELINE_UNAVAILABLE",
        "No complete recovery document is available",
    ))
}

/// Resume an already published choice. Caller holds the project lock.
pub(super) fn resume(
    root: &Path,
    dir: &Path,
    journal: &Journal,
    journal_bytes: &[u8],
) -> Result<Option<Loaded>> {
    let id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CoreError::invalid("Invalid recovery directory"))?;
    let intent_bytes = read_regular(
        root,
        &format!(".aic-local/transactions/{id}/recovery-choice.json"),
    )?;
    let intent: Intent = serde_json::from_slice(&intent_bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid recovery choice: {e}")))?;
    let archive = load_archive(root, &intent.conflict_id)?;
    if intent.schema_version != 1
        || intent.journal_hash != hash(journal_bytes)
        || archive.journal_hash != intent.journal_hash
        || archive.journal_id != id
        || archive.snapshots.old.manifest_hash != journal.old_manifest_hash
        || archive.snapshots.new.manifest_hash != journal.new_manifest_hash
        || read_regular(root, &format!(".aic-local/transactions/{id}/journal.json"))?
            != journal_bytes
    {
        return Err(CoreError::conflict(
            "Recovery choice no longer matches its journal",
        ));
    }
    let snapshot = selected(&archive, &intent.take)?;
    let (target_marker, target) = verify_snapshot(root, &archive.id, &intent.take, snapshot)?;
    let pairs = manifests(dir, journal)?;
    let expected_manifest = if intent.take == "old" {
        &pairs[0].0
    } else {
        &pairs[1].0
    };
    if &target_marker != expected_manifest {
        return Err(CoreError::conflict(
            "Selected snapshot is not from this journal",
        ));
    }
    verify_observed_archive(root, &archive)?;
    verify_available_snapshots(root, &archive)?;
    let marker_hash = hash(&target_marker);
    let retained = retained_choice(root, &archive, journal, &target, &intent.take, &marker_hash)?;
    let paths: Vec<_> = archive
        .observed_files
        .keys()
        .chain(target.files.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let reason = "External change during chosen recovery; select a snapshot again";
    let observed = scan(root)?;
    let current_marker = file_hash(root, "project.json")?;
    if current_marker != archive.observed_marker_hash
        && current_marker.as_deref() != Some(marker_hash.as_str())
    {
        return readonly(root, dir, journal, &archive, reason);
    }
    if unexpected_retained(
        &retained.join("manifest"),
        archive.observed_marker_hash.as_deref(),
        Some(&marker_hash),
    )? {
        return readonly(root, dir, journal, &archive, reason);
    }
    for (index, path) in paths.iter().enumerate() {
        let before = archive.observed_files.get(path);
        let after = target.files.get(path);
        if (observed.get(path) != before && observed.get(path) != after)
            || unexpected_retained(
                &retained.join(format!("entry-{index}")),
                before.map(String::as_str),
                after.map(String::as_str),
            )?
        {
            return readonly(root, dir, journal, &archive, reason);
        }
    }
    for path in observed.keys() {
        if !archive.observed_files.contains_key(path) && !target.files.contains_key(path) {
            return readonly(root, dir, journal, &archive, reason);
        }
    }
    let mut destination_directories = BTreeMap::new();
    for (index, path) in paths.iter().enumerate() {
        control::check()?;
        let before = archive.observed_files.get(path);
        let after = target.files.get(path);
        let current = file_hash(root, path)?;
        if current.as_ref() == after {
            continue;
        }
        if current.as_ref() != before {
            return readonly(root, dir, journal, &archive, reason);
        }
        if file_hash(root, "project.json")? != current_marker {
            return readonly(root, dir, journal, &archive, reason);
        }
        if read_regular(root, &format!(".aic-local/transactions/{id}/journal.json"))?
            != journal_bytes
        {
            return Err(CoreError::conflict(
                "Recovery journal changed during publication",
            ));
        }
        super::checkpoint_destination(root, path, &mut destination_directories)?;
        let bytes = match after {
            Some(expected) => {
                let source = snapshot
                    .files
                    .binary_search_by(|entry| entry.path.as_str().cmp(path))
                    .ok()
                    .and_then(|index| snapshot.files[index].archive_relative_path.as_deref())
                    .ok_or_else(|| CoreError::invalid("Selected recovery copy is missing"))?;
                let bytes = read_regular(root, source)?;
                if hash(&bytes) != *expected {
                    return Err(CoreError::conflict("Selected recovery copy changed"));
                }
                Some(bytes)
            }
            None => None,
        };
        #[cfg(test)]
        super::BEFORE_REPLACE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        replace_with(
            root,
            path,
            current.as_deref(),
            bytes.as_deref(),
            &retained.join(format!("entry-{index}")),
        )?;
        #[cfg(test)]
        super::interrupt_at(2002 + index)?;
    }
    if scan(root)? != target.files || file_hash(root, "project.json")? != current_marker {
        return readonly(root, dir, journal, &archive, reason);
    }
    if current_marker.as_deref() != Some(marker_hash.as_str()) {
        #[cfg(test)]
        super::BEFORE_MANIFEST_REPLACE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        replace_with(
            root,
            "project.json",
            current_marker.as_deref(),
            Some(&target_marker),
            &retained.join("manifest"),
        )?;
        #[cfg(test)]
        super::interrupt_at(3000)?;
    }
    if scan(root)? != target.files
        || file_hash(root, "project.json")?.as_deref() != Some(marker_hash.as_str())
    {
        return readonly(root, dir, journal, &archive, reason);
    }
    verify_observed_archive(root, &archive)?;
    verify_available_snapshots(root, &archive)?;
    if read_regular(root, &format!(".aic-local/transactions/{id}/journal.json"))? != journal_bytes {
        return Err(CoreError::conflict(
            "Recovery journal changed during publication",
        ));
    }
    // Never delete a transaction directory that could contain unknown files.
    // Retiring only its marker leaves all original inputs recoverable.
    let completed = dir.join(format!("journal-completed-{}", Uuid::new_v4()));
    atomic::move_no_replace(&dir.join("journal.json"), &completed)?;
    sync_dir(dir)?;
    Ok(None)
}

impl ProjectSession {
    pub fn resolve_recovery_conflict(&self, request_json: &str) -> Result<String> {
        let request: Request = serde_json::from_str(request_json)
            .map_err(|e| CoreError::invalid(format!("Invalid recovery request: {e}")))?;
        if request.schema_version != 1 {
            return Err(CoreError::new(
                "UNSUPPORTED_SCHEMA",
                "Unsupported recovery schema",
            ));
        }
        if !is_digest(&request.conflict_id) || !is_digest(&request.expected_disk_token) {
            return Err(CoreError::invalid(
                "Invalid recovery conflict or disk token",
            ));
        }
        let mut state = self.state()?;
        if state.local_generation != request.expected_local_generation {
            return Err(CoreError::conflict(
                "Local changes no longer match the recovery choice",
            ));
        }
        let next_generation = state.next_generation()?;
        {
            let _lock = lock(&self.path)?;
            let observed = scan(&self.path)?;
            let marker = file_bytes(&self.path, "project.json")?;
            if conflicts::disk_token(marker.as_deref(), &observed)? != request.expected_disk_token {
                return Err(CoreError::conflict(
                    "Disk changed since recovery was inspected",
                ));
            }
            let archive = load_archive(&self.path, &request.conflict_id)?;
            if archive.observed_disk_token != request.expected_disk_token
                || archive.observed_files != observed
                || archive.observed_marker_hash != marker.as_deref().map(hash)
            {
                return Err(CoreError::conflict(
                    "Recovery archive no longer matches disk",
                ));
            }
            let (id, dir, journal_bytes, journal) = active_journal(&self.path)?
                .ok_or_else(|| CoreError::conflict("The recovery journal is no longer active"))?;
            if id != archive.journal_id
                || hash(&journal_bytes) != archive.journal_hash
                || archive.snapshots.old.manifest_hash != journal.old_manifest_hash
                || archive.snapshots.new.manifest_hash != journal.new_manifest_hash
            {
                return Err(CoreError::conflict("Recovery journal changed"));
            }
            verify_observed_archive(&self.path, &archive)?;
            verify_available_snapshots(&self.path, &archive)?;
            let snapshot = selected(&archive, &request.take)?;
            let (target, _) = verify_snapshot(&self.path, &archive.id, &request.take, snapshot)?;
            let pairs = manifests(&dir, &journal)?;
            let expected = if request.take == "old" {
                &pairs[0].0
            } else {
                &pairs[1].0
            };
            if &target != expected {
                return Err(CoreError::conflict(
                    "Recovery snapshot does not match the active journal",
                ));
            }
            if state.dirty || !state.pending.is_empty() {
                conflicts::archive_failed(&self.path, &state, &observed, marker.as_deref())?;
            }
            if scan(&self.path)? != observed || file_bytes(&self.path, "project.json")? != marker {
                return Err(CoreError::conflict(
                    "Disk changed while preparing the recovery choice",
                ));
            }
            let intent = Intent {
                schema_version: 1,
                conflict_id: archive.id.clone(),
                journal_hash: archive.journal_hash.clone(),
                take: request.take,
            };
            let pending = dir.join(format!("choice-{}.pending", Uuid::new_v4()));
            durable_write(&pending, &encode(&intent)?)?;
            sync_dir(&dir)?;
            #[cfg(test)]
            super::interrupt_at(2000)?;
            #[cfg(test)]
            BEFORE_CHOICE.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            control::begin_commit()?;
            let choice = dir.join("recovery-choice.json");
            if choice.exists() {
                if !fs::symlink_metadata(&choice)?.file_type().is_file() {
                    return Err(CoreError::invalid("Recovery choice is not a regular file"));
                }
                atomic::swap(&pending, &choice)?;
            } else {
                atomic::move_no_replace(&pending, &choice)?;
            }
            sync_dir(&dir)?;
            #[cfg(test)]
            super::interrupt_at(2001)?;
            if resume(&self.path, &dir, &journal, &journal_bytes)?.is_some() {
                return Err(CoreError::conflict(
                    "Project changed while applying recovery",
                ));
            }
        }
        // Reuse the normal open path for document/history validation and for a
        // possible external change immediately after releasing the write lock.
        let fresh = crate::open_project(&self.path)?;
        let mut replacement = fresh
            .state
            .into_inner()
            .map_err(|_| CoreError::io("Project state lock poisoned"))?;
        replacement.local_generation = next_generation;
        *state = replacement;
        state.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{create_project, open_project};
    use serde_json::json;
    use std::sync::Arc;

    fn document(session: &ProjectSession) -> Value {
        serde_json::from_str(&session.snapshot_json().unwrap()).unwrap()
    }

    fn setup(missing_marker: bool) -> (tempfile::TempDir, PathBuf, ProjectSession) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let session = create_project(&path, "Example").unwrap();
        let state = document(&session);
        let root = &state["document"]["pages"][0]["rootId"];
        session.apply_transaction(&json!({
            "transactionId": Uuid::new_v4(),
            "baseRevision": state["document"]["revision"],
            "baseFingerprint": state["fingerprint"],
            "origin": "user", "scopeNodeIds": [root],
            "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Changed"}]
        }).to_string()).unwrap();
        super::super::INTERRUPT_AFTER.with(|point| point.set(Some(1)));
        assert!(session.save().is_err());
        drop(session);
        fs::write(path.join("document.json"), b"external document").unwrap();
        fs::write(path.join("src/user/external.txt"), b"external extra").unwrap();
        fs::write(path.join("outside-project-tree.txt"), b"outside data").unwrap();
        if missing_marker {
            fs::remove_file(path.join("project.json")).unwrap();
        } else {
            fs::write(path.join("project.json"), b"external marker").unwrap();
        }
        let session = open_project(&path).unwrap();
        assert_eq!(document(&session)["readOnly"], true);
        (temp, path, session)
    }

    fn diagnostics(session: &ProjectSession) -> (Value, Value) {
        let info: Value = serde_json::from_str(&session.diagnostics_json().unwrap()).unwrap();
        let conflict = info["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "recovery")
            .unwrap()
            .clone();
        (info, conflict)
    }

    fn request(info: &Value, conflict: &Value, take: &str) -> String {
        json!({
            "schemaVersion": 1, "conflictId": conflict["id"],
            "expectedDiskToken": info["observedDiskToken"],
            "expectedLocalGeneration": info["localGeneration"], "take": take
        })
        .to_string()
    }

    fn contains_bytes(root: &Path, expected: &[u8]) -> bool {
        walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|item| item.file_type().is_file())
            .any(|item| fs::read(item.path()).is_ok_and(|bytes| bytes == expected))
    }

    #[test]
    fn known_marker_with_external_edit_offers_whole_snapshot_recovery() {
        for point in [1, 4] {
            for take in ["old", "new"] {
                for crash_after_choice in [false, true] {
                    let temp = tempfile::tempdir().unwrap();
                    let path = temp.path().join("project");
                    let session = create_project(&path, "Example").unwrap();
                    let before = document(&session);
                    let root = &before["document"]["pages"][0]["rootId"];
                    session.apply_transaction(&json!({
                        "transactionId": Uuid::new_v4(),
                        "baseRevision": before["document"]["revision"],
                        "baseFingerprint": before["fingerprint"],
                        "origin": "user", "scopeNodeIds": [root],
                        "operations": [{"type":"setProperty","nodeId":root,"path":"/name","value":"Changed"}]
                    }).to_string()).unwrap();
                    super::super::INTERRUPT_AFTER.with(|slot| slot.set(Some(point)));
                    assert!(session.save().is_err());
                    drop(session);
                    let known_marker = fs::read(path.join("project.json")).unwrap();
                    fs::write(path.join("document.json"), b"known-marker external edit").unwrap();
                    let session = open_project(&path).unwrap();
                    assert_eq!(document(&session)["readOnly"], true);
                    let (info, conflict) = diagnostics(&session);
                    assert!(conflict["reason"]
                        .as_str()
                        .unwrap()
                        .contains("External changes"));
                    assert_eq!(fs::read(path.join("project.json")).unwrap(), known_marker);
                    if crash_after_choice {
                        super::super::INTERRUPT_AFTER.with(|slot| slot.set(Some(2001)));
                        assert!(session
                            .resolve_recovery_conflict(&request(&info, &conflict, take))
                            .is_err());
                        drop(session);
                        let reopened = open_project(&path).unwrap();
                        assert_eq!(document(&reopened)["readOnly"], false);
                        assert_eq!(
                            document(&reopened)["document"]["nodes"][0]["name"],
                            if take == "old" { "Frame" } else { "Changed" }
                        );
                    } else {
                        session
                            .resolve_recovery_conflict(&request(&info, &conflict, take))
                            .unwrap();
                        assert_eq!(document(&session)["readOnly"], false);
                        assert_eq!(
                            document(&session)["document"]["nodes"][0]["name"],
                            if take == "old" { "Frame" } else { "Changed" }
                        );
                    }
                    let manifest: Manifest =
                        serde_json::from_slice(&fs::read(path.join("project.json")).unwrap())
                            .unwrap();
                    assert_eq!(scan(&path).unwrap(), manifest.files);
                    assert!(contains_bytes(
                        &path.join("conflicts"),
                        b"known-marker external edit"
                    ));
                }
            }
        }
    }

    #[test]
    fn partial_retention_journal_is_retryable_and_old_partial_bytes_are_preserved() {
        for legacy_mode in 0..=2 {
            let (_temp, path, session) = setup(false);
            let (info, conflict) = diagnostics(&session);
            let retained = path
                .join(".aic-local/retained")
                .join(format!("choice-{}-new", conflict["id"].as_str().unwrap()));
            if legacy_mode != 0 {
                super::super::INTERRUPT_AFTER.with(|slot| slot.set(Some(4000)));
            } else {
                super::super::PARTIAL_JOURNAL_WRITE.with(|slot| slot.set(true));
            }
            assert!(session
                .resolve_recovery_conflict(&request(&info, &conflict, "new"))
                .is_err());
            let pending = fs::read_dir(&retained)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|entry| {
                    entry
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("journal-")
                })
                .unwrap();
            let pending_bytes = fs::read(&pending).unwrap();
            let partial = if legacy_mode == 1 {
                let prefix = pending_bytes[..pending_bytes.len() / 2].to_vec();
                fs::write(retained.join("journal.json"), &prefix).unwrap();
                prefix
            } else if legacy_mode == 2 {
                let complete = pending_bytes.strip_suffix(b"\n").unwrap().to_vec();
                fs::write(retained.join("journal.json"), &complete).unwrap();
                complete
            } else {
                assert!(!retained.join("journal.json").exists());
                pending_bytes
            };
            drop(session);
            let reopened = open_project(&path).unwrap();
            assert_eq!(document(&reopened)["readOnly"], false);
            assert_eq!(
                document(&reopened)["document"]["nodes"][0]["name"],
                "Changed"
            );
            assert!(contains_bytes(&retained, &partial));
            assert!(contains_bytes(
                &path.join("conflicts"),
                b"external document"
            ));
        }
    }

    #[test]
    fn recovery_choice_restores_only_the_whole_checked_snapshot_and_preserves_external_data() {
        for missing in [false, true] {
            for take in ["old", "new"] {
                let (_temp, path, session) = setup(missing);
                let (info, conflict) = diagnostics(&session);
                assert_eq!(conflict["snapshots"]["old"]["available"], true);
                assert_eq!(conflict["snapshots"]["new"]["available"], true);
                for version in ["old", "new"] {
                    for file in conflict["snapshots"][version]["files"].as_array().unwrap() {
                        let archived = path.join(file["archiveRelativePath"].as_str().unwrap());
                        assert_eq!(hash(&fs::read(archived).unwrap()), file["hash"]);
                    }
                }
                let expected_manifest = fs::read(
                    path.join(
                        conflict["snapshots"][take]["manifestPath"]
                            .as_str()
                            .unwrap(),
                    ),
                )
                .unwrap();
                let source = path
                    .join(".aic-local/transactions")
                    .join(conflict["journalId"].as_str().unwrap());
                fs::write(
                    source.join("third-party.txt"),
                    b"unrecognized transaction data",
                )
                .unwrap();
                let restored: Value = serde_json::from_str(
                    &session
                        .resolve_recovery_conflict(&request(&info, &conflict, take))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(restored["readOnly"], false);
                assert_eq!(
                    restored["document"]["nodes"][0]["name"],
                    if take == "old" { "Frame" } else { "Changed" }
                );
                assert_eq!(
                    fs::read(path.join("project.json")).unwrap(),
                    expected_manifest
                );
                let manifest: Manifest = serde_json::from_slice(&expected_manifest).unwrap();
                assert_eq!(scan(&path).unwrap(), manifest.files);
                assert_eq!(
                    fs::read(path.join("outside-project-tree.txt")).unwrap(),
                    b"outside data"
                );
                assert_eq!(
                    fs::read(source.join("third-party.txt")).unwrap(),
                    b"unrecognized transaction data"
                );
                assert!(contains_bytes(
                    &path.join("conflicts"),
                    b"external document"
                ));
                assert!(contains_bytes(&path.join("conflicts"), b"external extra"));
                if !missing {
                    assert!(contains_bytes(&path.join("conflicts"), b"external marker"));
                }
                drop(session);
                assert_eq!(document(&open_project(&path).unwrap())["readOnly"], false);
            }
        }
    }

    #[test]
    fn missing_marker_of_resolution_journal_requires_a_whole_snapshot_choice() {
        for take in ["old", "new"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("project");
            let session = create_project(&path, "Example").unwrap();
            fs::remove_file(path.join("project.json")).unwrap();
            let (base, mut next, observed, mut doc) = {
                let state = session.state().unwrap();
                (
                    state.manifest_bytes.clone(),
                    state.manifest.clone(),
                    scan(&path).unwrap(),
                    state.document.clone(),
                )
            };
            doc["revision"] = json!(1);
            doc["nodes"][0]["name"] = json!("Resolved");
            next.revision = 1;
            let changes = BTreeMap::from([("document.json".to_string(), encode(&doc).unwrap())]);
            super::super::INTERRUPT_AFTER.with(|point| point.set(Some(0)));
            assert!(super::super::commit_resolution(
                &path,
                &base,
                &next,
                &changes,
                &BTreeSet::new(),
                &observed,
                None
            )
            .is_err());
            drop(session);
            let session = open_project(&path).unwrap();
            assert_eq!(document(&session)["readOnly"], true);
            let (info, conflict) = diagnostics(&session);
            assert_eq!(conflict["snapshots"][take]["available"], true);
            session
                .resolve_recovery_conflict(&request(&info, &conflict, take))
                .unwrap();
            assert_eq!(
                document(&session)["document"]["nodes"][0]["name"],
                if take == "old" { "Frame" } else { "Resolved" }
            );
        }
    }

    #[test]
    fn stale_disk_or_generation_and_corrupt_archives_cannot_publish_a_choice() {
        let (_temp, path, session) = setup(false);
        let (info, conflict) = diagnostics(&session);
        let mut stale: Value = serde_json::from_str(&request(&info, &conflict, "new")).unwrap();
        stale["expectedLocalGeneration"] = json!(info["localGeneration"].as_u64().unwrap() + 1);
        assert_eq!(
            session
                .resolve_recovery_conflict(&stale.to_string())
                .unwrap_err()
                .code,
            "REVISION_CONFLICT"
        );
        fs::write(
            path.join("src/user/external.txt"),
            b"changed after inspection",
        )
        .unwrap();
        assert_eq!(
            session
                .resolve_recovery_conflict(&request(&info, &conflict, "new"))
                .unwrap_err()
                .code,
            "REVISION_CONFLICT"
        );
        assert_eq!(
            fs::read(path.join("src/user/external.txt")).unwrap(),
            b"changed after inspection"
        );

        let (info, conflict) = diagnostics(&session);
        let target = conflict["snapshots"]["new"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == "document.json")
            .unwrap();
        fs::write(
            path.join(target["archiveRelativePath"].as_str().unwrap()),
            b"tampered",
        )
        .unwrap();
        assert!(session
            .resolve_recovery_conflict(&request(&info, &conflict, "new"))
            .is_err());
        assert_eq!(
            fs::read(path.join("project.json")).unwrap(),
            b"external marker"
        );
        assert_eq!(
            fs::read(path.join("document.json")).unwrap(),
            b"external document"
        );
    }

    #[test]
    fn an_incomplete_snapshot_is_never_offered_or_invented() {
        let (_temp, path, session) = setup(false);
        let old_bytes = session.state().unwrap().manifest_bytes.clone();
        let old: Manifest = serde_json::from_slice(&old_bytes).unwrap();
        let digest = old.files.get(".gitignore").unwrap();
        fs::remove_file(path.join(".aic-local/baseline/v1/blobs").join(digest)).unwrap();
        fs::write(path.join(".gitignore"), b"external gitignore").unwrap();
        let (info, conflict) = diagnostics(&session);
        assert_eq!(conflict["snapshots"]["old"]["available"], false);
        assert_eq!(conflict["snapshots"]["new"]["available"], false);
        assert!(conflict["choices"].as_array().unwrap().is_empty());
        assert!(session
            .resolve_recovery_conflict(&request(&info, &conflict, "old"))
            .is_err());
        assert_eq!(
            fs::read(path.join(".gitignore")).unwrap(),
            b"external gitignore"
        );
    }

    #[test]
    fn interruption_before_and_after_choice_reopens_without_a_mixed_snapshot() {
        for take in ["old", "new"] {
            let (_temp, _path, session) = setup(false);
            let (_, conflict) = diagnostics(&session);
            let target: BTreeMap<String, String> = conflict["snapshots"][take]["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    (
                        entry["path"].as_str().unwrap().to_owned(),
                        entry["hash"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            let observed: BTreeMap<String, String> =
                serde_json::from_value(conflict["observedFiles"].clone()).unwrap();
            let paths: BTreeSet<_> = observed.keys().chain(target.keys()).cloned().collect();
            let mut points = vec![2000, 2001, 4000, 4001, 1000, 3000];
            points.extend(paths.iter().enumerate().filter_map(|(index, path)| {
                (observed.get(path) != target.get(path)).then_some(2002 + index)
            }));
            drop(session);
            for point in points {
                let (_temp, path, session) = setup(false);
                let (info, conflict) = diagnostics(&session);
                super::super::INTERRUPT_AFTER.with(|slot| slot.set(Some(point)));
                let error = session
                    .resolve_recovery_conflict(&request(&info, &conflict, take))
                    .unwrap_err();
                assert!(
                    error.message.contains("Simulated interruption"),
                    "point={point}: {}",
                    error.message
                );
                drop(session);
                let reopened = open_project(&path).unwrap();
                let snapshot = document(&reopened);
                if point == 2000 {
                    assert_eq!(snapshot["readOnly"], true);
                    assert_eq!(
                        fs::read(path.join("document.json")).unwrap(),
                        b"external document"
                    );
                    assert_eq!(
                        fs::read(path.join("project.json")).unwrap(),
                        b"external marker"
                    );
                } else {
                    assert_eq!(snapshot["readOnly"], false, "point={point}");
                    assert_eq!(
                        snapshot["document"]["nodes"][0]["name"],
                        if take == "old" { "Frame" } else { "Changed" }
                    );
                    let marker: Manifest =
                        serde_json::from_slice(&fs::read(path.join("project.json")).unwrap())
                            .unwrap();
                    assert_eq!(scan(&path).unwrap(), marker.files);
                }
                assert!(contains_bytes(
                    &path.join("conflicts"),
                    b"external document"
                ));
                assert!(contains_bytes(&path.join("conflicts"), b"external extra"));
            }
        }
    }

    #[test]
    fn cancellation_before_choice_does_not_hide_the_original_journal() {
        use crate::cancellation::{CancelOutcome, OperationControl, OperationStatus};
        let (_temp, path, session) = setup(false);
        let (info, conflict) = diagnostics(&session);
        let control = Arc::new(OperationControl::new());
        let cancellation = control.clone();
        BEFORE_CHOICE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert_eq!(cancellation.request_cancel(), CancelOutcome::Accepted);
            }))
        });
        assert_eq!(
            session
                .resolve_recovery_conflict_controlled(
                    &request(&info, &conflict, "new"),
                    control.clone()
                )
                .unwrap_err()
                .code,
            "CANCELED"
        );
        assert_eq!(control.status(), OperationStatus::Canceled);
        drop(session);
        assert_eq!(document(&open_project(&path).unwrap())["readOnly"], true);
        assert_eq!(
            fs::read(path.join("document.json")).unwrap(),
            b"external document"
        );
        assert_eq!(
            fs::read(path.join("project.json")).unwrap(),
            b"external marker"
        );
    }

    #[test]
    fn cancellation_after_choice_publication_is_too_late() {
        use crate::cancellation::{CancelOutcome, OperationControl, OperationStatus};
        let (_temp, _path, session) = setup(false);
        let (info, conflict) = diagnostics(&session);
        let control = Arc::new(OperationControl::new());
        let cancellation = control.clone();
        super::super::BEFORE_REPLACE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                assert_eq!(cancellation.request_cancel(), CancelOutcome::TooLate);
            }))
        });
        session
            .resolve_recovery_conflict_controlled(
                &request(&info, &conflict, "new"),
                control.clone(),
            )
            .unwrap();
        assert_eq!(control.status(), OperationStatus::Succeeded);
        assert_eq!(
            document(&session)["document"]["nodes"][0]["name"],
            "Changed"
        );
    }

    #[test]
    fn external_race_is_retained_and_reopen_stays_read_only() {
        let (_temp, path, session) = setup(false);
        let (info, conflict) = diagnostics(&session);
        let document_path = path.join("document.json");
        super::super::BEFORE_REPLACE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::write(document_path, b"racing external document").unwrap();
            }))
        });
        assert!(session
            .resolve_recovery_conflict(&request(&info, &conflict, "old"))
            .is_err());
        assert!(contains_bytes(
            &path.join(".aic-local/retained"),
            b"racing external document"
        ));
        drop(session);
        assert_eq!(document(&open_project(&path).unwrap())["readOnly"], true);
    }

    #[test]
    fn retained_descriptor_writes_remain_diagnosable_after_recovery() {
        use std::io::Write;
        let (_temp, path, session) = setup(false);
        let mut descriptor = fs::OpenOptions::new()
            .write(true)
            .open(path.join("document.json"))
            .unwrap();
        let (info, conflict) = diagnostics(&session);
        session
            .resolve_recovery_conflict(&request(&info, &conflict, "old"))
            .unwrap();
        descriptor.set_len(0).unwrap();
        descriptor.write_all(b"late external write").unwrap();
        descriptor.sync_all().unwrap();
        assert!(contains_bytes(
            &path.join(".aic-local/retained"),
            b"late external write"
        ));
        let info: Value = serde_json::from_str(&session.diagnostics_json().unwrap()).unwrap();
        assert!(info["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "retainedWrite"));
    }

    #[cfg(unix)]
    #[test]
    fn process_death_after_choice_and_swap_replays_the_chosen_snapshot() {
        const CHILD: &str = "AIC_TEST_RECOVERY_CHOICE_CHILD";
        if let Some(path) = std::env::var_os(CHILD) {
            let session = open_project(path).unwrap();
            let (info, conflict) = diagnostics(&session);
            let point: usize = std::env::var("AIC_TEST_RECOVERY_CHOICE_POINT")
                .unwrap()
                .parse()
                .unwrap();
            super::super::KILL_AFTER.with(|slot| slot.set(Some(point)));
            let _ = session.resolve_recovery_conflict(&request(&info, &conflict, "new"));
            panic!("child did not reach the kill point");
        }
        for point in [2001, 4000, 4001, 1000, 3000] {
            let (_temp, path, session) = setup(false);
            drop(session);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "storage::recovery_choice::tests::process_death_after_choice_and_swap_replays_the_chosen_snapshot"])
                .env(CHILD, &path).env("AIC_TEST_RECOVERY_CHOICE_POINT", point.to_string())
                .status().unwrap();
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(libc::SIGKILL), "point={point}");
            let reopened = open_project(&path).unwrap();
            assert_eq!(document(&reopened)["readOnly"], false);
            assert_eq!(
                document(&reopened)["document"]["nodes"][0]["name"],
                "Changed"
            );
            assert!(contains_bytes(
                &path.join("conflicts"),
                b"external document"
            ));
        }
    }
}
