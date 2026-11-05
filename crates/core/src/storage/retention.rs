//! Opt-in retirement of verified, completed retained save transactions.
//!
//! A receipt is written only after the recovery journal has been retired. Old
//! transactions without such a receipt are deliberately never inferred to be
//! complete. Moving a candidate to a tombstone is atomic; its receipt remains
//! until every listed file has been removed, so an interrupted deletion can be
//! resumed without interpreting an incomplete recovery journal.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{atomic, hash, hash_file, json, lock, sync_dir, Journal, Manifest};
use crate::{CoreError, Result};

pub(crate) const UNLIMITED: u64 = u64::MAX;
const RECEIPT: &str = "completed.json";
const TOMBSTONE: &str = ".pruning-";

#[cfg(test)]
thread_local! {
    pub(super) static INTERRUPT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    #[cfg(unix)]
    pub(super) static KILL: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    version: u32,
    transaction_id: Uuid,
    project_id: Uuid,
    generation: u64,
    journal: String,
    new_manifest: String,
    files: BTreeMap<String, String>,
    directories: BTreeSet<String>,
}

fn regular_hash(path: &Path) -> Result<String> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(CoreError::invalid("Retained item is not a regular file"));
    }
    hash_file(path)
}

fn canonical_uuid(name: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(name).ok()?;
    (id.to_string() == name).then_some(id)
}

fn directory(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(CoreError::invalid("Retained item is not a directory"));
    }
    Ok(())
}

fn add_slots(
    root: &Path,
    name: &str,
    old: Option<&str>,
    files: &mut BTreeMap<String, String>,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    let path = root.join(name);
    directory(&path)?;
    directories.insert(name.to_owned());
    let mut slots = fs::read_dir(path)?;
    match (old, slots.next().transpose()?) {
        (None, None) => {}
        (Some(digest), Some(slot)) if slots.next().is_none() => {
            let name_part = slot.file_name();
            let name_part = name_part
                .to_str()
                .ok_or_else(|| CoreError::invalid("Invalid retained slot name"))?;
            if canonical_uuid(name_part).is_none() || !slot.file_type()?.is_file() {
                return Err(CoreError::invalid("Unknown retained slot"));
            }
            files.insert(format!("{name}/{name_part}"), digest.to_owned());
        }
        _ => return Err(CoreError::invalid("Unexpected retained slots")),
    }
    Ok(())
}

/// Called under the project lock, only after the active journal has gone away.
pub(super) fn record(retained: &Path, journal: &Journal, new: &Manifest) -> Result<()> {
    let name = retained
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CoreError::invalid("Invalid retained transaction"))?;
    let transaction_id = canonical_uuid(name)
        .ok_or_else(|| CoreError::invalid("Invalid retained transaction ID"))?;
    directory(retained)?;
    let journal_bytes = fs::read(retained.join("journal.json"))?;
    if journal_bytes != json(journal)? || new.storage_generation != journal.new_generation {
        return Err(CoreError::invalid(
            "Retained journal does not match the completed save",
        ));
    }
    let mut files = BTreeMap::from([
        ("journal.json".into(), hash(&journal_bytes)),
        ("old-manifest".into(), journal.old_manifest_hash.clone()),
        ("new-manifest".into(), journal.new_manifest_hash.clone()),
    ]);
    if let Some(digest) = journal
        .observed_marker
        .as_ref()
        .and_then(|m| m.hash.as_ref())
    {
        files.insert("observed-manifest".into(), digest.clone());
    }
    let mut directories = BTreeSet::new();
    for (index, entry) in journal.entries.iter().enumerate() {
        for (prefix, name, digest) in [
            ("old", &entry.old_copy, &entry.old_hash),
            ("new", &entry.new_copy, &entry.new_hash),
        ] {
            match (name, digest) {
                (Some(name), Some(digest)) if *name == format!("{prefix}-{index}") => {
                    files.insert(name.clone(), digest.clone());
                }
                (None, None) => {}
                _ => return Err(CoreError::invalid("Invalid retained journal copy")),
            }
        }
        add_slots(
            retained,
            &format!("entry-{index}"),
            entry.old_hash.as_deref(),
            &mut files,
            &mut directories,
        )?;
    }
    let old_marker = journal
        .observed_marker
        .as_ref()
        .map(|m| m.hash.as_deref())
        .unwrap_or(Some(journal.old_manifest_hash.as_str()));
    add_slots(
        retained,
        "manifest",
        old_marker,
        &mut files,
        &mut directories,
    )?;
    let receipt = Receipt {
        version: 1,
        transaction_id,
        project_id: new.project_id,
        generation: new.storage_generation,
        journal: String::from_utf8(journal_bytes).map_err(|e| CoreError::invalid(e.to_string()))?,
        new_manifest: String::from_utf8(fs::read(retained.join("new-manifest"))?)
            .map_err(|e| CoreError::invalid(e.to_string()))?,
        files,
        directories,
    };
    inspect(retained, &receipt, false, false)?;
    let pending = retained.join("completed.pending");
    super::durable_write(&pending, &json(&receipt)?)?;
    sync_dir(retained)?;
    atomic::move_no_replace(&pending, &retained.join(RECEIPT))?;
    sync_dir(retained)
}

