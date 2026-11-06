pub mod cancellation;
mod model;
mod storage;

pub use storage::control::{create_project_controlled, open_project_controlled};

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
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
    pub(crate) fn import_limit(message: impl Into<String>) -> Self {
        Self::new("IMPORT_LIMIT_EXCEEDED", message)
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

fn duplicate_history(bytes: &[u8], source_project: Uuid, target_project: Uuid) -> Result<History> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| CoreError::invalid(format!("Invalid history: {error}")))?;
    let fields = ["undo", "redo", "transaction_ids", "file_restore_barrier"];
    if value
        .as_object()
        .is_none_or(|object| object.keys().any(|key| !fields.contains(&key.as_str())))
    {
        return Err(CoreError::unsupported(
            "Cannot duplicate history with unknown fields",
        ));
    }
    let mut history: History = serde_json::from_value(value)
        .map_err(|error| CoreError::invalid(format!("Invalid history: {error}")))?;
    for document in history.undo.iter_mut().chain(history.redo.iter_mut()) {
        if document.project_id != source_project {
            return Err(CoreError::invalid("History belongs to another project"));
        }
        document.project_id = target_project;
    }
    Ok(history)
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

fn duplicate_checkpoints(
    inputs: &BTreeMap<String, Vec<u8>>,
    source_project: Uuid,
    target_project: Uuid,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut copied = BTreeMap::new();
    for (path, bytes) in inputs
        .iter()
        .filter(|(path, _)| path.starts_with("checkpoints/"))
    {
        if let Some(digest) = path.strip_prefix("checkpoints/blobs/") {
            if digest.len() != 64 || storage::hash(bytes) != digest {
                return Err(CoreError::invalid(format!(
                    "Invalid checkpoint blob {path}"
                )));
            }
            copied.insert(path.clone(), bytes.clone());
            continue;
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid checkpoint {path}: {e}")))?;
        let known = [
            "id",
            "projectId",
            "revision",
            "documentHash",
            "dependencies",
            "executableModes",
        ];
        if value
            .as_object()
            .is_none_or(|object| object.keys().any(|key| !known.contains(&key.as_str())))
        {
            return Err(CoreError::unsupported(format!(
                "Unsupported checkpoint metadata {path}"
            )));
        }
        let mut checkpoint: Checkpoint = serde_json::from_value(value)
            .map_err(|e| CoreError::invalid(format!("Invalid checkpoint {path}: {e}")))?;
        if checkpoint.project_id != source_project
            || *path != format!("checkpoints/{}.json", checkpoint.id)
        {
            return Err(CoreError::invalid(format!(
                "Checkpoint identity does not match {path}"
            )));
        }
        let blob = format!("checkpoints/blobs/{}", checkpoint.document_hash);
        let doc = inputs
            .get(&blob)
            .ok_or_else(|| CoreError::invalid(format!("Missing {blob}")))?;
        if storage::hash(doc) != checkpoint.document_hash {
            return Err(CoreError::invalid(format!(
                "Checkpoint document is corrupted: {path}"
            )));
        }
        let mut document: Document = serde_json::from_slice(doc)
            .map_err(|e| CoreError::unsupported(format!("Unsupported checkpoint document: {e}")))?;
        if document.project_id != source_project || document.revision != checkpoint.revision {
            return Err(CoreError::invalid(format!(
                "Checkpoint document identity does not match {path}"
            )));
        }
        document.validate()?;
        document.validate_files(&checkpoint.dependencies)?;
        for (dependency, digest) in &checkpoint.dependencies {
            if !storage::checkpoint_dependency_path(dependency) {
                return Err(CoreError::invalid(format!(
                    "Unsafe checkpoint dependency {dependency}"
                )));
            }
            storage::validate_checkpoint_asset(dependency, digest)?;
            let blob_path = format!("checkpoints/blobs/{digest}");
            if inputs
                .get(&blob_path)
                .is_none_or(|bytes| storage::hash(bytes) != *digest)
            {
                return Err(CoreError::invalid(format!(
                    "Missing or corrupted checkpoint blob {digest}"
                )));
            }
        }
        document.project_id = target_project;
        document.validate()?;
        let doc = storage::json(&document)?;
        checkpoint.project_id = target_project;
        checkpoint.document_hash = storage::hash(&doc);
        copied.insert(
            format!("checkpoints/blobs/{}", checkpoint.document_hash),
            doc,
        );
        copied.insert(path.clone(), storage::json(&checkpoint)?);
    }
    Ok(copied)
}

struct State {
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    document: Value,
    history: History,
    pending: BTreeMap<String, Vec<u8>>,
    read_only_reason: Option<String>,
    local_generation: u64,
    dirty: bool,
}

pub struct ProjectSession {
    path: PathBuf,
    state: Mutex<State>,
    retention_limit: AtomicU64,
}

struct StagedProject(Option<PathBuf>);

impl StagedProject {
    fn new(destination: &Path) -> Result<Self> {
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let path = parent.join(format!(".aic-stage-{}", Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        let stage = Self(Some(path));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(stage.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(stage)
    }

    fn path(&self) -> &Path {
        self.0.as_deref().expect("staged directory is present")
    }

    fn publish(mut self, destination: &Path) -> Result<()> {
        storage::publish_directory(self.path(), destination)?;
        self.0 = None;
        Ok(())
    }
}

impl Drop for StagedProject {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

fn create_in_place(path: &Path, name: &str) -> Result<ProjectSession> {
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
    storage::create(path, &manifest, &bytes)?;
    open_project(path)
}

pub fn create_project(path: impl AsRef<Path>, name: &str) -> Result<ProjectSession> {
    let path = path.as_ref();
    let stage = StagedProject::new(path)?;
    let project = storage::control::staging(|| create_in_place(stage.path(), name))?;
    drop(project);
    stage.publish(path)?;
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
    if storage::manifest_has_unknown_fields(&loaded.manifest_bytes)? {
        read_only_reason =
            Some("This project manifest contains unsupported fields and is open read-only".into());
    }
    if schema_version > 1 || loaded.manifest.schema_version > 1 {
        read_only_reason = Some("This project uses a newer schema and is open read-only".into());
    } else if schema_version != 1 || loaded.manifest.schema_version != 1 {
        return Err(CoreError::unsupported(
            "This project schema cannot be migrated",
        ));
    } else {
        match serde_json::from_value::<Document>(document.clone()) {
            Ok(model) => {
                model.validate()?;
                if let Err(error) = model.validate_files(&loaded.manifest.files) {
                    read_only_reason = Some(error.to_string());
                }
                if !model.editable() {
                    read_only_reason = Some(
                        "This document contains features not yet supported for editing".into(),
                    );
                }
                if model.project_id != loaded.manifest.project_id
                    || model.revision != loaded.manifest.revision
                {
                    read_only_reason = Some("Document and project manifest do not match".into());
                }
            }
            Err(error)
                if error.to_string().contains("unknown field")
                    || error.to_string().contains("unknown variant") =>
            {
                read_only_reason = Some(format!(
                    "This document contains unsupported data and is open read-only: {error}"
                ));
            }
            Err(error) => return Err(CoreError::invalid(format!("Invalid document: {error}"))),
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
        retention_limit: AtomicU64::new(storage::retention::UNLIMITED),
        state: Mutex::new(State {
            manifest: loaded.manifest,
            manifest_bytes: loaded.manifest_bytes,
            document,
            history,
            pending: BTreeMap::new(),
            local_generation: 0,
            read_only_reason,
            dirty: false,
        }),
    })
}

impl State {
    fn next_generation(&self) -> Result<u64> {
        self.local_generation
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Local state generation overflow"))
    }
    fn bump(&mut self) -> Result<()> {
        self.local_generation = self.next_generation()?;
        Ok(())
    }

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
        self.bump()?;
        self.pending
            .insert(format!("history/transactions/{id}.json"), transaction);
        self.pending.insert("history/state.json".into(), history);
        self.document = document;
        self.dirty = true;
        Ok(snapshot)
    }
}

impl ProjectSession {
    /// Applies only to subsequent completed saves, not to opening a project.
    pub fn set_retention_limit(&self, limit: Option<u32>) {
        self.retention_limit.store(
            limit
                .map(u64::from)
                .unwrap_or(storage::retention::UNLIMITED),
            Ordering::SeqCst,
        );
    }

    fn after_commit(&self, before: &[u8], after: &[u8], project: Uuid) {
        if before == after {
            return;
        }
        if let Err(error) = storage::retention::prune(&self.path, project, &self.retention_limit) {
            // The save has already committed; cleanup errors must not make it look unsaved.
            eprintln!("AI Constructor: retained cleanup deferred: {error}");
        }
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        loop {
            storage::control::check()?;
            match self.state.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(CoreError::io("Project session lock poisoned"));
                }
            }
        }
    }

    fn preserve_conflict_error(&self, state: &State, error: CoreError) -> CoreError {
        if error.code != "REVISION_CONFLICT" {
            return error;
        }
        match storage::capture_external(&self.path, state) {
            Ok(()) => error,
            Err(archive_error) => CoreError::new(
                "CONFLICT_ARCHIVE_FAILED",
                format!(
                    "{}; could not preserve local and external versions: {}",
                    error.message, archive_error
                ),
            ),
        }
    }

    pub fn snapshot_json(&self) -> Result<String> {
        self.state()?.snapshot()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the flat API aligned with its UniFFI bridge"
    )]
    pub fn transform_point_json(
        &self,
        node_id: &str,
        x: f64,
        y: f64,
        zoom: f64,
        pan_x: f64,
        pan_y: f64,
        inverse: bool,
        source_pixels: bool,
    ) -> Result<String> {
        let node_id =
            Uuid::parse_str(node_id).map_err(|_| CoreError::invalid("Invalid node ID"))?;
        let document = self.state()?.model()?;
        let (x, y) = document.transform_point(
            &self.path,
            node_id,
            (x, y),
            (zoom, pan_x, pan_y),
            inverse,
            source_pixels,
        )?;
        serde_json::to_string(&serde_json::json!({"x": x, "y": y}))
            .map_err(|e| CoreError::invalid(e.to_string()))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Keep the flat API aligned with its UniFFI bridge"
    )]
    pub fn transform_point_at_breakpoint_json(
        &self,
        node_id: &str,
        x: f64,
        y: f64,
        zoom: f64,
        pan_x: f64,
        pan_y: f64,
        inverse: bool,
        source_pixels: bool,
        breakpoint: f64,
    ) -> Result<String> {
        let node_id =
            Uuid::parse_str(node_id).map_err(|_| CoreError::invalid("Invalid node ID"))?;
        let document = self.state()?.model()?;
        let (x, y) = document.transform_point_at_breakpoint(
            &self.path,
            node_id,
            (x, y),
            (zoom, pan_x, pan_y),
            inverse,
            source_pixels,
            breakpoint,
        )?;
        serde_json::to_string(&serde_json::json!({"x": x, "y": y}))
            .map_err(|e| CoreError::invalid(e.to_string()))
    }

    pub fn apply_transaction(&self, transaction_json: &str) -> Result<String> {
        let transaction: Transaction = serde_json::from_str(transaction_json).map_err(|e| {
            CoreError::unsupported(format!("Invalid or unsupported transaction: {e}"))
        })?;
        let mut state = self.state()?;
        state.ensure_writable()?;
        // The in-memory base may have been checked out or changed by another session.
        if let Err(error) = storage::validate_current(&self.path, &state.manifest_bytes) {
            return Err(self.preserve_conflict_error(&state, error));
        }
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
        storage::control::begin_commit()?;
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
        if let Err(error) = storage::validate_current(&self.path, &state.manifest_bytes) {
            return Err(self.preserve_conflict_error(&state, error));
        }
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
        storage::control::begin_commit()?;
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
        if let Err(error) = storage::validate_current(&self.path, &state.manifest_bytes) {
            return Err(self.preserve_conflict_error(&state, error));
        }
        if state.manifest.name != name {
            storage::control::begin_commit()?;
            state.bump()?;
            state.manifest.name = name.to_owned();
            state.dirty = true;
        }
        Ok(())
    }

    /// Export a saved, conflict-free revision, preserving tracked files and
    /// conflict archives but excluding machine-local state and access grants.
    pub fn create_portable_snapshot(&self, destination: impl AsRef<Path>) -> Result<()> {
        let state = self.state()?;
        if state.dirty {
            return Err(CoreError::new(
                "UNSAVED_CHANGES",
                "Save the project before creating a snapshot",
            ));
        }
        let destination = destination.as_ref();
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if parent
            .canonicalize()?
            .starts_with(self.path.canonicalize()?)
        {
            return Err(CoreError::invalid(
                "A snapshot cannot be placed inside the source project",
            ));
        }
        let stage = StagedProject::new(destination)?;
        storage::write_portable_snapshot(&self.path, stage.path(), &state)?;
        state.ensure_writable()?;
        stage.publish(destination)
    }

    pub fn duplicate_to(
        &self,
        destination: impl AsRef<Path>,
        name: &str,
    ) -> Result<ProjectSession> {
        self.save()?;
        let state = self.state()?;
        state.ensure_writable()?;
        let destination = destination.as_ref();
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if parent
            .canonicalize()?
            .starts_with(self.path.canonicalize()?)
        {
            return Err(CoreError::invalid(
                "A duplicate cannot be placed inside the source project",
            ));
        }
        let snapshot = storage::snapshot_all_inputs(&self.path, &state.manifest_bytes)?;
        let inputs = snapshot.files;
        let blocked_generated = snapshot.blocked_generated;
        if inputs.keys().any(|path| path.starts_with("agent/")) {
            return Err(CoreError::unsupported(
                "Duplicating projects with agent records is not supported by this version",
            ));
        }
        let chats = storage::rewrite_chats(&inputs, state.manifest.project_id)?;
        let modes: Vec<String> = inputs
            .keys()
            .filter(|path| path.starts_with("src/") || path.starts_with("assets/"))
            .cloned()
            .collect();
        let source = state.model()?;
        let stage = StagedProject::new(destination)?;
        let duplicate = storage::control::staging(|| {
            let duplicate = create_in_place(stage.path(), name)?;
            let mut archives = BTreeMap::new();
            {
                let mut target = duplicate.state()?;
                target.bump()?;
                let checkpoints = duplicate_checkpoints(
                    &inputs,
                    state.manifest.project_id,
                    target.manifest.project_id,
                )?;
                let mut document = source;
                document.project_id = target.manifest.project_id;
                document.validate()?;
                target.document = serde_json::to_value(document)
                    .map_err(|e| CoreError::invalid(e.to_string()))?;
                let history = inputs
                    .get("history/state.json")
                    .map(|bytes| {
                        duplicate_history(
                            bytes,
                            state.manifest.project_id,
                            target.manifest.project_id,
                        )
                    })
                    .transpose()?;
                for (path, bytes) in inputs {
                    if path.starts_with("conflicts/") {
                        archives.insert(path, bytes);
                    } else if path == ".gitignore"
                        || path.starts_with("src/")
                        || path.starts_with("assets/")
                        || (path.starts_with("history/") && path != "history/state.json")
                    {
                        target.pending.insert(path, bytes);
                    }
                }
                if let Some(history) = history {
                    target
                        .pending
                        .insert("history/state.json".into(), storage::json(&history)?);
                    target.history = history;
                }
                for (path, bytes) in chats.into_files(target.manifest.project_id)? {
                    target.pending.insert(path, bytes);
                }
                target.pending.extend(checkpoints);
                target.dirty = true;
            }
            duplicate.save()?;
            storage::copy_executable_modes(&self.path, stage.path(), &modes)?;
            storage::write_duplicate_safety(stage.path(), &archives, &blocked_generated)?;
            Ok(duplicate)
        })?;
        drop(duplicate);
        stage.publish(destination)?;
        open_project(destination)
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
        let next_generation = state.next_generation()?;
        let committed = match storage::commit(&self.path, &state.manifest_bytes, &saved, &changes) {
            Ok(committed) => committed,
            Err(error) if error.code == "CANCELED" => return Err(error),
            Err(error) => {
                state.local_generation = next_generation;
                state.read_only_reason =
                    Some("Checkpoint publication failed; reopen the project to recover".into());
                return Err(error);
            }
        };
        self.after_commit(&state.manifest_bytes, &committed, state.manifest.project_id);
        state.local_generation = next_generation;
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
        storage::validate_current(&self.path, &state.manifest_bytes)?;
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
        let next_generation = state.next_generation()?;
        let committed = match storage::commit_with_deletions(
            &self.path,
            &state.manifest_bytes,
            &manifest,
            &changes,
            &deletions,
        ) {
            Ok(committed) => committed,
            Err(error) if error.code == "CANCELED" => return Err(error),
            Err(error) => {
                state.local_generation = next_generation;
                state.read_only_reason =
                    Some("Checkpoint restoration failed; reopen the project to recover".into());
                return Err(error);
            }
        };
        self.after_commit(&state.manifest_bytes, &committed, state.manifest.project_id);
        state.local_generation = next_generation;
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
            return storage::validate_current(&self.path, &state.manifest_bytes)
                .map_err(|error| self.preserve_conflict_error(&state, error));
        }
        let mut changes = state.pending.clone();
        changes.insert("document.json".into(), storage::json(&state.document)?);
        let mut manifest = state.manifest.clone();
        manifest.revision = state.model()?.revision;
        let next_generation = state.next_generation()?;
        let committed = storage::commit(&self.path, &state.manifest_bytes, &manifest, &changes)
            .map_err(|error| self.preserve_conflict_error(&state, error))?;
        self.after_commit(&state.manifest_bytes, &committed, state.manifest.project_id);
        state.local_generation = next_generation;
        state.manifest =
            serde_json::from_slice(&committed).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.manifest_bytes = committed;
        state.pending.clear();
        state.dirty = false;
        Ok(())
    }
}
