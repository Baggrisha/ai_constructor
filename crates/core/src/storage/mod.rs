use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{CoreError, Result};

mod atomic;

#[cfg(test)]
mod tests;

#[cfg(test)]
thread_local! {
    static INTERRUPT_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static PARTIAL_JOURNAL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PARTIAL_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AFTER_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static AFTER_CONFLICT_SCAN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_REPLACE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static BEFORE_MANIFEST_REPLACE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    #[cfg(unix)]
    static KILL_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn interrupt_at(step: usize) -> Result<()> {
    #[cfg(unix)]
    KILL_AFTER.with(|point| {
        if point.get() == Some(step) {
            // Test a process death without running destructors or creating a core dump.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
        }
    });
    INTERRUPT_AFTER.with(|point| {
        if point.get() == Some(step) {
            point.set(None);
            Err(CoreError::io("Simulated interruption"))
        } else {
            Ok(())
        }
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub project_id: Uuid,
    pub name: String,
    pub revision: u64,
    pub storage_generation: u64,
    pub snapshot_hash: String,
    pub files: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    path: String,
    old_hash: Option<String>,
    new_hash: Option<String>,
    old_copy: Option<String>,
    new_copy: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    old_manifest_hash: String,
    new_manifest_hash: String,
    old_revision: u64,
    new_revision: u64,
    old_generation: u64,
    new_generation: u64,
    entries: Vec<Entry>,
}

pub struct Loaded {
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    pub document_bytes: Vec<u8>,
    pub read_only_reason: Option<String>,
}

pub fn json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| CoreError::invalid(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn fingerprint(document: &[u8], files: &BTreeMap<String, String>) -> String {
    let mut digest = Sha256::new();
    digest.update(document);
    for (path, value) in files {
        if path.starts_with("src/user/") || path.starts_with("assets/") {
            digest.update((path.len() as u64).to_be_bytes());
            digest.update(path.as_bytes());
            digest.update(value.as_bytes());
        }
    }
    format!("{:x}", digest.finalize())
}

fn lock(root: &Path) -> Result<File> {
    let local = root.join(".aic-local");
    if let Ok(meta) = fs::symlink_metadata(&local) {
        if meta.file_type().is_symlink() {
            return Err(CoreError::invalid(
                "Local project directory must not be a symlink",
            ));
        }
    }
    fs::create_dir_all(&local)?;
    if let Ok(meta) = fs::symlink_metadata(local.join("write.lock")) {
        if meta.file_type().is_symlink() {
            return Err(CoreError::invalid("Project lock must not be a symlink"));
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".aic-local/write.lock"))?;
    file.lock_exclusive()?;
    Ok(file)
}

fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
        || relative.is_empty()
    {
        return Err(CoreError::invalid("Unsafe project file path"));
    }
    let mut current = root.to_path_buf();
    for part in path.components() {
        current.push(part.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(CoreError::invalid("Project symlinks are not supported"))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(current)
}

fn file_bytes(root: &Path, relative: &str) -> Result<Option<Vec<u8>>> {
    match fs::read(safe_path(root, relative)?) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn file_hash(root: &Path, relative: &str) -> Result<Option<String>> {
    Ok(file_bytes(root, relative)?.as_deref().map(hash))
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn durable_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    #[cfg(test)]
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("journal"))
        && PARTIAL_JOURNAL_WRITE.with(|flag| flag.replace(false))
    {
        file.write_all(&bytes[..bytes.len() / 2])?;
        file.sync_all()?;
        return Err(CoreError::io("Simulated partial journal write"));
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn remove_transaction(dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if PARTIAL_CLEANUP.with(|flag| flag.replace(false)) {
        fs::remove_file(dir.join("old-manifest"))?;
        return Err(std::io::Error::other(
            "Simulated partial transaction cleanup",
        ));
    }
    fs::remove_dir_all(dir)
}

// Retire the recovery marker durably before deleting any files it refers to.
fn retire_transaction(dir: &Path) -> Result<()> {
    fs::remove_file(dir.join("journal.json"))?;
    sync_dir(dir)?;
    remove_transaction(dir)?;
    sync_dir(
        dir.parent()
            .ok_or_else(|| CoreError::invalid("Invalid transaction directory"))?,
    )
}

// Each published transaction keeps its journal and all displaced inodes until
// an operator can establish that no external file descriptors remain open.
fn retained_transaction(root: &Path, txdir: &Path, journal: &Journal) -> Result<PathBuf> {
    let local = root.join(".aic-local");
    let base = local.join("retained");
    let retained = base.join(
        txdir
            .file_name()
            .ok_or_else(|| CoreError::invalid("Invalid transaction directory"))?,
    );
    fs::create_dir_all(&retained)?;
    sync_dir(&local)?;
    sync_dir(&base)?;
    let names = ["journal.json", "old-manifest", "new-manifest"]
        .into_iter()
        .chain(journal.entries.iter().flat_map(|entry| {
            [entry.old_copy.as_deref(), entry.new_copy.as_deref()]
                .into_iter()
                .flatten()
        }));
    for name in names {
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(CoreError::invalid("Invalid journal copy path"));
        }
        let source = txdir.join(name);
        let archived = retained.join(name);
        if !fs::symlink_metadata(&source)?.file_type().is_file() {
            return Err(CoreError::invalid("Invalid retained transaction file"));
        }
        match fs::hard_link(&source, &archived) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let source = fs::symlink_metadata(&source)?;
                    let archived = fs::symlink_metadata(&archived)?;
                    if !archived.file_type().is_file()
                        || source.dev() != archived.dev()
                        || source.ino() != archived.ino()
                    {
                        return Err(CoreError::conflict("Retained transaction file changed"));
                    }
                }
                #[cfg(not(unix))]
                return Err(CoreError::unsupported(
                    "Cannot verify retained transaction files on this platform",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    }
    sync_dir(&retained)?;
    Ok(retained)
}

fn unexpected_retained(dir: &Path, old: Option<&str>, new: Option<&str>) -> Result<bool> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Ok(true);
        }
        let found = hash(&fs::read(entry.path())?);
        if Some(found.as_str()) != old && Some(found.as_str()) != new {
            return Ok(true);
        }
    }
    Ok(false)
}

fn replace_with(
    root: &Path,
    relative: &str,
    expected: Option<&str>,
    bytes: Option<&[u8]>,
    retained: &Path,
) -> Result<()> {
    let path = safe_path(root, relative)?;
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::invalid("Invalid file path"))?;
    fs::create_dir_all(parent)?;
    fs::create_dir_all(retained)?;
    sync_dir(
        retained
            .parent()
            .ok_or_else(|| CoreError::invalid("Invalid retention directory"))?,
    )?;
    let slot = retained.join(Uuid::new_v4().to_string());
    if let Some(bytes) = bytes {
        let local = root.join(".aic-local");
        let temp = local.join(format!("replace-{}.tmp", Uuid::new_v4()));
        durable_write(&temp, bytes)?;
        sync_dir(&local)?;
        atomic::move_no_replace(&temp, &slot)?;
        sync_dir(&local)?;
        sync_dir(retained)?;
        if expected.is_some() {
            atomic::swap(&slot, &path)?;
        } else {
            atomic::move_no_replace(&slot, &path)?;
        }
    } else if expected.is_some() {
        atomic::move_no_replace(&path, &slot)?;
    } else {
        return Ok(());
    }
    #[cfg(test)]
    interrupt_at(1000)?;
    sync_dir(retained)?;
    sync_dir(parent)?;
    if let Some(expected) = expected {
        if !fs::symlink_metadata(&slot)?.file_type().is_file()
            || hash(&fs::read(&slot)?) != expected
        {
            return Err(CoreError::conflict(format!(
                "External edit of {relative} preserved at {}",
                slot.display()
            )));
        }
    }
    Ok(())
}

fn scan(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    for base in [
        ".gitignore",
        "document.json",
        "src",
        "assets",
        "chats",
        "agent",
        "history",
        "checkpoints",
    ] {
        let path = root.join(base);
        if !path.exists() {
            continue;
        }
        for entry in WalkDir::new(&path).follow_links(false) {
            let entry = entry.map_err(|e| CoreError::io(e.to_string()))?;
            if entry.file_type().is_symlink() {
                return Err(CoreError::invalid("Project symlinks are not supported"));
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|e| CoreError::invalid(e.to_string()))?;
            let name = relative
                .to_str()
                .ok_or_else(|| CoreError::invalid("Non-UTF-8 project path"))?
                .replace('\\', "/");
            files.insert(name, hash(&fs::read(entry.path())?));
        }
    }
    Ok(files)
}

pub fn create(root: &Path, manifest: &Manifest, document: &[u8]) -> Result<Manifest> {
    fs::create_dir(root)?;
    for dir in [
        "src/user",
        "src/generated",
        "assets",
        "chats",
        "agent/runs",
        "agent/proposals",
        "history/transactions",
        "checkpoints",
        "conflicts",
        ".aic-local",
        ".cache",
    ] {
        fs::create_dir_all(root.join(dir))?;
    }
    durable_write(&root.join("document.json"), document)?;
    durable_write(
        &root.join(".gitignore"),
        b".aic-local/\n.cache/\n.DS_Store\n.env\n.env.*\n",
    )?;
    let mut manifest = manifest.clone();
    manifest.files = scan(root)?;
    manifest.snapshot_hash = fingerprint(document, &manifest.files);
    durable_write(&root.join("project.json"), &json(&manifest)?)?;
    sync_dir(root)?;
    Ok(manifest)
}

fn verify_backup(dir: &Path, name: &str, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(dir.join(name))?;
    if hash(&bytes) != expected {
        return Err(CoreError::invalid("Transaction backup hash mismatch"));
    }
    Ok(bytes)
}

fn preserve_conflict(root: &Path, dir: &Path, journal: &Journal, reason: &str) -> Result<()> {
    let conflict = root.join("conflicts").join(Uuid::new_v4().to_string());
    fs::create_dir_all(&conflict)?;
    for name in ["old-manifest", "new-manifest", "journal.json"] {
        durable_write(&conflict.join(name), &fs::read(dir.join(name))?)?;
    }
    if let Some(bytes) = file_bytes(root, "project.json")? {
        durable_write(&conflict.join("current-manifest"), &bytes)?;
    }
    for (i, entry) in journal.entries.iter().enumerate() {
        if let Some(bytes) = file_bytes(root, &entry.path)? {
            durable_write(&conflict.join(format!("{i}-current")), &bytes)?;
        }
        if let Some(name) = &entry.old_copy {
            durable_write(
                &conflict.join(format!("{i}-old")),
                &fs::read(dir.join(name))?,
            )?;
        }
        if let Some(name) = &entry.new_copy {
            durable_write(
                &conflict.join(format!("{i}-new")),
                &fs::read(dir.join(name))?,
            )?;
        }
    }
    let old: Manifest = serde_json::from_slice(&fs::read(dir.join("old-manifest"))?)
        .map_err(|e| CoreError::invalid(e.to_string()))?;
    let new: Manifest = serde_json::from_slice(&fs::read(dir.join("new-manifest"))?)
        .map_err(|e| CoreError::invalid(e.to_string()))?;
    let observed = scan(root)?;
    #[cfg(test)]
    AFTER_CONFLICT_SCAN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    let paths: BTreeSet<_> = old
        .files
        .keys()
        .chain(new.files.keys())
        .chain(observed.keys())
        .collect();
    let mut external = Vec::new();
    for path in paths {
        let current = observed.get(path);
        if current != old.files.get(path) && current != new.files.get(path) {
            let (copy, copied_hash) = if let Some(bytes) = file_bytes(root, path)? {
                let name = format!("external-{}", external.len());
                let copied_hash = hash(&bytes);
                durable_write(&conflict.join(&name), &bytes)?;
                (Some(name), Some(copied_hash))
            } else {
                (None, None)
            };
            external.push(serde_json::json!({"path": path, "copy": copy, "hash": copied_hash}));
        }
    }
    durable_write(
        &conflict.join("metadata.json"),
        &json(&serde_json::json!({
            "reason": reason, "journal": dir.file_name().and_then(|s| s.to_str()), "externalFiles": external
        }))?,
    )?;
    sync_dir(&conflict)?;
    sync_dir(&root.join("conflicts"))?;
    Ok(())
}

// The lock is held by the caller. Prepared copies are never installed unless every
// touched destination is still one of the two journalled versions.
fn recover(root: &Path) -> Result<Option<Loaded>> {
    let transactions = root.join(".aic-local/transactions");
    if !transactions.exists() {
        return Ok(None);
    }
    let mut dirs: Vec<_> = fs::read_dir(&transactions)?.collect::<std::io::Result<Vec<_>>>()?;
    dirs.sort_by_key(|e| e.file_name());
    for item in dirs {
        if !item.file_type()?.is_dir() {
            continue;
        }
        let dir = item.path();
        let journal_path = dir.join("journal.json");
        if !journal_path.exists() {
            continue;
        } // incomplete preparation, never published
        let journal: Journal = serde_json::from_slice(&fs::read(&journal_path)?)
            .map_err(|e| CoreError::invalid(format!("Invalid recovery journal: {e}")))?;
        let old_manifest = verify_backup(&dir, "old-manifest", &journal.old_manifest_hash)?;
        let new_manifest = verify_backup(&dir, "new-manifest", &journal.new_manifest_hash)?;
        let old: Manifest =
            serde_json::from_slice(&old_manifest).map_err(|e| CoreError::invalid(e.to_string()))?;
        let new: Manifest =
            serde_json::from_slice(&new_manifest).map_err(|e| CoreError::invalid(e.to_string()))?;
        if old.revision != journal.old_revision
            || new.revision != journal.new_revision
            || old.storage_generation != journal.old_generation
            || new.storage_generation != journal.new_generation
            || old.project_id != new.project_id
        {
            return Err(CoreError::invalid(
                "Journal manifest metadata does not match",
            ));
        }
        let retained = retained_transaction(root, &dir, &journal)?;
        let marker = file_hash(root, "project.json")?;
        let forward = marker.as_deref() == Some(journal.new_manifest_hash.as_str());
        let backward = marker.as_deref() == Some(journal.old_manifest_hash.as_str());
        let mut reason = if !forward && !backward {
            Some("Manifest does not match either saved version")
        } else {
            None
        };
        if unexpected_retained(
            &retained.join("manifest"),
            Some(&journal.old_manifest_hash),
            Some(&journal.new_manifest_hash),
        )? {
            reason = Some("External change retained in .aic-local/retained; resolve manually");
        }
        for (index, entry) in journal.entries.iter().enumerate() {
            if unexpected_retained(
                &retained.join(format!("entry-{index}")),
                entry.old_hash.as_deref(),
                entry.new_hash.as_deref(),
            )? {
                reason = Some("External change retained in .aic-local/retained; resolve manually");
            }
            let allowed = entry.path == "document.json"
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
                .any(|prefix| entry.path.starts_with(prefix));
            if !allowed {
                return Err(CoreError::invalid("Invalid journal destination"));
            }
            for copy in [&entry.old_copy, &entry.new_copy].into_iter().flatten() {
                if Path::new(copy).components().count() != 1
                    || !matches!(
                        Path::new(copy).components().next(),
                        Some(Component::Normal(_))
                    )
                {
                    return Err(CoreError::invalid("Invalid journal copy path"));
                }
            }
            if old.files.get(&entry.path) != entry.old_hash.as_ref()
                || new.files.get(&entry.path) != entry.new_hash.as_ref()
                || entry.old_copy.is_some() != entry.old_hash.is_some()
                || entry.new_copy.is_some() != entry.new_hash.is_some()
            {
                return Err(CoreError::invalid("Journal entry does not match manifests"));
            }
            if let (Some(name), Some(expected)) = (&entry.old_copy, &entry.old_hash) {
                verify_backup(&dir, name, expected)?;
            }
            if let (Some(name), Some(expected)) = (&entry.new_copy, &entry.new_hash) {
                verify_backup(&dir, name, expected)?;
            }
            let current = file_hash(root, &entry.path)?;
            if current != entry.old_hash && current != entry.new_hash {
                reason = Some("External change conflicts with recovery");
            }
        }
        let paths: BTreeSet<_> = old.files.keys().chain(new.files.keys()).collect();
        for path in paths {
            let changes = journal
                .entries
                .iter()
                .filter(|entry| &entry.path == path)
                .count();
            if changes
                != if old.files.get(path) != new.files.get(path) {
                    1
                } else {
                    0
                }
            {
                return Err(CoreError::invalid(
                    "Journal does not describe all changed files",
                ));
            }
        }
        let observed = scan(root)?;
        for path in old
            .files
            .keys()
            .chain(new.files.keys())
            .chain(observed.keys())
        {
            let current = observed.get(path);
            if current != old.files.get(path) && current != new.files.get(path) {
                reason = Some("External project change conflicts with recovery");
            }
        }
        if let Some(reason) = reason {
            preserve_conflict(root, &dir, &journal, reason)?;
            let manifest: Manifest = serde_json::from_slice(&old_manifest)
                .map_err(|e| CoreError::invalid(e.to_string()))?;
            let doc = journal
                .entries
                .iter()
                .find(|e| e.path == "document.json")
                .and_then(|e| e.old_copy.as_ref())
                .map(|name| fs::read(dir.join(name)))
                .transpose()?
                .or(file_bytes(root, "document.json")?)
                .ok_or_else(|| CoreError::invalid("Recovery document unavailable"))?;
            return Ok(Some(Loaded {
                manifest,
                manifest_bytes: old_manifest,
                document_bytes: doc,
                read_only_reason: Some(reason.into()),
            }));
        }
        let expected_marker = if forward {
            &journal.new_manifest_hash
        } else {
            &journal.old_manifest_hash
        };
        for (index, entry) in journal.entries.iter().enumerate() {
            if file_hash(root, "project.json")?.as_ref() != Some(expected_marker) {
                preserve_conflict(root, &dir, &journal, "Manifest changed during recovery")?;
                return Err(CoreError::conflict(
                    "Manifest changed during recovery; copies saved in conflicts",
                ));
            }
            let desired = if forward {
                &entry.new_hash
            } else {
                &entry.old_hash
            };
            if &file_hash(root, &entry.path)? == desired {
                continue;
            }
            let current = file_hash(root, &entry.path)?;
            if current != entry.old_hash && current != entry.new_hash {
                preserve_conflict(root, &dir, &journal, "External change during recovery")?;
                return Err(CoreError::conflict(
                    "External change during recovery; copies saved in conflicts",
                ));
            }
            let copy = if forward {
                &entry.new_copy
            } else {
                &entry.old_copy
            };
            let bytes = copy
                .as_ref()
                .zip(desired.as_ref())
                .map(|(name, hash)| verify_backup(&dir, name, hash))
                .transpose()?;
            #[cfg(test)]
            BEFORE_REPLACE.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            replace_with(
                root,
                &entry.path,
                current.as_deref(),
                bytes.as_deref(),
                &retained.join(format!("entry-{index}")),
            )?;
        }
        let target: Manifest = serde_json::from_slice(if forward {
            &new_manifest
        } else {
            &old_manifest
        })
        .map_err(|e| CoreError::invalid(e.to_string()))?;
        if scan(root)? != target.files
            || file_hash(root, "project.json")?.as_ref() != Some(expected_marker)
        {
            preserve_conflict(root, &dir, &journal, "Project changed during recovery")?;
            return Err(CoreError::conflict(
                "External project change during recovery; copies saved in conflicts",
            ));
        }
        retire_transaction(&dir)?;
    }
    Ok(None)
}

pub fn load(root: &Path) -> Result<Loaded> {
    if !root.is_dir() {
        return Err(CoreError::io("Project folder does not exist"));
    }
    if !root.join("project.json").is_file() && !root.join(".aic-local/transactions").is_dir() {
        return Err(CoreError::invalid(
            "Folder does not contain an AI Constructor project",
        ));
    }
    let _lock = lock(root)?;
    if let Some(loaded) = recover(root)? {
        return Ok(loaded);
    }
    let manifest_bytes = fs::read(root.join("project.json"))?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid project manifest: {e}")))?;
    let document_bytes = fs::read(root.join("document.json"))?;
    let read_only_reason = if scan(root)? != manifest.files {
        Some("Project files were changed outside the editor; open read-only to avoid overwriting them".into())
    } else {
        None
    };
    Ok(Loaded {
        manifest,
        manifest_bytes,
        document_bytes,
        read_only_reason,
    })
}

pub fn snapshot_inputs(root: &Path, expected_manifest: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let _lock = lock(root)?;
    if recover(root)?.is_some() {
        return Err(CoreError::conflict("Project requires recovery"));
    }
    if fs::read(root.join("project.json"))? != expected_manifest {
        return Err(CoreError::conflict("Project manifest changed on disk"));
    }
    let manifest: Manifest =
        serde_json::from_slice(expected_manifest).map_err(|e| CoreError::invalid(e.to_string()))?;
    if scan(root)? != manifest.files {
        return Err(CoreError::conflict("Project files changed on disk"));
    }
    let mut result = BTreeMap::new();
    for (path, expected) in &manifest.files {
        if path == "document.json"
            || path.starts_with("src/user/")
            || path.starts_with("src/generated/")
            || path.starts_with("assets/")
        {
            let bytes = file_bytes(root, path)?
                .ok_or_else(|| CoreError::conflict(format!("Missing {path}")))?;
            if hash(&bytes) != *expected {
                return Err(CoreError::conflict(format!("External edit of {path}")));
            }
            result.insert(path.clone(), bytes);
        }
    }
    if scan(root)? != manifest.files || fs::read(root.join("project.json"))? != expected_manifest {
        return Err(CoreError::conflict(
            "Project changed while making a snapshot",
        ));
    }
    Ok(result)
}

pub fn commit(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    let _lock = lock(root)?;
    if recover(root)?.is_some() {
        return Err(CoreError::conflict("Project requires recovery"));
    }
    let old_bytes = fs::read(root.join("project.json"))?;
    if old_bytes != expected_manifest {
        return Err(CoreError::conflict("Project manifest changed on disk"));
    }
    let old: Manifest =
        serde_json::from_slice(&old_bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
    if scan(root)? != old.files {
        return Err(CoreError::conflict("Project files changed on disk"));
    }
    #[cfg(test)]
    AFTER_SCAN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    let mut new = new_manifest.clone();
    new.storage_generation = old
        .storage_generation
        .checked_add(1)
        .ok_or_else(|| CoreError::invalid("Storage generation overflow"))?;
    new.files = old.files.clone();
    let mut actual = BTreeMap::new();
    for (path, bytes) in changes {
        safe_path(root, path)?;
        let next = hash(bytes);
        if old.files.get(path) != Some(&next) {
            new.files.insert(path.clone(), next);
            actual.insert(path.clone(), bytes.clone());
        }
    }
    let document = actual
        .get("document.json")
        .cloned()
        .or(file_bytes(root, "document.json")?)
        .ok_or_else(|| CoreError::invalid("Missing document"))?;
    new.snapshot_hash = fingerprint(&document, &new.files);
    let new_bytes = json(&new)?;
    if actual.is_empty() && old.name == new.name && old.revision == new.revision {
        return Ok(old_bytes);
    }
    let txdir = root
        .join(".aic-local/transactions")
        .join(Uuid::new_v4().to_string());
    fs::create_dir_all(&txdir)?;
    sync_dir(&root.join(".aic-local"))?;
    durable_write(&txdir.join("old-manifest"), &old_bytes)?;
    durable_write(&txdir.join("new-manifest"), &new_bytes)?;
    let mut entries = Vec::new();
    for (i, (path, bytes)) in actual.iter().enumerate() {
        let old_content = file_bytes(root, path)?;
        let old_hash = old_content.as_deref().map(hash);
        if old.files.get(path) != old_hash.as_ref() {
            return Err(CoreError::conflict(format!("External edit of {path}")));
        }
        let old_copy = if let Some(old_content) = &old_content {
            let name = format!("old-{i}");
            durable_write(&txdir.join(&name), old_content)?;
            Some(name)
        } else {
            None
        };
        let new_copy = format!("new-{i}");
        durable_write(&txdir.join(&new_copy), bytes)?;
        entries.push(Entry {
            path: path.clone(),
            old_hash,
            new_hash: Some(hash(bytes)),
            old_copy,
            new_copy: Some(new_copy),
        });
    }
    let journal = Journal {
        old_manifest_hash: hash(&old_bytes),
        new_manifest_hash: hash(&new_bytes),
        old_revision: old.revision,
        new_revision: new.revision,
        old_generation: old.storage_generation,
        new_generation: new.storage_generation,
        entries,
    };
    let pending_journal = txdir.join("journal.pending");
    durable_write(&pending_journal, &json(&journal)?)?;
    // Persist the prepared copies and temporary marker before publishing the journal.
    sync_dir(&txdir)?;
    atomic::move_no_replace(&pending_journal, &txdir.join("journal.json"))?;
    sync_dir(&txdir)?;
    sync_dir(
        txdir
            .parent()
            .ok_or_else(|| CoreError::invalid("Invalid transaction directory"))?,
    )?;
    let retained = retained_transaction(root, &txdir, &journal)?;
    #[cfg(test)]
    interrupt_at(0)?;
    for (index, entry) in journal.entries.iter().enumerate() {
        if file_hash(root, &entry.path)? != entry.old_hash {
            return Err(CoreError::conflict(format!(
                "External edit of {}",
                entry.path
            )));
        }
        #[cfg(test)]
        BEFORE_REPLACE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        replace_with(
            root,
            &entry.path,
            entry.old_hash.as_deref(),
            actual.get(&entry.path).map(Vec::as_slice),
            &retained.join(format!("entry-{index}")),
        )?;
        #[cfg(test)]
        interrupt_at(index + 1)?;
    }
    if scan(root)? != new.files
        || file_hash(root, "project.json")? != Some(journal.old_manifest_hash.clone())
    {
        return Err(CoreError::conflict(
            "External change before publishing the manifest",
        ));
    }
    #[cfg(test)]
    BEFORE_MANIFEST_REPLACE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    replace_with(
        root,
        "project.json",
        Some(&journal.old_manifest_hash),
        Some(&new_bytes),
        &retained.join("manifest"),
    )?;
    #[cfg(test)]
    interrupt_at(journal.entries.len() + 1)?;
    if let Err(error) = retire_transaction(&txdir) {
        // The commit marker is already durable; an unretired journal remains recoverable.
        eprintln!("AI Constructor: could not clean completed transaction: {error}");
    }
    Ok(new_bytes)
}