fn valid_receipt(receipt: &Receipt, id: Uuid, project: Uuid) -> bool {
    if receipt.version != 1
        || receipt.transaction_id != id
        || receipt.project_id != project
        || receipt.generation == 0
    {
        return false;
    }
    let Ok(journal) = serde_json::from_str::<Journal>(&receipt.journal) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Manifest>(&receipt.new_manifest) else {
        return false;
    };
    if json(&journal).ok().as_deref() != Some(receipt.journal.as_bytes())
        || json(&manifest).ok().as_deref() != Some(receipt.new_manifest.as_bytes())
        || manifest.project_id != project
        || manifest.storage_generation != receipt.generation
        || journal.new_generation != receipt.generation
        || journal.old_generation.checked_add(1) != Some(receipt.generation)
        || journal.new_manifest_hash != hash(receipt.new_manifest.as_bytes())
    {
        return false;
    }
    let mut expected = BTreeMap::from([
        ("journal.json".to_owned(), hash(receipt.journal.as_bytes())),
        ("old-manifest".to_owned(), journal.old_manifest_hash.clone()),
        ("new-manifest".to_owned(), journal.new_manifest_hash.clone()),
    ]);
    if let Some(digest) = journal
        .observed_marker
        .as_ref()
        .and_then(|marker| marker.hash.as_ref())
    {
        expected.insert("observed-manifest".into(), digest.clone());
    }
    let mut slots = BTreeMap::new();
    for (index, entry) in journal.entries.iter().enumerate() {
        for (prefix, name, digest) in [
            ("old", &entry.old_copy, &entry.old_hash),
            ("new", &entry.new_copy, &entry.new_hash),
        ] {
            match (name, digest) {
                (Some(name), Some(digest)) if *name == format!("{prefix}-{index}") => {
                    expected.insert(name.clone(), digest.clone());
                }
                (None, None) => {}
                _ => return false,
            }
        }
        slots.insert(format!("entry-{index}"), entry.old_hash.clone());
    }
    let marker = journal
        .observed_marker
        .as_ref()
        .map(|m| m.hash.clone())
        .unwrap_or(Some(journal.old_manifest_hash.clone()));
    slots.insert("manifest".into(), marker);
    if receipt.directories != slots.keys().cloned().collect() {
        return false;
    }
    let mut seen = BTreeSet::new();
    for (name, digest) in &receipt.files {
        if let Some((dir, file)) = name.split_once('/') {
            let Some(Some(old)) = slots.get(dir) else {
                return false;
            };
            if canonical_uuid(file).is_none() || digest != old || !seen.insert(dir) {
                return false;
            }
            expected.insert(name.clone(), digest.clone());
        }
    }
    if slots
        .iter()
        .any(|(dir, old)| old.is_some() != seen.contains(dir.as_str()))
    {
        return false;
    }
    receipt.files.values().all(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) && receipt.files == expected
}

/// Exact set for a completed record; a tombstone may be missing listed items.
fn inspect(path: &Path, receipt: &Receipt, partial: bool, has_receipt: bool) -> Result<()> {
    directory(path)?;
    let mut seen_files = BTreeSet::new();
    let mut seen_dirs = BTreeSet::new();
    for entry in walkdir::WalkDir::new(path).min_depth(1).follow_links(false) {
        let entry = entry.map_err(|e| CoreError::io(e.to_string()))?;
        let relative = entry
            .path()
            .strip_prefix(path)
            .map_err(|_| CoreError::invalid("Invalid retained path"))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| CoreError::invalid("Invalid retained path"))?;
        if has_receipt && relative == RECEIPT {
            if !entry.file_type().is_file() {
                return Err(CoreError::invalid("Invalid retention receipt"));
            }
            continue;
        }
        if entry.file_type().is_dir() && receipt.directories.contains(relative) {
            seen_dirs.insert(relative.to_owned());
        } else if entry.file_type().is_file() {
            let expected = receipt
                .files
                .get(relative)
                .ok_or_else(|| CoreError::invalid("Unknown retained file"))?;
            if regular_hash(entry.path())? != *expected {
                return Err(CoreError::conflict("Retained file changed"));
            }
            seen_files.insert(relative.to_owned());
        } else {
            return Err(CoreError::invalid("Unknown retained item"));
        }
    }
    if !partial
        && (seen_dirs != receipt.directories
            || seen_files != receipt.files.keys().cloned().collect())
    {
        return Err(CoreError::invalid("Incomplete retained transaction"));
    }
    Ok(())
}

fn receipt(path: &Path, id: Uuid, project: Uuid) -> Result<Receipt> {
    let file = path.join(RECEIPT);
    if !fs::symlink_metadata(&file)?.file_type().is_file() {
        return Err(CoreError::invalid("Invalid retention receipt"));
    }
    let receipt: Receipt =
        serde_json::from_slice(&fs::read(file)?).map_err(|e| CoreError::invalid(e.to_string()))?;
    if !valid_receipt(&receipt, id, project) {
        return Err(CoreError::invalid("Invalid retention receipt"));
    }
    Ok(receipt)
}

