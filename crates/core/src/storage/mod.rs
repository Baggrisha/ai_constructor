use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{CoreError, Result};

mod assets;
mod atomic;
mod conflicts;
pub(crate) mod control;
mod diagnostics;
mod migration;
mod recovery_choice;
pub(crate) mod retention;
mod services;

pub(crate) use assets::{asset_bytes, font_asset, image_dimensions};
pub(crate) use conflicts::generated_block;
pub(crate) use diagnostics::capture_external;
pub(crate) use services::{chat_attachment_ids, rewrite_chats};
const EXTERNAL_CHANGE_REASON: &str =
    "Project files were changed outside the editor; open read-only to avoid overwriting them";
/// Manifest schema version written by this build. It tracks the document schema.
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 2;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod checkpoint_tests;

#[cfg(test)]
mod retention_tests;

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

/// Inspect only top-level keys; unknown values are never deserialized into
/// the current manifest or rewritten by this version of the application.
pub(crate) fn manifest_has_unknown_fields(bytes: &[u8]) -> Result<bool> {
    let fields: BTreeMap<String, serde::de::IgnoredAny> = serde_json::from_slice(bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid project manifest: {e}")))?;
    const KNOWN: &[&str] = &[
        "schemaVersion",
        "projectId",
        "name",
        "revision",
        "storageGeneration",
        "snapshotHash",
        "files",
    ];
    Ok(fields.keys().any(|key| !KNOWN.contains(&key.as_str())))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModeTransition {
    pub(crate) before: Option<u32>,
    pub(crate) after: Option<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    path: String,
    old_hash: Option<String>,
    new_hash: Option<String>,
    old_copy: Option<String>,
    new_copy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modes: Option<ModeTransition>,
}

#[derive(Serialize, Deserialize)]
struct ObservedMarker {
    hash: Option<String>,
}

// Older cores must not replay a journal that also changes file modes.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(untagged, rename_all = "camelCase")]
enum JournalRevision {
    Legacy(u64),
    ModeAware { mode_aware_revision: u64 },
}

impl JournalRevision {
    fn value(self) -> u64 {
        match self {
            Self::Legacy(value)
            | Self::ModeAware {
                mode_aware_revision: value,
            } => value,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    old_manifest_hash: String,
    new_manifest_hash: String,
    old_revision: JournalRevision,
    new_revision: u64,
    old_generation: u64,
    new_generation: u64,
    // External resolution rolls back to the observed filesystem, not the
    // manifest's last known-good hashes. Old journals omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_files: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_marker: Option<ObservedMarker>,
    entries: Vec<Entry>,
}

#[derive(Debug)]
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
    loop {
        control::check()?;
        match file.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(error.into()),
        }
    }
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

fn read_checked(mut file: File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control::check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control::check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn file_bytes(root: &Path, relative: &str) -> Result<Option<Vec<u8>>> {
    match File::open(safe_path(root, relative)?) {
        Ok(file) => Ok(Some(read_checked(file)?)),
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
    durable_write_mode(path, bytes, None)
}

fn durable_write_mode(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
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
    for chunk in bytes.chunks(64 * 1024) {
        control::check()?;
        file.write_all(chunk)?;
    }
    control::check()?;
    if let Some(mode) = mode {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if mode & !0o7777 != 0 {
                return Err(CoreError::invalid("Invalid file mode"));
            }
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        return Err(CoreError::unsupported(
            "File modes are unavailable on this platform",
        ));
    }
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
        .chain(
            journal
                .observed_marker
                .as_ref()
                .and_then(|marker| marker.hash.as_ref())
                .map(|_| "observed-manifest"),
        )
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

fn entry_matches(root: &Path, entry: &Entry, after: bool) -> Result<bool> {
    let (digest, mode) = if after {
        (&entry.new_hash, entry.modes.as_ref().map(|m| m.after))
    } else {
        (&entry.old_hash, entry.modes.as_ref().map(|m| m.before))
    };
    if file_hash(root, &entry.path)? != *digest {
        return Ok(false);
    }
    match mode {
        Some(expected) => Ok(file_mode(root, &entry.path)? == expected),
        None => Ok(true),
    }
}

fn unexpected_retained_mode(dir: &Path, entry: &Entry) -> Result<bool> {
    let Some(modes) = &entry.modes else {
        return Ok(false);
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    for item in entries {
        let item = item?;
        if !item.file_type()?.is_file() {
            return Ok(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = Some(item.metadata()?.permissions().mode() & 0o7777);
            let digest = hash_file(&item.path())?;
            if !((entry.old_hash.as_ref() == Some(&digest) && modes.before == mode)
                || (entry.new_hash.as_ref() == Some(&digest) && modes.after == mode))
            {
                return Ok(true);
            }
        }
        #[cfg(not(unix))]
        return Err(CoreError::unsupported(
            "File modes are unavailable on this platform",
        ));
    }
    Ok(false)
}

fn create_destination_parent(root: &Path, parent: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut current = parent;
    loop {
        match fs::symlink_metadata(current) {
            Ok(meta) if meta.file_type().is_dir() || (current == root && current.is_dir()) => break,
            Ok(_) => return Err(CoreError::conflict("Destination parent is not a directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if current == root {
                    return Err(CoreError::conflict("Project directory disappeared"));
                }
                missing.push(current);
                current = current
                    .parent()
                    .ok_or_else(|| CoreError::invalid("Invalid destination parent"))?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    fs::create_dir_all(parent)?;
    for directory in missing.iter().rev() {
        if !fs::symlink_metadata(directory)?.file_type().is_dir() {
            return Err(CoreError::conflict(
                "Destination parent changed during publication",
            ));
        }
        sync_dir(
            directory
                .parent()
                .ok_or_else(|| CoreError::invalid("Invalid destination parent"))?,
        )?;
    }
    Ok(())
}

fn replace_with(
    root: &Path,
    relative: &str,
    expected: Option<&str>,
    bytes: Option<&[u8]>,
    retained: &Path,
) -> Result<()> {
    replace_with_modes(root, relative, expected, bytes, retained, None)
}

fn replace_with_modes(
    root: &Path,
    relative: &str,
    expected: Option<&str>,
    bytes: Option<&[u8]>,
    retained: &Path,
    modes: Option<&ModeTransition>,
) -> Result<()> {
    if let Some(modes) = modes {
        if modes.before.is_some() != expected.is_some()
            || modes.after.is_some() != bytes.is_some()
            || file_mode(root, relative)? != modes.before
        {
            return Err(CoreError::conflict(format!(
                "File mode changed: {relative}"
            )));
        }
    }
    let path = safe_path(root, relative)?;
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::invalid("Invalid file path"))?;
    create_destination_parent(root, parent)?;
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
        durable_write_mode(&temp, bytes, modes.and_then(|mode| mode.after))?;
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
        if let Some(modes) = modes {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if Some(fs::symlink_metadata(&slot)?.permissions().mode() & 0o7777) != modes.before
                {
                    return Err(CoreError::conflict(format!(
                        "External mode change of {relative} preserved at {}",
                        slot.display()
                    )));
                }
            }
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
            control::check()?;
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
            files.insert(name, hash_file(entry.path())?);
        }
    }
    Ok(files)
}

pub fn create(root: &Path, manifest: &Manifest, document: &[u8]) -> Result<Manifest> {
    control::check()?;
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
        b".aic-local/\n.cache/\n.DS_Store\n.env\n.env.*\n/build/\n/dist/\n/target/\n/out/\n/node_modules/\n/.next/\n/coverage/\n/DerivedData/\n",
    )?;
    let mut manifest = manifest.clone();
    manifest.files = scan(root)?;
    manifest.snapshot_hash = fingerprint(document, &manifest.files);
    durable_write(&root.join("project.json"), &json(&manifest)?)?;
    sync_dir(root)?;
    Ok(manifest)
}

#[cfg(unix)]
pub fn copy_executable_modes(root: &Path, target: &Path, paths: &[String]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for path in paths {
        control::check()?;
        let mode = checkpoint_executable_mode(root, path)?;
        let destination = safe_path(target, path)?;
        let mut permissions = fs::symlink_metadata(&destination)?.permissions();
        permissions.set_mode((permissions.mode() & !0o111) | mode);
        fs::set_permissions(&destination, permissions)?;
        File::open(&destination)?.sync_all()?;
        if checkpoint_executable_mode(root, path)? != mode {
            return Err(CoreError::conflict(format!(
                "File mode changed while duplicating {path}"
            )));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn copy_executable_modes(_root: &Path, _target: &Path, _paths: &[String]) -> Result<()> {
    Err(CoreError::unsupported(
        "Cannot preserve executable modes on this platform",
    ))
}

pub fn publish_directory(staged: &Path, destination: &Path) -> Result<()> {
    control::begin_commit()?;
    atomic::move_no_replace(staged, destination)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    sync_dir(parent)
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
    if let Some(marker) = &journal.observed_marker {
        if let Some(expected) = &marker.hash {
            durable_write(
                &conflict.join("observed-manifest"),
                &verify_backup(dir, "observed-manifest", expected)?,
            )?;
        }
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
          // A published journal may require recovery writes before the main operation.
        control::begin_commit()?;
        let journal_bytes = fs::read(&journal_path)?;
        let journal: Journal = serde_json::from_slice(&journal_bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid recovery journal: {e}")))?;
        if dir.join("recovery-choice.json").exists() {
            if let Some(loaded) = recovery_choice::resume(root, &dir, &journal, &journal_bytes)? {
                return Ok(Some(loaded));
            }
            continue;
        }
        let old_manifest = verify_backup(&dir, "old-manifest", &journal.old_manifest_hash)?;
        let new_manifest = verify_backup(&dir, "new-manifest", &journal.new_manifest_hash)?;
        let old: Manifest =
            serde_json::from_slice(&old_manifest).map_err(|e| CoreError::invalid(e.to_string()))?;
        let new: Manifest =
            serde_json::from_slice(&new_manifest).map_err(|e| CoreError::invalid(e.to_string()))?;
        if old.revision != journal.old_revision.value()
            || new.revision != journal.new_revision
            || old.storage_generation != journal.old_generation
            || new.storage_generation != journal.new_generation
            || old.project_id != new.project_id
        {
            return Err(CoreError::invalid(
                "Journal manifest metadata does not match",
            ));
        }
        let old_files = journal.observed_files.as_ref().unwrap_or(&old.files);
        let old_marker = match &journal.observed_marker {
            Some(marker) => {
                if let Some(expected) = &marker.hash {
                    verify_backup(&dir, "observed-manifest", expected)?;
                }
                marker.hash.as_deref()
            }
            None => Some(journal.old_manifest_hash.as_str()),
        };
        let retained = retained_transaction(root, &dir, &journal)?;
        let marker = file_hash(root, "project.json")?;
        let forward = marker.as_deref() == Some(journal.new_manifest_hash.as_str());
        // Absence is not a completion marker, even if an earlier external
        // resolution happened to start with an absent project.json.
        let backward = marker.is_some() && marker.as_deref() == old_marker;
        if forward && backward {
            return Err(CoreError::invalid("Recovery marker is ambiguous"));
        }
        let mut reason = if !forward && !backward {
            Some("Manifest does not match either saved version")
        } else {
            None
        };
        if unexpected_retained(
            &retained.join("manifest"),
            old_marker,
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
            if unexpected_retained_mode(&retained.join(format!("entry-{index}")), entry)? {
                reason = Some("External file mode change retained; resolve manually");
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
            if old_files.get(&entry.path) != entry.old_hash.as_ref()
                || new.files.get(&entry.path) != entry.new_hash.as_ref()
                || entry.old_copy.is_some() != entry.old_hash.is_some()
                || entry.new_copy.is_some() != entry.new_hash.is_some()
            {
                return Err(CoreError::invalid("Journal entry does not match manifests"));
            }
            if let Some(modes) = &entry.modes {
                if !checkpoint_dependency_path(&entry.path)
                    || modes.before.is_some() != entry.old_hash.is_some()
                    || modes.after.is_some() != entry.new_hash.is_some()
                    || [modes.before, modes.after]
                        .into_iter()
                        .flatten()
                        .any(|mode| mode & !0o7777 != 0)
                {
                    return Err(CoreError::invalid("Invalid journal file modes"));
                }
            }
            if let (Some(name), Some(expected)) = (&entry.old_copy, &entry.old_hash) {
                verify_backup(&dir, name, expected)?;
            }
            if let (Some(name), Some(expected)) = (&entry.new_copy, &entry.new_hash) {
                verify_backup(&dir, name, expected)?;
            }
            if !entry_matches(root, entry, false)? && !entry_matches(root, entry, true)? {
                reason = Some("External change conflicts with recovery");
            }
        }
        let paths: BTreeSet<_> = old_files.keys().chain(new.files.keys()).collect();
        for path in paths {
            let changes = journal
                .entries
                .iter()
                .filter(|entry| &entry.path == path)
                .count();
            let mode_changed = journal.entries.iter().any(|entry| {
                &entry.path == path
                    && entry
                        .modes
                        .as_ref()
                        .is_some_and(|mode| mode.before != mode.after)
            });
            if changes != usize::from(old_files.get(path) != new.files.get(path) || mode_changed) {
                return Err(CoreError::invalid(
                    "Journal does not describe all changed files",
                ));
            }
        }
        let observed = scan(root)?;
        for path in old_files
            .keys()
            .chain(new.files.keys())
            .chain(observed.keys())
        {
            let current = observed.get(path);
            if current != old_files.get(path) && current != new.files.get(path) {
                reason = Some("External project change conflicts with recovery");
            }
        }
        if let Some(reason) = reason {
            preserve_conflict(root, &dir, &journal, reason)?;
            let manifest: Manifest = serde_json::from_slice(&old_manifest)
                .map_err(|e| CoreError::invalid(e.to_string()))?;
            let fallback = journal
                .entries
                .iter()
                .find(|entry| entry.path == "document.json")
                .and_then(|entry| entry.old_copy.as_ref())
                .map(|name| fs::read(dir.join(name)))
                .transpose()?
                .or(file_bytes(root, "document.json")?);
            let doc = match conflicts::baseline_document(root, &old_manifest, &old)? {
                Some(document) => document,
                None => fallback
                    .filter(|bytes| old.files.get("document.json") == Some(&hash(bytes)))
                    .ok_or_else(|| {
                        CoreError::new(
                            "BASELINE_UNAVAILABLE",
                            "Recovery baseline document is unavailable",
                        )
                    })?,
            };
            return Ok(Some(Loaded {
                manifest,
                manifest_bytes: old_manifest,
                document_bytes: doc,
                read_only_reason: Some(reason.into()),
            }));
        }
        let expected_marker = if forward {
            Some(journal.new_manifest_hash.as_str())
        } else {
            old_marker
        };
        for (index, entry) in journal.entries.iter().enumerate() {
            if file_hash(root, "project.json")?.as_deref() != expected_marker {
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
            if entry_matches(root, entry, forward)? {
                continue;
            }
            let current = file_hash(root, &entry.path)?;
            if !entry_matches(root, entry, false)? && !entry_matches(root, entry, true)? {
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
            let recovery_modes = entry.modes.as_ref().map(|modes| ModeTransition {
                before: if forward { modes.before } else { modes.after },
                after: if forward { modes.after } else { modes.before },
            });
            #[cfg(test)]
            BEFORE_REPLACE.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            replace_with_modes(
                root,
                &entry.path,
                current.as_deref(),
                bytes.as_deref(),
                &retained.join(format!("entry-{index}")),
                recovery_modes.as_ref(),
            )?;
        }
        let target: Manifest = serde_json::from_slice(if forward {
            &new_manifest
        } else {
            &old_manifest
        })
        .map_err(|e| CoreError::invalid(e.to_string()))?;
        let target_files = if forward { &target.files } else { old_files };
        for entry in &journal.entries {
            if entry.modes.is_some() && !entry_matches(root, entry, forward)? {
                preserve_conflict(root, &dir, &journal, "File mode changed during recovery")?;
                return Err(CoreError::conflict("File mode changed during recovery"));
            }
        }
        if scan(root)? != *target_files
            || file_hash(root, "project.json")?.as_deref() != expected_marker
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
    if !root.join("project.json").is_file()
        && !root.join(".aic-local/transactions").is_dir()
        && !root.join("conflicts").is_dir()
    {
        return Err(CoreError::invalid(
            "Folder does not contain an AI Constructor project",
        ));
    }
    let _lock = lock(root)?;
    if let Some(loaded) = recover(root)? {
        return Ok(loaded);
    }
    let marker = file_bytes(root, "project.json")?;
    let observed = scan(root)?;
    let parsed = marker
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Manifest>(bytes).ok());
    let Some(mut manifest) = parsed else {
        return conflicts::archived_base_for_disk(root, &observed, marker.as_deref())?
            .ok_or_else(|| CoreError::new(
                "BASELINE_UNAVAILABLE",
                "Project manifest is missing or invalid and no unique verified recovery baseline is available",
            ));
    };
    let mut manifest_bytes = marker.expect("a parsed manifest has source bytes");
    let (document_bytes, read_only_reason) = if observed == manifest.files {
        let mut document = file_bytes(root, "document.json")?
            .ok_or_else(|| CoreError::invalid("Missing document"))?;
        // A future extension cannot be safely migrated by a model which
        // cannot represent all of its fields.
        if !manifest_has_unknown_fields(&manifest_bytes)? {
            if let Some(migrated) =
                migration::migrate_locked(root, &manifest_bytes, &manifest, &document)?
            {
                manifest = migrated.manifest;
                manifest_bytes = migrated.manifest_bytes;
                document = migrated.document_bytes;
            }
            // Earlier releases stored generated-file protection only locally.
            // Promote those verified records through the normal file journal.
            if manifest.schema_version <= 2
                && serde_json::from_slice::<crate::model::Document>(&document)
                    .ok()
                    .is_some_and(|model| model.editable() && model.validate().is_ok())
            {
                let mut additions = BTreeMap::new();
                for (path, bytes) in conflicts::legacy_generated_blocks(root)? {
                    match manifest.files.get(&path) {
                        Some(expected) if *expected == hash(&bytes) => {}
                        Some(_) => {
                            return Err(CoreError::conflict("Generated source block differs"))
                        }
                        None => {
                            additions.insert(path, bytes);
                        }
                    }
                }
                if !additions.is_empty() {
                    manifest_bytes = commit_locked(
                        root,
                        &manifest_bytes,
                        &manifest,
                        &additions,
                        &BTreeSet::new(),
                        None,
                        None,
                    )?;
                    manifest = serde_json::from_slice(&manifest_bytes)
                        .map_err(|e| CoreError::invalid(e.to_string()))?;
                }
            }
        }
        if !conflicts::bootstrap(root, &manifest_bytes, &manifest)? {
            return Err(CoreError::conflict(
                "Project changed while preparing its baseline",
            ));
        }
        (document, None)
    } else {
        let document = match conflicts::baseline_document(root, &manifest_bytes, &manifest)? {
            Some(bytes) => bytes,
            None => file_bytes(root, "document.json")?.ok_or_else(|| {
                CoreError::new(
                    "BASELINE_UNAVAILABLE",
                    "Document is missing and no verified baseline is available",
                )
            })?,
        };
        (document, Some(EXTERNAL_CHANGE_REASON.into()))
    };
    Ok(Loaded {
        manifest,
        manifest_bytes,
        document_bytes,
        read_only_reason,
    })
}

pub fn validate_current(root: &Path, expected_manifest: &[u8]) -> Result<()> {
    let _lock = lock(root)?;
    if recover(root)?.is_some() {
        return Err(CoreError::conflict("Project requires recovery"));
    }
    let manifest: Manifest =
        serde_json::from_slice(expected_manifest).map_err(|e| CoreError::invalid(e.to_string()))?;
    if file_bytes(root, "project.json")?.as_deref() != Some(expected_manifest)
        || scan(root)? != manifest.files
        || file_bytes(root, "project.json")?.as_deref() != Some(expected_manifest)
    {
        return Err(CoreError::conflict("Project changed on disk"));
    }
    Ok(())
}

pub fn snapshot_inputs(root: &Path, expected_manifest: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    Ok(snapshot_selected(root, expected_manifest, false)?.files)
}

pub(super) struct DuplicateSnapshot {
    pub(super) files: BTreeMap<String, Vec<u8>>,
    pub(super) blocked_generated: BTreeMap<String, String>,
}

pub(super) fn snapshot_all_inputs(
    root: &Path,
    expected_manifest: &[u8],
) -> Result<DuplicateSnapshot> {
    snapshot_selected(root, expected_manifest, true)
}

fn portable_conflict_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let directory = safe_path(root, "conflicts")?;
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(CoreError::invalid(
                "Conflict directory is not a real directory",
            ))
        }
    }
    let mut result = BTreeMap::new();
    for entry in WalkDir::new(directory).min_depth(1).follow_links(false) {
        control::check()?;
        let entry = entry.map_err(|error| CoreError::io(error.to_string()))?;
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            return Err(CoreError::invalid(
                "Conflict archive contains a non-regular file",
            ));
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| CoreError::invalid(error.to_string()))?
            .to_str()
            .ok_or_else(|| CoreError::invalid("Non-UTF-8 conflict path"))?;
        result.insert(relative.to_owned(), hash_file(entry.path())?);
    }
    Ok(result)
}

fn check_portable_conflicts(
    root: &Path,
    state: &crate::State,
    observed: &BTreeMap<String, String>,
) -> Result<()> {
    let (_, conflicts) = conflicts::external_diagnostics(
        root,
        state,
        observed,
        Some(state.manifest_bytes.as_slice()),
    )?;
    if !conflicts.is_empty() {
        return Err(CoreError::conflict(
            "Resolve preserved conflicts before creating a snapshot",
        ));
    }
    for path in conflicts::blocked_generated(root, &state.manifest)? {
        let digest = state
            .manifest
            .files
            .get(&path)
            .ok_or_else(|| CoreError::invalid("Missing blocked generated file"))?;
        let (record, bytes) = conflicts::generated_block(root, &path, digest)?;
        if state.manifest.files.get(&record) != Some(&hash(&bytes)) {
            return Err(CoreError::conflict(
                "Reopen the project to transfer its local generated-file protection",
            ));
        }
    }
    Ok(())
}

fn archive_portable_mismatch(root: &Path, state: &crate::State) -> Result<()> {
    let observed = scan(root)?;
    let marker = file_bytes(root, "project.json")?;
    conflicts::external_diagnostics(root, state, &observed, marker.as_deref())?;
    Ok(())
}

fn copy_portable_file(root: &Path, stage: &Path, path: &str, expected: &str) -> Result<()> {
    let source = safe_path(root, path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut input = options.open(&source).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CoreError::conflict(format!("Missing {path}"))
        } else {
            error.into()
        }
    })?;
    let metadata = input.metadata()?;
    if !metadata.is_file() {
        return Err(CoreError::invalid(format!(
            "Snapshot source is not a regular file: {path}"
        )));
    }
    let destination = safe_path(stage, path)?;
    let parent = destination
        .parent()
        .ok_or_else(|| CoreError::invalid("Invalid snapshot path"))?;
    create_destination_parent(stage, parent)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control::check()?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err(CoreError::conflict(format!("External edit of {path}")));
    }
    #[cfg(unix)]
    if path.starts_with("src/") || path.starts_with("assets/") {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o111;
        if checkpoint_executable_mode(root, path)? != mode {
            return Err(CoreError::conflict(format!(
                "File mode changed while copying {path}"
            )));
        }
        let mut permissions = output.metadata()?.permissions();
        permissions.set_mode((permissions.mode() & !0o111) | mode);
        output.set_permissions(permissions)?;
    }
    control::check()?;
    output.sync_all()?;
    sync_dir(parent)
}

pub(super) fn write_portable_snapshot(
    root: &Path,
    stage: &Path,
    state: &crate::State,
) -> Result<()> {
    let _lock = lock(root)?;
    if recover(root)?.is_some() {
        return Err(CoreError::conflict("Project requires recovery"));
    }
    let observed = scan(root)?;
    if observed != state.manifest.files
        || file_bytes(root, "project.json")?.as_deref() != Some(state.manifest_bytes.as_slice())
    {
        archive_portable_mismatch(root, state)?;
        return Err(CoreError::conflict(
            "Project changed while making a snapshot",
        ));
    }
    check_portable_conflicts(root, state, &observed)?;
    let conflict_hashes = portable_conflict_hashes(root)?;
    for directory in [
        "src/user",
        "src/generated",
        "assets",
        "chats",
        "agent/runs",
        "agent/proposals",
        "history/transactions",
        "checkpoints",
        "conflicts",
    ] {
        control::check()?;
        let destination = safe_path(stage, directory)?;
        create_destination_parent(stage, &destination)?;
        sync_dir(&destination)?;
    }
    for (path, expected) in &state.manifest.files {
        if let Err(error) = copy_portable_file(root, stage, path, expected) {
            if error.code == "REVISION_CONFLICT" {
                archive_portable_mismatch(root, state)?;
            }
            return Err(error);
        }
    }
    for (path, expected) in &conflict_hashes {
        copy_portable_file(root, stage, path, expected)?;
    }
    if scan(root)? != observed
        || file_bytes(root, "project.json")?.as_deref() != Some(state.manifest_bytes.as_slice())
    {
        archive_portable_mismatch(root, state)?;
        return Err(CoreError::conflict(
            "Project changed while making a snapshot",
        ));
    }
    check_portable_conflicts(root, state, &observed)?;
    if portable_conflict_hashes(root)? != conflict_hashes {
        return Err(CoreError::conflict(
            "Conflict archives changed while making a snapshot",
        ));
    }
    durable_write(&stage.join("project.json"), &state.manifest_bytes)?;
    sync_dir(stage)
}

pub(super) fn write_duplicate_safety(
    root: &Path,
    archives: &BTreeMap<String, Vec<u8>>,
    blocked: &BTreeMap<String, String>,
) -> Result<()> {
    for (path, bytes) in archives {
        control::check()?;
        if !path.starts_with("conflicts/") {
            return Err(CoreError::invalid("Invalid conflict archive path"));
        }
        let destination = safe_path(root, path)?;
        let parent = destination
            .parent()
            .ok_or_else(|| CoreError::invalid("Invalid archive path"))?;
        create_destination_parent(root, parent)?;
        durable_write(&destination, bytes)?;
        sync_dir(parent)?;
    }
    for (path, expected) in blocked {
        control::check()?;
        if hash_file(&safe_path(root, path)?)? != *expected {
            return Err(CoreError::conflict(format!(
                "Generated source changed in duplicate: {path}"
            )));
        }
        let manifest: Manifest = serde_json::from_slice(
            &file_bytes(root, "project.json")?
                .ok_or_else(|| CoreError::invalid("Missing project manifest"))?,
        )
        .map_err(|e| CoreError::invalid(e.to_string()))?;
        let (record, bytes) = conflicts::generated_block(root, path, expected)?;
        if manifest.files.get(&record) != Some(&hash(&bytes)) {
            return Err(CoreError::conflict(
                "Duplicate is missing generated-file protection",
            ));
        }
    }
    Ok(())
}

fn snapshot_selected(
    root: &Path,
    expected_manifest: &[u8],
    all: bool,
) -> Result<DuplicateSnapshot> {
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
    let mut blocked_generated = BTreeMap::new();
    if all {
        for path in conflicts::blocked_generated(root, &manifest)? {
            let digest = manifest
                .files
                .get(&path)
                .ok_or_else(|| CoreError::invalid("Missing blocked generated file"))?;
            blocked_generated.insert(path, digest.clone());
        }
    }
    let conflict_hashes = if all {
        portable_conflict_hashes(root)?
    } else {
        BTreeMap::new()
    };
    let mut result = BTreeMap::new();
    for (path, expected) in &manifest.files {
        if all
            || path == "document.json"
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
    for (path, expected) in &conflict_hashes {
        let bytes = file_bytes(root, path)?
            .ok_or_else(|| CoreError::conflict(format!("Missing conflict archive {path}")))?;
        if hash(&bytes) != *expected {
            return Err(CoreError::conflict(format!(
                "Conflict archive changed: {path}"
            )));
        }
        result.insert(path.clone(), bytes);
    }
    if scan(root)? != manifest.files || fs::read(root.join("project.json"))? != expected_manifest {
        return Err(CoreError::conflict(
            "Project changed while making a snapshot",
        ));
    }
    if all && portable_conflict_hashes(root)? != conflict_hashes {
        return Err(CoreError::conflict(
            "Conflict archives changed while making a snapshot",
        ));
    }
    if all
        && !conflicts::blocked_generated(root, &manifest)?
            .iter()
            .eq(blocked_generated.keys())
    {
        return Err(CoreError::conflict(
            "Generated source protection changed while making a snapshot",
        ));
    }
    Ok(DuplicateSnapshot {
        files: result,
        blocked_generated,
    })
}

pub fn checkpoint_dependency_path(path: &str) -> bool {
    (path.starts_with("src/user/")
        || path.starts_with("src/generated/")
        || path.starts_with("assets/"))
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(unix)]
pub fn checkpoint_executable_mode(root: &Path, path: &str) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(safe_path(root, path)?)?;
    if !metadata.file_type().is_file() {
        return Err(CoreError::unsupported(format!(
            "Checkpoint dependency is not a regular file: {path}"
        )));
    }
    Ok(metadata.permissions().mode() & 0o111)
}

#[cfg(not(unix))]
pub fn checkpoint_executable_mode(_root: &Path, _path: &str) -> Result<u32> {
    Err(CoreError::unsupported(
        "Checkpoint file permissions are unavailable on this platform",
    ))
}

#[cfg(unix)]
pub(crate) fn file_mode(root: &Path, path: &str) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    match fs::symlink_metadata(safe_path(root, path)?) {
        Ok(metadata) if metadata.file_type().is_file() => {
            Ok(Some(metadata.permissions().mode() & 0o7777))
        }
        Ok(_) => Err(CoreError::unsupported(format!(
            "Not a regular file: {path}"
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
pub(crate) fn file_mode(_root: &Path, _path: &str) -> Result<Option<u32>> {
    Err(CoreError::unsupported(
        "File modes are unavailable on this platform",
    ))
}

#[cfg(unix)]
pub(crate) fn default_file_mode(root: &Path) -> Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    let path = safe_path(root, &format!(".aic-local/mode-probe-{}", Uuid::new_v4()))?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mode = file
        .metadata()
        .map(|metadata| metadata.permissions().mode() & 0o7777);
    drop(file);
    fs::remove_file(&path)?;
    Ok(mode?)
}

#[cfg(not(unix))]
pub(crate) fn default_file_mode(_root: &Path) -> Result<u32> {
    Err(CoreError::unsupported(
        "File modes are unavailable on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn require_non_casefold_directory(path: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    let directory = File::open(path)?;
    let mut flags: libc::c_int = 0;
    // ioctl writes the flags into this initialized int; the descriptor stays open.
    let result = unsafe {
        libc::ioctl(
            directory.as_raw_fd(),
            libc::FS_IOC_GETFLAGS as _,
            &mut flags,
        )
    };
    if result < 0 {
        return Err(CoreError::unsupported(format!(
            "Cannot determine directory casefold behavior for {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    const FS_CASEFOLD_FL: libc::c_int = 0x4000_0000;
    if flags & FS_CASEFOLD_FL != 0 {
        return Err(CoreError::unsupported(format!(
            "Casefolded directory is not supported: {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn validate_checkpoint_tree(
    root: &Path,
    dependencies: &BTreeMap<String, String>,
) -> Result<()> {
    if dependencies.is_empty() {
        return Ok(());
    }
    let probe = root
        .join(".aic-local")
        .join(format!("checkpoint-probe-{}", Uuid::new_v4()));
    fs::create_dir(&probe)?;
    let result = (|| {
        #[cfg(unix)]
        let device = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(&probe)?.dev()
        };
        #[cfg(target_os = "linux")]
        let mut checked_directories = {
            require_non_casefold_directory(root)?;
            BTreeSet::from([root.to_path_buf()])
        };
        let mut expected_children: BTreeMap<PathBuf, BTreeSet<std::ffi::OsString>> =
            BTreeMap::new();
        for path in dependencies.keys() {
            let mut actual = root.to_path_buf();
            let mut scratch = probe.clone();
            let mut missing = false;
            for part in Path::new(path).components() {
                let parent = scratch.clone();
                scratch.push(part.as_os_str());
                if expected_children
                    .entry(parent)
                    .or_default()
                    .insert(part.as_os_str().to_owned())
                {
                    match fs::create_dir(&scratch) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(CoreError::unsupported(format!(
                                "Checkpoint paths have filesystem aliases: {path}"
                            )));
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                if !missing {
                    actual.push(part.as_os_str());
                    match fs::symlink_metadata(&actual) {
                        Ok(meta) => {
                            if meta.file_type().is_symlink() {
                                return Err(CoreError::unsupported(
                                    "Checkpoint destinations must not contain symlinks",
                                ));
                            }
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::MetadataExt;
                                if meta.dev() != device {
                                    return Err(CoreError::unsupported(
                                        "Checkpoint files must be on the project storage volume",
                                    ));
                                }
                            }
                            #[cfg(target_os = "linux")]
                            if meta.file_type().is_dir()
                                && checked_directories.insert(actual.clone())
                            {
                                require_non_casefold_directory(&actual)?;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            missing = true
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
                            return Err(CoreError::unsupported(format!(
                                "Checkpoint file/directory collision: {path}"
                            )));
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        for (parent, expected) in expected_children {
            let actual = fs::read_dir(parent)?
                .map(|entry| entry.map(|item| item.file_name()))
                .collect::<std::io::Result<BTreeSet<_>>>()?;
            if actual != expected {
                return Err(CoreError::unsupported(
                    "Checkpoint paths are not preserved by the filesystem",
                ));
            }
        }
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&probe);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub fn checkpoint_destination(
    root: &Path,
    path: &str,
    directories: &mut BTreeMap<PathBuf, BTreeSet<std::ffi::OsString>>,
    mode_aware: bool,
) -> Result<()> {
    let mut current = root.to_path_buf();
    let mut parts = Path::new(path).components().peekable();
    while let Some(part) = parts.next() {
        current.push(part.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                let parent = current
                    .parent()
                    .ok_or_else(|| CoreError::invalid("Invalid checkpoint path"))?;
                let exact = if let Some(names) = directories.get(parent) {
                    names.contains(part.as_os_str())
                } else {
                    let names = fs::read_dir(parent)?
                        .map(|entry| entry.map(|item| item.file_name()))
                        .collect::<std::io::Result<BTreeSet<_>>>()?;
                    let exact = names.contains(part.as_os_str());
                    directories.insert(parent.to_path_buf(), names);
                    exact
                };
                if !exact {
                    return Err(CoreError::unsupported(format!(
                        "Checkpoint destination has a filesystem alias: {path}"
                    )));
                }
                if meta.file_type().is_symlink() {
                    return Err(CoreError::unsupported(
                        "Checkpoint destinations must not contain symlinks",
                    ));
                }
                if parts.peek().is_some() && !meta.file_type().is_dir() {
                    return Err(CoreError::unsupported(format!(
                        "Checkpoint file/directory collision: {path}"
                    )));
                }
                if parts.peek().is_none() {
                    if !meta.file_type().is_file() {
                        return Err(CoreError::unsupported(format!(
                            "Checkpoint file/directory collision: {path}"
                        )));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if !mode_aware && meta.permissions().mode() & 0o111 != 0 {
                            return Err(CoreError::unsupported(format!(
                                "Cannot safely replace an executable file: {path}"
                            )));
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn validate_checkpoint_asset(path: &str, digest: &str) -> Result<()> {
    let Some(file) = path.strip_prefix("assets/") else {
        return Ok(());
    };
    if file.contains('/') {
        return Ok(());
    }
    let Some((address, _)) = file.split_once('.') else {
        return Ok(());
    };
    if address.len() == 64
        && address
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && address != digest
    {
        return Err(CoreError::conflict(format!(
            "Asset content does not match its address: {path}"
        )));
    }
    Ok(())
}

pub fn tracked_bytes(root: &Path, manifest: &Manifest, path: &str) -> Result<Vec<u8>> {
    let expected = manifest
        .files
        .get(path)
        .ok_or_else(|| CoreError::conflict(format!("Tracked file is missing: {path}")))?;
    let bytes = file_bytes(root, path)?
        .ok_or_else(|| CoreError::conflict(format!("Tracked file is missing: {path}")))?;
    if hash(&bytes) != *expected {
        return Err(CoreError::conflict(format!(
            "Tracked file was modified: {path}"
        )));
    }
    Ok(bytes)
}

pub fn checkpoint_blob(root: &Path, manifest: &Manifest, digest: &str) -> Result<Vec<u8>> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(CoreError::invalid("Invalid checkpoint hash"));
    }
    let path = format!("checkpoints/blobs/{digest}");
    if manifest.files.get(&path).map(String::as_str) != Some(digest) {
        return Err(CoreError::conflict(format!(
            "Checkpoint blob {digest} is missing or modified"
        )));
    }
    tracked_bytes(root, manifest, &path)
}

pub fn commit(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>> {
    commit_with_deletions(
        root,
        expected_manifest,
        new_manifest,
        changes,
        &BTreeSet::new(),
    )
}

pub fn commit_with_deletions(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
) -> Result<Vec<u8>> {
    commit_observed(
        root,
        expected_manifest,
        new_manifest,
        changes,
        deletions,
        None,
    )
}

fn commit_observed(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
    expected_observed: Option<&BTreeMap<String, String>>,
) -> Result<Vec<u8>> {
    commit_inner(
        root,
        expected_manifest,
        new_manifest,
        changes,
        deletions,
        expected_observed,
        None,
    )
}

fn commit_resolution(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
    expected_observed: &BTreeMap<String, String>,
    observed_marker: Option<&[u8]>,
) -> Result<Vec<u8>> {
    commit_inner(
        root,
        expected_manifest,
        new_manifest,
        changes,
        deletions,
        Some(expected_observed),
        Some(observed_marker),
    )
}

fn commit_inner(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
    expected_observed: Option<&BTreeMap<String, String>>,
    observed_marker: Option<Option<&[u8]>>,
) -> Result<Vec<u8>> {
    let _lock = lock(root)?;
    commit_locked(
        root,
        expected_manifest,
        new_manifest,
        changes,
        deletions,
        expected_observed,
        observed_marker,
    )
}

fn commit_locked(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
    expected_observed: Option<&BTreeMap<String, String>>,
    observed_marker: Option<Option<&[u8]>>,
) -> Result<Vec<u8>> {
    commit_locked_modes(
        root,
        expected_manifest,
        new_manifest,
        changes,
        deletions,
        expected_observed,
        observed_marker,
        None,
    )
}

pub(crate) fn commit_with_file_modes(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
    modes: &BTreeMap<String, ModeTransition>,
) -> Result<Vec<u8>> {
    let _lock = lock(root)?;
    commit_locked_modes(
        root,
        expected_manifest,
        new_manifest,
        changes,
        deletions,
        None,
        None,
        Some(modes),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "The journal records both file and mode preconditions"
)]
fn commit_locked_modes(
    root: &Path,
    expected_manifest: &[u8],
    new_manifest: &Manifest,
    changes: &BTreeMap<String, Vec<u8>>,
    deletions: &BTreeSet<String>,
    expected_observed: Option<&BTreeMap<String, String>>,
    observed_marker: Option<Option<&[u8]>>,
    modes: Option<&BTreeMap<String, ModeTransition>>,
) -> Result<Vec<u8>> {
    if recover(root)?.is_some() {
        return Err(CoreError::conflict("Project requires recovery"));
    }
    let marker = file_bytes(root, "project.json")?;
    let expected_marker = observed_marker.unwrap_or(Some(expected_manifest));
    if marker.as_deref() != expected_marker {
        return Err(CoreError::conflict("Project manifest changed on disk"));
    }
    let old_bytes = expected_manifest.to_vec();
    let old_marker_hash = marker.as_deref().map(hash);
    let old: Manifest =
        serde_json::from_slice(&old_bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
    if observed_marker.is_none() {
        for path in conflicts::blocked_generated(root, &old)? {
            if changes.contains_key(&path) || deletions.contains(&path) {
                return Err(CoreError::conflict(format!(
                    "Generated file {path} has an unresolved external change"
                )));
            }
        }
    }
    let old_files = expected_observed.unwrap_or(&old.files);
    if scan(root)? != *old_files {
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
    new.files = old_files.clone();
    let mut actual = BTreeMap::new();
    for (path, bytes) in changes {
        safe_path(root, path)?;
        let next = hash(bytes);
        if old_files.get(path) != Some(&next) {
            new.files.insert(path.clone(), next);
            actual.insert(path.clone(), Some(bytes.clone()));
        }
    }
    for path in deletions {
        safe_path(root, path)?;
        if changes.contains_key(path) || path == "document.json" {
            return Err(CoreError::invalid("Invalid or conflicting file deletion"));
        }
        if new.files.remove(path).is_some() {
            actual.insert(path.clone(), None);
        }
    }
    if let Some(modes) = modes {
        for (path, mode) in modes {
            if !checkpoint_dependency_path(path)
                || mode.before.is_some() != old_files.contains_key(path)
                || mode.after.is_some() != new.files.contains_key(path)
                || [mode.before, mode.after]
                    .into_iter()
                    .flatten()
                    .any(|bits| bits & !0o7777 != 0)
                || file_mode(root, path)? != mode.before
            {
                return Err(CoreError::conflict(format!(
                    "Invalid or changed file mode: {path}"
                )));
            }
            if mode.before != mode.after && !actual.contains_key(path) {
                let bytes = changes.get(path).ok_or_else(|| {
                    CoreError::invalid(format!("Missing mode-change content: {path}"))
                })?;
                actual.insert(path.clone(), Some(bytes.clone()));
            }
        }
    }
    let document = match actual.get("document.json") {
        Some(Some(bytes)) => bytes.clone(),
        Some(None) => return Err(CoreError::invalid("Cannot delete the document")),
        None => file_bytes(root, "document.json")?
            .ok_or_else(|| CoreError::invalid("Missing document"))?,
    };
    new.snapshot_hash = fingerprint(&document, &new.files);
    let new_bytes = json(&new)?;
    if old_marker_hash.as_deref() == Some(hash(&new_bytes).as_str()) {
        return Err(CoreError::conflict(
            "Observed and target commit markers are indistinguishable",
        ));
    }
    if actual.is_empty()
        && old.schema_version == new.schema_version
        && old.name == new.name
        && old.revision == new.revision
        && expected_observed.is_none()
    {
        return Ok(old_bytes);
    }
    conflicts::prepare_baselines(root, &old_bytes, &old, old_files, &new_bytes, &new, changes)?;
    let txdir = root
        .join(".aic-local/transactions")
        .join(Uuid::new_v4().to_string());
    fs::create_dir_all(&txdir)?;
    sync_dir(&root.join(".aic-local"))?;
    durable_write(&txdir.join("old-manifest"), &old_bytes)?;
    durable_write(&txdir.join("new-manifest"), &new_bytes)?;
    if let Some(Some(marker)) = observed_marker {
        durable_write(&txdir.join("observed-manifest"), marker)?;
    }
    let mut entries = Vec::new();
    for (i, (path, bytes)) in actual.iter().enumerate() {
        let old_content = file_bytes(root, path)?;
        let old_hash = old_content.as_deref().map(hash);
        if old_files.get(path) != old_hash.as_ref() {
            return Err(CoreError::conflict(format!("External edit of {path}")));
        }
        if let Some(mode) = modes.and_then(|modes| modes.get(path)) {
            if file_mode(root, path)? != mode.before {
                return Err(CoreError::conflict(format!("File mode changed: {path}")));
            }
        }
        let old_copy = if let Some(old_content) = &old_content {
            let name = format!("old-{i}");
            durable_write(&txdir.join(&name), old_content)?;
            Some(name)
        } else {
            None
        };
        let new_copy = if let Some(bytes) = bytes {
            let name = format!("new-{i}");
            durable_write(&txdir.join(&name), bytes)?;
            Some(name)
        } else {
            None
        };
        entries.push(Entry {
            path: path.clone(),
            old_hash,
            new_hash: bytes.as_deref().map(hash),
            old_copy,
            new_copy,
            modes: modes.and_then(|modes| modes.get(path).cloned()),
        });
    }
    let journal = Journal {
        old_manifest_hash: hash(&old_bytes),
        new_manifest_hash: hash(&new_bytes),
        old_revision: if modes.is_some_and(|items| !items.is_empty()) {
            JournalRevision::ModeAware {
                mode_aware_revision: old.revision,
            }
        } else {
            JournalRevision::Legacy(old.revision)
        },
        new_revision: new.revision,
        old_generation: old.storage_generation,
        new_generation: new.storage_generation,
        observed_files: expected_observed.cloned(),
        observed_marker: observed_marker.map(|_| ObservedMarker {
            hash: old_marker_hash.clone(),
        }),
        entries,
    };
    let pending_journal = txdir.join("journal.pending");
    durable_write(&pending_journal, &json(&journal)?)?;
    // Persist the prepared copies and temporary marker before publishing the journal.
    sync_dir(&txdir)?;
    // Once the recovery journal is active, cancellation must report TooLate.
    control::begin_commit()?;
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
        if !entry_matches(root, entry, false)? {
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
        replace_with_modes(
            root,
            &entry.path,
            entry.old_hash.as_deref(),
            actual.get(&entry.path).and_then(Option::as_deref),
            &retained.join(format!("entry-{index}")),
            entry.modes.as_ref(),
        )?;
        #[cfg(test)]
        interrupt_at(index + 1)?;
    }
    for entry in &journal.entries {
        if entry.modes.is_some() && !entry_matches(root, entry, true)? {
            return Err(CoreError::conflict(format!(
                "External mode change of {}",
                entry.path
            )));
        }
    }
    if scan(root)? != new.files || file_hash(root, "project.json")? != old_marker_hash {
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
        old_marker_hash.as_deref(),
        Some(&new_bytes),
        &retained.join("manifest"),
    )?;
    #[cfg(test)]
    interrupt_at(journal.entries.len() + 1)?;
    match retire_transaction(&txdir) {
        Ok(()) => {
            if let Err(error) = retention::record(&retained, &journal, &new) {
                // A missing receipt keeps this transaction indefinitely.
                eprintln!("AI Constructor: could not mark retained transaction complete: {error}");
            }
        }
        Err(error) => {
            // The commit marker is already durable; an unretired journal remains recoverable.
            eprintln!("AI Constructor: could not clean completed transaction: {error}");
        }
    }
    Ok(new_bytes)
}
