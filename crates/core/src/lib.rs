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
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    id: Uuid,
    project_id: Uuid,
    revision: u64,
    document_hash: String,
    dependencies: BTreeMap<String, String>,
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
    fn snapshot(&self) -> Result<String> {
        let snapshot = serde_json::json!({
            "document": self.document, "fingerprint": self.fingerprint()?, "name": self.manifest.name,
            "readOnly": self.read_only_reason.is_some(), "readOnlyReason": self.read_only_reason,
            "dirty": self.dirty,
        });
        serde_json::to_string(&snapshot).map_err(|e| CoreError::invalid(e.to_string()))
    }
    fn record(&mut self, id: Uuid, description: Value) -> Result<()> {
        self.pending.insert(
            format!("history/transactions/{id}.json"),
            storage::json(&description)?,
        );
        self.history.transaction_ids.insert(id);
        self.pending
            .insert("history/state.json".into(), storage::json(&self.history)?);
        self.dirty = true;
        Ok(())
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
        let description =
            serde_json::json!({"transaction": transaction, "before": before, "after": next});
        state.document =
            serde_json::to_value(&next).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.history.undo.push(before);
        state.history.redo.clear();
        state.record(transaction.transaction_id, description)?;
        state.snapshot()
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
        let before = state.model()?;
        let mut next = if redo {
            state.history.redo.pop()
        } else {
            state.history.undo.pop()
        }
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
        if redo {
            state.history.undo.push(before.clone());
        } else {
            state.history.redo.push(before.clone());
        }
        let id = Uuid::new_v4();
        let description = serde_json::json!({"transactionId": id, "kind": if redo { "redo" } else { "undo" }, "before": before, "after": next});
        state.document =
            serde_json::to_value(next).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.record(id, description)?;
        state.snapshot()
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
        let mut checkpoint = Checkpoint {
            id: Uuid::new_v4(),
            project_id: state.manifest.project_id,
            revision: state.manifest.revision,
            document_hash: storage::hash(doc),
            dependencies: BTreeMap::new(),
        };
        for (path, bytes) in &inputs {
            let hash = storage::hash(bytes);
            state
                .pending
                .insert(format!("checkpoints/blobs/{hash}"), bytes.clone());
            if path != "document.json" {
                checkpoint.dependencies.insert(path.clone(), hash);
            }
        }
        state.pending.insert(
            format!("checkpoints/{}.json", checkpoint.id),
            storage::json(&checkpoint)?,
        );
        state.dirty = true;
        let id = checkpoint.id.to_string();
        drop(state);
        self.save()?;
        Ok(id)
    }

    pub fn checkpoints_json(&self) -> Result<String> {
        let state = self.state()?;
        storage::snapshot_inputs(&self.path, &state.manifest_bytes)?;
        let mut checkpoints = Vec::new();
        for (path, expected) in &state.manifest.files {
            if !path.starts_with("checkpoints/")
                || path.starts_with("checkpoints/blobs/")
                || !path.ends_with(".json")
            {
                continue;
            }
            let bytes = std::fs::read(self.path.join(path))?;
            if storage::hash(&bytes) != *expected {
                return Err(CoreError::conflict("Checkpoint changed on disk"));
            }
            let checkpoint: Checkpoint =
                serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
            checkpoints.push(checkpoint);
        }
        String::from_utf8(storage::json(&checkpoints)?)
            .map_err(|e| CoreError::invalid(e.to_string()))
    }

    pub fn restore_checkpoint(&self, id: &str) -> Result<String> {
        self.save()?;
        let id = Uuid::parse_str(id).map_err(|e| CoreError::invalid(e.to_string()))?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        let inputs = storage::snapshot_inputs(&self.path, &state.manifest_bytes)?;
        let metadata_path = format!("checkpoints/{id}.json");
        let expected = state
            .manifest
            .files
            .get(&metadata_path)
            .ok_or_else(|| CoreError::invalid("Checkpoint does not exist"))?;
        let metadata = std::fs::read(self.path.join(metadata_path))?;
        if storage::hash(&metadata) != *expected {
            return Err(CoreError::conflict("Checkpoint changed on disk"));
        }
        let checkpoint: Checkpoint =
            serde_json::from_slice(&metadata).map_err(|e| CoreError::invalid(e.to_string()))?;
        if checkpoint.id != id || checkpoint.project_id != state.manifest.project_id {
            return Err(CoreError::invalid("Checkpoint belongs to another project"));
        }
        let current_dependencies: BTreeMap<_, _> = inputs
            .iter()
            .filter(|(path, _)| path.as_str() != "document.json")
            .map(|(path, bytes)| (path.clone(), storage::hash(bytes)))
            .collect();
        if current_dependencies != checkpoint.dependencies {
            return Err(CoreError::unsupported(
                "Restoring changed code or resources is not supported; no files were modified",
            ));
        }
        if checkpoint.document_hash.len() != 64
            || !checkpoint
                .document_hash
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(CoreError::invalid("Invalid checkpoint hash"));
        }
        let blob_path = format!("checkpoints/blobs/{}", checkpoint.document_hash);
        let bytes = std::fs::read(self.path.join(&blob_path))?;
        if storage::hash(&bytes) != checkpoint.document_hash
            || state.manifest.files.get(&blob_path) != Some(&checkpoint.document_hash)
        {
            return Err(CoreError::conflict(
                "Checkpoint document is missing or modified",
            ));
        }
        let mut next: Document =
            serde_json::from_slice(&bytes).map_err(|e| CoreError::invalid(e.to_string()))?;
        let before = state.model()?;
        if next.project_id != before.project_id || !next.editable() {
            return Err(CoreError::unsupported(
                "Checkpoint document is not editable",
            ));
        }
        next.revision = before
            .revision
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Revision overflow"))?;
        next.mark_restored_properties(&before);
        next.validate()?;
        state.history.undo.push(before.clone());
        state.history.redo.clear();
        let transaction_id = Uuid::new_v4();
        state.document =
            serde_json::to_value(&next).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.record(transaction_id, serde_json::json!({"transactionId": transaction_id, "kind": "restoreCheckpoint", "checkpointId": id, "before": before, "after": next}))?;
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