fn interrupted(point: usize) -> Result<()> {
    #[cfg(all(test, unix))]
    KILL.with(|p| {
        if p.get() == Some(point) {
            // Simulate sudden process death without destructors.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
        }
    });
    #[cfg(test)]
    if INTERRUPT.with(|p| p.get() == Some(point)) {
        INTERRUPT.with(|p| p.set(None));
        return Err(CoreError::io("Simulated retention interruption"));
    }
    let _ = point;
    Ok(())
}

fn remove_tombstone(
    path: &Path,
    receipt: &Receipt,
    policy: &AtomicU64,
    limit: u64,
    validated: bool,
) -> Result<()> {
    if !validated {
        inspect(path, receipt, true, true)?;
    }
    let mut step = 1;
    for (name, expected) in &receipt.files {
        if policy.load(Ordering::SeqCst) != limit {
            return Ok(());
        }
        let target = path.join(name);
        match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        if regular_hash(&target)? != *expected {
            return Err(CoreError::conflict("Retained file changed during cleanup"));
        }
        fs::remove_file(&target)?;
        sync_dir(
            target
                .parent()
                .ok_or_else(|| CoreError::invalid("Invalid retention path"))?,
        )?;
        interrupted(step)?;
        step += 1;
    }
    for name in &receipt.directories {
        if policy.load(Ordering::SeqCst) != limit {
            return Ok(());
        }
        match fs::remove_dir(path.join(name)) {
            Ok(()) => sync_dir(path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    if policy.load(Ordering::SeqCst) != limit {
        return Ok(());
    }
    // No unlisted item is removed: remove_dir fails if something appeared.
    let mut remaining = fs::read_dir(path)?;
    let only_receipt = remaining
        .next()
        .transpose()?
        .is_some_and(|e| e.file_name() == RECEIPT)
        && remaining.next().is_none();
    if !only_receipt {
        return Err(CoreError::invalid("Unknown item in retention tombstone"));
    }
    fs::remove_file(path.join(RECEIPT))?;
    sync_dir(path)?;
    fs::remove_dir(path)?;
    sync_dir(
        path.parent()
            .ok_or_else(|| CoreError::invalid("Invalid retention directory"))?,
    )
}

/// Best-effort cleanup after an actual save, never on open or on setting change.
pub(crate) fn prune(root: &Path, project: Uuid, policy: &AtomicU64) -> Result<()> {
    let limit = policy.load(Ordering::SeqCst);
    if limit == UNLIMITED {
        return Ok(());
    }
    let _guard = lock(root)?;
    if policy.load(Ordering::SeqCst) != limit {
        return Ok(());
    }
    let conflicts = root.join("conflicts");
    match fs::symlink_metadata(&conflicts) {
        Ok(meta) if meta.file_type().is_dir() => {
            if fs::read_dir(conflicts)?.next().transpose()?.is_some() {
                return Ok(());
            }
        }
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let base = root.join(".aic-local/retained");
    directory(&base)?;
    let transactions = root.join(".aic-local/transactions");
    directory(&transactions)?;
    let mut candidates: Vec<(u64, String, PathBuf)> = Vec::new();
    let mut tombstones = Vec::new();
    for entry in fs::read_dir(&base)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let (is_tombstone, id_name) = name
            .strip_prefix(TOMBSTONE)
            .map_or((false, name), |s| (true, s));
        let Some(id) = canonical_uuid(id_name) else {
            continue;
        };
        // Never retire or resume deletion for an active or unretired journal.
        match fs::symlink_metadata(transactions.join(id_name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => continue,
        }
        let Ok(record) = receipt(&entry.path(), id, project) else {
            continue;
        };
        if is_tombstone {
            tombstones.push((entry.path(), record));
        } else {
            candidates.push((record.generation, id_name.to_owned(), entry.path()));
        }
    }
    for (path, record) in tombstones {
        if policy.load(Ordering::SeqCst) != limit {
            return Ok(());
        }
        remove_tombstone(&path, &record, policy, limit, false)?;
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let excess = candidates
        .len()
        .saturating_sub(usize::try_from(limit).unwrap_or(usize::MAX));
    for (_, id, path) in candidates.into_iter().take(excess) {
        if policy.load(Ordering::SeqCst) != limit {
            return Ok(());
        }
        let id = canonical_uuid(&id)
            .ok_or_else(|| CoreError::invalid("Invalid retained transaction ID"))?;
        let record = match receipt(&path, id, project) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if inspect(&path, &record, false, true).is_err() {
            continue;
        }
        if policy.load(Ordering::SeqCst) != limit {
            return Ok(());
        }
        let tombstone = base.join(format!("{TOMBSTONE}{id}"));
        atomic::move_no_replace(&path, &tombstone)?;
        sync_dir(&base)?;
        interrupted(0)?;
        remove_tombstone(&tombstone, &record, policy, limit, true)?;
    }
    Ok(())
}
