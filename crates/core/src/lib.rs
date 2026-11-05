mod model;
mod storage;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
};

use model::{Document, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use storage::Manifest;
use uuid::Uuid;

#[cfg(test)]
thread_local! {
    static AFTER_CHECKPOINT_SAVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CoreError {
    pub code: &'static str,
    pub message: String,
}

impl CoreError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_DOCUMENT", message)
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new("REVISION_CONFLICT", message)
    }
    pub(crate) fn locked(message: impl Into<String>) -> Self {
        Self::new("LOCKED_PROPERTY", message)
    }
    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new("UNSUPPORTED_FEATURE", message)
    }
    pub(crate) fn io(message: impl Into<String>) -> Self {
        Self::new("IO_ERROR", message)
    }
}
impl From<std::io::Error> for CoreError {
    fn from(value: std::io::Error) -> Self {
        Self::io(value.to_string())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct History {
    undo: Vec<Document>,
    redo: Vec<Document>,
    transaction_ids: BTreeSet<Uuid>,
    #[serde(default)]
    file_restore_barrier: Option<usize>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    id: Uuid,
    project_id: Uuid,
    revision: u64,
    document_hash: String,
    dependencies: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    executable_modes: Option<BTreeMap<String, u32>>,
}

struct State {
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    document: Value,
    history: History,
    pending: BTreeMap<String, Vec<u8>>,
    read_only_reason: Option<String>,
    dirty: bool,
}

pub struct ProjectSession {
    path: PathBuf,
    state: Mutex<State>,
}

pub fn create_project(path: impl AsRef<Path>, name: &str) -> Result<ProjectSession> {
    if name.trim().is_empty() {
        return Err(CoreError::invalid("Project name must not be empty"));
    }
    let document = Document::new(Uuid::new_v4());
    let value = serde_json::to_value(&document).map_err(|e| CoreError::invalid(e.to_string()))?;
    let bytes = storage::json(&value)?;
    let manifest = Manifest {
        schema_version: 1,
        project_id: document.project_id,
        name: name.trim().to_owned(),
        revision: 0,
        storage_generation: 0,
        snapshot_hash: String::new(),
        files: BTreeMap::new(),
    };
    storage::create(path.as_ref(), &manifest, &bytes)?;
    open_project(path)
}

pub fn open_project(path: impl AsRef<Path>) -> Result<ProjectSession> {
    let path = path.as_ref().to_path_buf();
    let loaded = storage::load(&path)?;
    let document: Value = serde_json::from_slice(&loaded.document_bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid document: {e}")))?;
    let schema_version = document
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| CoreError::invalid("Missing document schemaVersion"))?;
    let mut read_only_reason = loaded.read_only_reason;
    if schema_version > 1 || loaded.manifest.schema_version > 1 {
        read_only_reason = Some("This project uses a newer schema and is open read-only".into());
    } else if schema_version != 1 || loaded.manifest.schema_version != 1 {
        return Err(CoreError::unsupported(
            "This project schema cannot be migrated",
        ));
    } else {
        let model: Document = serde_json::from_value(document.clone())
            .map_err(|e| CoreError::invalid(format!("Invalid document: {e}")))?;
        model.validate()?;
        if let Err(error) = model.validate_files(&loaded.manifest.files) {
            read_only_reason = Some(error.to_string());
        }
        if !model.editable() {
            read_only_reason =
                Some("This document contains features not yet supported for editing".into());
        }
        if model.project_id != loaded.manifest.project_id
            || model.revision != loaded.manifest.revision
        {
            read_only_reason = Some("Document and project manifest do not match".into());
        }
        if storage::fingerprint(&loaded.document_bytes, &loaded.manifest.files)
            != loaded.manifest.snapshot_hash
        {
            read_only_reason = Some("Document fingerprint does not match the manifest".into());
        }
    }
    let history = if read_only_reason.is_some() {
        History::default()
    } else {
        match std::fs::read(path.join("history/state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::invalid(format!("Invalid history: {e}")))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => History::default(),
            Err(e) => return Err(e.into()),
        }
    };
    Ok(ProjectSession {
        path,
        state: Mutex::new(State {
            manifest: loaded.manifest,
            manifest_bytes: loaded.manifest_bytes,
            document,
            history,
            pending: BTreeMap::new(),
            read_only_reason,
            dirty: false,
        }),
    })
}

impl State {
    fn ensure_writable(&self) -> Result<()> {
        if let Some(reason) = &self.read_only_reason {
            return Err(CoreError::unsupported(reason));
        }
        Ok(())
    }
    fn model(&self) -> Result<Document> {
        serde_json::from_value(self.document.clone()).map_err(|e| CoreError::invalid(e.to_string()))
    }
    fn fingerprint(&self) -> Result<String> {
        Ok(storage::fingerprint(
            &storage::json(&self.document)?,
            &self.manifest.files,
        ))
    }
    fn snapshot_for(&self, document: &Value, dirty: bool) -> Result<String> {
        let fingerprint = storage::fingerprint(&storage::json(document)?, &self.manifest.files);
        let snapshot = serde_json::json!({
            "document": document, "fingerprint": fingerprint, "name": self.manifest.name,
            "readOnly": self.read_only_reason.is_some(), "readOnlyReason": self.read_only_reason,
            "dirty": dirty,
        });
        serde_json::to_string(&snapshot).map_err(|e| CoreError::invalid(e.to_string()))
    }
    fn snapshot(&self) -> Result<String> {
        self.snapshot_for(&self.document, self.dirty)
    }
    fn record(&mut self, id: Uuid, description: Value, document: Value) -> Result<String> {
        let transaction = storage::json(&description)?;
        let history = storage::json(&self.history)?;
        let snapshot = self.snapshot_for(&document, true)?;
        self.pending
            .insert(format!("history/transactions/{id}.json"), transaction);
        self.pending.insert("history/state.json".into(), history);
        self.document = document;
        self.dirty = true;
        Ok(snapshot)
    }
}

impl ProjectSession {
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| CoreError::io("Project session lock poisoned"))
    }

    pub fn snapshot_json(&self) -> Result<String> {
        self.state()?.snapshot()
    }

    pub fn apply_transaction(&self, transaction_json: &str) -> Result<String> {
        let transaction: Transaction = serde_json::from_str(transaction_json).map_err(|e| {
            CoreError::unsupported(format!("Invalid or unsupported transaction: {e}"))
        })?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let before = state.model()?;
        if transaction.base_revision != before.revision
            || transaction.base_fingerprint != state.fingerprint()?
        {
            return Err(CoreError::conflict(
                "Transaction is based on a different revision or fingerprint",
            ));
        }
        if state
            .history
            .transaction_ids
            .contains(&transaction.transaction_id)
        {
            return Err(CoreError::conflict("Transaction ID has already been used"));
        }
        let mut next = before.clone();
        next.apply(&transaction)?;
        next.validate_files(&state.manifest.files)?;
        let document =
            serde_json::to_value(&next).map_err(|e| CoreError::invalid(e.to_string()))?;
        let id = transaction.transaction_id;
        let description =
            serde_json::json!({"transaction": transaction, "before": before, "after": next});
        let redo = std::mem::take(&mut state.history.redo);
        state.history.undo.push(before);
        state.history.transaction_ids.insert(id);
        let result = state.record(id, description, document);
        if result.is_err() {
            state.history.undo.pop();
            state.history.redo = redo;
            state.history.transaction_ids.remove(&id);
        }
        result
    }

    pub fn undo(&self) -> Result<String> {
        self.travel(false)
    }
    pub fn redo(&self) -> Result<String> {
        self.travel(true)
    }

    fn travel(&self, redo: bool) -> Result<String> {
        let mut state = self.state()?;
        state.ensure_writable()?;
        if !redo
            && state
                .history
                .file_restore_barrier
                .is_some_and(|n| state.history.undo.len() <= n)
        {
            return Err(CoreError::unsupported(
                "Cannot undo a checkpoint restore that changed project files",
            ));
        }
        let before = state.model()?;
        let mut next = if redo {
            state.history.redo.last()
        } else {
            state.history.undo.last()
        }
        .cloned()
        .ok_or_else(|| {
            CoreError::unsupported(if redo {
                "Nothing to redo"
            } else {
                "Nothing to undo"
            })
        })?;
        next.revision = before
            .revision
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Revision overflow"))?;
        next.mark_restored_properties(&before);
        next.validate()?;
        next.validate_files(&state.manifest.files)?;
        let document =
            serde_json::to_value(&next).map_err(|e| CoreError::invalid(e.to_string()))?;
        let id = loop {
            let candidate = Uuid::new_v4();
            if !state.history.transaction_ids.contains(&candidate) {
                break candidate;
            }
        };
        let description = serde_json::json!({"transactionId": id, "kind": if redo { "redo" } else { "undo" }, "before": before, "after": next});
        let selected = if redo {
            state.history.redo.pop().expect("history entry was checked")
        } else {
            state.history.undo.pop().expect("history entry was checked")
        };
        if redo {
            state.history.undo.push(before);
        } else {
            state.history.redo.push(before);
        }
        state.history.transaction_ids.insert(id);
        let result = state.record(id, description, document);
        if result.is_err() {
            state.history.transaction_ids.remove(&id);
            if redo {
                state.history.undo.pop();
                state.history.redo.push(selected);
            } else {
                state.history.redo.pop();
                state.history.undo.push(selected);
            }
        }
        result
    }

    pub fn rename(&self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::invalid("Project name must not be empty"));
        }
        let mut state = self.state()?;
        state.ensure_writable()?;
        if state.manifest.name != name {
            state.manifest.name = name.to_owned();
            state.dirty = true;
        }
        Ok(())
    }

    pub fn duplicate_to(
        &self,
        destination: impl AsRef<Path>,
        name: &str,
    ) -> Result<ProjectSession> {
        self.save()?;
        let state = self.state()?;
        state.ensure_writable()?;
        if state
            .manifest
            .files
            .keys()
            .any(|path| path.starts_with("chats/"))
        {
            return Err(CoreError::unsupported(
                "Duplicating projects with chats is not supported",
            ));
        }
        let destination = destination.as_ref();
        let parent = destination
            .parent()
            .ok_or_else(|| CoreError::invalid("Destination needs a parent directory"))?;
        if parent
            .canonicalize()?
            .starts_with(self.path.canonicalize()?)
        {
            return Err(CoreError::invalid(
                "A duplicate cannot be placed inside the source project",
            ));
        }
        let inputs = storage::snapshot_inputs(&self.path, &state.manifest_bytes)?;
        let source = state.model()?;
        let duplicate = create_project(destination, name)?;
        {
            let mut target = duplicate.state()?;
            let mut document = source;
            document.project_id = target.manifest.project_id;
            target.document =
                serde_json::to_value(document).map_err(|e| CoreError::invalid(e.to_string()))?;
            for (path, bytes) in inputs {
                if path != "document.json" {
                    target.pending.insert(path, bytes);
                }
            }
            target.dirty = true;
        }
        duplicate.save()?;
        Ok(duplicate)
    }

    pub fn create_checkpoint(&self) -> Result<String> {
        self.save()?;
        #[cfg(test)]
        AFTER_CHECKPOINT_SAVE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let mut state = self.state()?;
        state.ensure_writable()?;
        let inputs = storage::snapshot_inputs(&self.path, &state.manifest_bytes)?;
        let doc = inputs
            .get("document.json")
            .ok_or_else(|| CoreError::invalid("Missing document"))?;
        let captured: Document =
            serde_json::from_slice(doc).map_err(|e| CoreError::invalid(e.to_string()))?;
        let saved: Manifest = serde_json::from_slice(&state.manifest_bytes)
            .map_err(|e| CoreError::invalid(e.to_string()))?;
        if captured.project_id != saved.project_id || captured.revision != saved.revision {
            return Err(CoreError::conflict(
                "Saved document and manifest do not match",
            ));
        }
        captured.validate()?;
        let mut checkpoint = Checkpoint {
            id: Uuid::new_v4(),
            project_id: saved.project_id,
            revision: saved.revision,
            document_hash: storage::hash(doc),
            dependencies: BTreeMap::new(),
            executable_modes: None,
        };
        let mut changes = BTreeMap::new();
        let mut executable_modes = BTreeMap::new();
        for (path, bytes) in &inputs {
            let digest = storage::hash(bytes);
            let blob = format!("checkpoints/blobs/{digest}");
            if saved.files.contains_key(&blob) {
                storage::checkpoint_blob(&self.path, &saved, &digest)?;
            } else {
                changes.entry(blob).or_insert_with(|| bytes.clone());
            }
            if path != "document.json" {
                if !storage::checkpoint_dependency_path(path) {
                    return Err(CoreError::unsupported(format!(
                        "Unsupported checkpoint path: {path}"
                    )));
                }
                storage::validate_checkpoint_asset(path, &digest)?;
                executable_modes.insert(
                    path.clone(),
                    storage::checkpoint_executable_mode(&self.path, path)?,
                );
                checkpoint.dependencies.insert(path.clone(), digest);
            }
        }
        for path in checkpoint.dependencies.keys() {
            if Some(&storage::checkpoint_executable_mode(&self.path, path)?)
                != executable_modes.get(path)
            {
                return Err(CoreError::conflict(format!(
                    "File permissions changed while creating checkpoint: {path}"
                )));
            }
        }
        checkpoint.executable_modes = Some(executable_modes);
        captured.validate_files(&checkpoint.dependencies)?;
        let metadata_path = format!("checkpoints/{}.json", checkpoint.id);
        if saved.files.contains_key(&metadata_path) {
            return Err(CoreError::conflict("Checkpoint ID already exists"));
        }
        changes.insert(metadata_path, storage::json(&checkpoint)?);
        let committed = match storage::commit(&self.path, &state.manifest_bytes, &saved, &changes) {
            Ok(committed) => committed,
            Err(error) => {
                state.read_only_reason =
                    Some("Checkpoint publication failed; reopen the project to recover".into());
                return Err(error);
            }
        };
        let mut manifest: Manifest =
            serde_json::from_slice(&committed).map_err(|e| CoreError::invalid(e.to_string()))?;
        // A concurrent in-memory rename after the initial save is still pending.
        manifest.name.clone_from(&state.manifest.name);
        state.manifest = manifest;
        state.manifest_bytes = committed;
        Ok(checkpoint.id.to_string())
    }

    pub fn checkpoints_json(&self) -> Result<String> {
        let state = self.state()?;
        storage::snapshot_inputs(&self.path, &state.manifest_bytes)?;
        let mut checkpoints = Vec::new();
        for path in state.manifest.files.keys() {
            if !path.starts_with("checkpoints/")
                || path.starts_with("checkpoints/blobs/")
                || !path.ends_with(".json")
            {
                continue;
            }
            let bytes = storage::tracked_bytes(&self.path, &state.manifest, path)?;
            let checkpoint: Checkpoint =
                serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
            checkpoints.push(checkpoint);
        }
        String::from_utf8(storage::json(&checkpoints)?)
            .map_err(|e| CoreError::invalid(e.to_string()))
    }

    pub fn restore_checkpoint(&self, id: &str) -> Result<String> {
        let id = Uuid::parse_str(id).map_err(|e| CoreError::invalid(e.to_string()))?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let inputs = storage::snapshot_inputs(&self.path, &state.manifest_bytes)?;
        let metadata_path = format!("checkpoints/{id}.json");
        if !state.manifest.files.contains_key(&metadata_path) {
            return Err(CoreError::invalid("Checkpoint does not exist"));
        }
        let metadata = storage::tracked_bytes(&self.path, &state.manifest, &metadata_path)?;
        let checkpoint: Checkpoint =
            serde_json::from_slice(&metadata).map_err(|e| CoreError::invalid(e.to_string()))?;
        if checkpoint.id != id || checkpoint.project_id != state.manifest.project_id {
            return Err(CoreError::invalid("Checkpoint belongs to another project"));
        }
        if state
            .pending
            .keys()
            .any(|path| path.starts_with("src/") || path.starts_with("assets/"))
        {
            return Err(CoreError::unsupported(
                "Save pending code and resource changes before restoring a checkpoint",
            ));
        }
        let bytes =
            storage::checkpoint_blob(&self.path, &state.manifest, &checkpoint.document_hash)?;
        let mut next: Document =
            serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
        let before = state.model()?;
        if next.project_id != before.project_id || next.revision != checkpoint.revision {
            return Err(CoreError::invalid(
                "Checkpoint document metadata does not match",
            ));
        }
        if !next.editable() {
            return Err(CoreError::unsupported(
                "Checkpoint document is not editable",
            ));
        }
        next.validate()?;
        next.validate_files(&checkpoint.dependencies)?;
        if checkpoint.executable_modes.as_ref().is_some_and(|modes| {
            modes.len() != checkpoint.dependencies.len()
                || modes.iter().any(|(path, mode)| {
                    !checkpoint.dependencies.contains_key(path) || *mode & !0o111 != 0
                })
        }) {
            return Err(CoreError::invalid(
                "Invalid checkpoint executable mode metadata",
            ));
        }

        let mut changes = state.pending.clone();
        let mut deletions = BTreeSet::new();
        let mut file_changes = BTreeMap::new();
        for (path, digest) in &checkpoint.dependencies {
            if !storage::checkpoint_dependency_path(path) {
                return Err(CoreError::invalid(format!(
                    "Invalid checkpoint dependency path: {path}"
                )));
            }
            let mut parent = Path::new(path).parent();
            while let Some(directory) = parent {
                if directory
                    .to_str()
                    .is_some_and(|name| checkpoint.dependencies.contains_key(name))
                {
                    return Err(CoreError::invalid(format!(
                        "Checkpoint file/directory collision: {path}"
                    )));
                }
                parent = directory.parent();
            }
            storage::validate_checkpoint_asset(path, digest)?;
            let content = storage::checkpoint_blob(&self.path, &state.manifest, digest)?;
            let old_hash = inputs.get(path).map(|bytes| storage::hash(bytes));
            if let Some(modes) = &checkpoint.executable_modes {
                let expected_mode = *modes
                    .get(path)
                    .ok_or_else(|| CoreError::invalid("Missing checkpoint executable mode"))?;
                let current_mode = if old_hash.is_some() {
                    Some(storage::checkpoint_executable_mode(&self.path, path)?)
                } else {
                    None
                };
                if (expected_mode != 0
                    && (current_mode != Some(expected_mode) || old_hash.as_ref() != Some(digest)))
                    || (expected_mode == 0 && current_mode.is_some_and(|mode| mode != 0))
                {
                    return Err(CoreError::unsupported(format!(
                        "Cannot safely restore executable permissions for {path}"
                    )));
                }
            } else if old_hash.as_ref() != Some(digest) {
                return Err(CoreError::unsupported(format!("Legacy checkpoint does not record executable permissions for changed or missing file {path}")));
            }
            if path.starts_with("assets/")
                && old_hash.is_some()
                && old_hash.as_ref() != Some(digest)
            {
                return Err(CoreError::conflict(format!(
                    "Checkpoint asset has different contents on disk: {path}"
                )));
            }
            if old_hash.as_ref() != Some(digest) {
                changes.insert(path.clone(), content);
                file_changes.insert(
                    path.clone(),
                    serde_json::json!({"beforeHash": old_hash, "afterHash": digest}),
                );
            }
        }
        for (path, content) in &inputs {
            if path == "document.json" {
                continue;
            }
            if !storage::checkpoint_dependency_path(path) {
                return Err(CoreError::unsupported(format!(
                    "Unsupported checkpoint path: {path}"
                )));
            }
            // Later assets may still be referenced by chat history; keep them as orphans.
            if !path.starts_with("assets/") && !checkpoint.dependencies.contains_key(path) {
                deletions.insert(path.clone());
                file_changes.insert(
                    path.clone(),
                    serde_json::json!({"beforeHash": storage::hash(content), "afterHash": null}),
                );
            }
        }
        if !file_changes.is_empty() {
            storage::validate_checkpoint_tree(&self.path, &checkpoint.dependencies)?;
        }
        let mut directory_names = BTreeMap::new();
        // Preserve the inverse file contents in the same content-addressed store.
        for path in file_changes.keys() {
            storage::checkpoint_destination(&self.path, path, &mut directory_names)?;
            if let Some(content) = inputs.get(path) {
                let digest = storage::hash(content);
                let blob = format!("checkpoints/blobs/{digest}");
                if state.manifest.files.contains_key(&blob) {
                    storage::checkpoint_blob(&self.path, &state.manifest, &digest)?;
                } else if let Some(existing) = changes.get(&blob) {
                    if storage::hash(existing) != digest {
                        return Err(CoreError::conflict("Pending checkpoint blob has changed"));
                    }
                } else {
                    changes.insert(blob, content.clone());
                }
            }
        }
        next.revision = before
            .revision
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Revision overflow"))?;
        next.mark_restored_properties(&before);
        next.validate()?;
        let document =
            serde_json::to_value(&next).map_err(|e| CoreError::invalid(e.to_string()))?;
        let transaction_id = Uuid::new_v4();
        let transaction_path = format!("history/transactions/{transaction_id}.json");
        if state.history.transaction_ids.contains(&transaction_id)
            || state.manifest.files.contains_key(&transaction_path)
            || changes.contains_key(&transaction_path)
        {
            return Err(CoreError::conflict("Transaction ID has already been used"));
        }
        let mut history = state.history.clone();
        history.undo.push(before.clone());
        history.redo.clear();
        if !file_changes.is_empty() {
            history.file_restore_barrier = Some(history.undo.len());
        }
        history.transaction_ids.insert(transaction_id);
        changes.insert(
            transaction_path,
            storage::json(&serde_json::json!({
                "transactionId": transaction_id, "kind": "restoreCheckpoint",
                "checkpointId": id, "before": before, "after": next, "fileChanges": file_changes
            }))?,
        );
        changes.insert("history/state.json".into(), storage::json(&history)?);
        changes.insert("document.json".into(), storage::json(&document)?);
        let mut manifest = state.manifest.clone();
        manifest.revision = next.revision;
        let committed = match storage::commit_with_deletions(
            &self.path,
            &state.manifest_bytes,
            &manifest,
            &changes,
            &deletions,
        ) {
            Ok(committed) => committed,
            Err(error) => {
                state.read_only_reason =
                    Some("Checkpoint restoration failed; reopen the project to recover".into());
                return Err(error);
            }
        };
        state.manifest =
            serde_json::from_slice(&committed).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.manifest_bytes = committed;
        state.document = document;
        state.history = history;
        state.pending.clear();
        state.dirty = false;
        state.snapshot()
    }

    pub fn save(&self) -> Result<()> {
        let mut state = self.state()?;
        state.ensure_writable()?;
        if !state.dirty {
            return Ok(());
        }
        let mut changes = state.pending.clone();
        changes.insert("document.json".into(), storage::json(&state.document)?);
        let mut manifest = state.manifest.clone();
        manifest.revision = state.model()?.revision;
        let committed = storage::commit(&self.path, &state.manifest_bytes, &manifest, &changes)?;
        state.manifest =
            serde_json::from_slice(&committed).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.manifest_bytes = committed;
        state.pending.clear();
        state.dirty = false;
        Ok(())
    }
}
