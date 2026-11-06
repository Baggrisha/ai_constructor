use std::sync::Arc;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("{code}: {message}")]
    Failure { code: String, message: String },
}

impl From<aic_core::CoreError> for CoreError {
    fn from(error: aic_core::CoreError) -> Self {
        Self::Failure {
            code: error.code.into(),
            message: error.message,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum CancelOutcome {
    Accepted,
    TooLate,
    Finished,
}

#[derive(uniffi::Enum)]
pub enum OperationStatus {
    Preparing,
    CancelRequested,
    Committing,
    Succeeded,
    Canceled,
    Failed,
}

#[derive(uniffi::Object)]
pub struct OperationControl {
    inner: Arc<aic_core::cancellation::OperationControl>,
}

#[uniffi::export]
pub fn new_operation_control() -> Arc<OperationControl> {
    Arc::new(OperationControl {
        inner: Arc::new(aic_core::cancellation::OperationControl::new()),
    })
}

#[uniffi::export]
impl OperationControl {
    pub fn request_cancel(&self) -> CancelOutcome {
        match self.inner.request_cancel() {
            aic_core::cancellation::CancelOutcome::Accepted => CancelOutcome::Accepted,
            aic_core::cancellation::CancelOutcome::TooLate => CancelOutcome::TooLate,
            aic_core::cancellation::CancelOutcome::Finished => CancelOutcome::Finished,
        }
    }
    pub fn status(&self) -> OperationStatus {
        match self.inner.status() {
            aic_core::cancellation::OperationStatus::Preparing => OperationStatus::Preparing,
            aic_core::cancellation::OperationStatus::CancelRequested => {
                OperationStatus::CancelRequested
            }
            aic_core::cancellation::OperationStatus::Committing => OperationStatus::Committing,
            aic_core::cancellation::OperationStatus::Succeeded => OperationStatus::Succeeded,
            aic_core::cancellation::OperationStatus::Canceled => OperationStatus::Canceled,
            aic_core::cancellation::OperationStatus::Failed => OperationStatus::Failed,
        }
    }
}

#[derive(uniffi::Object)]
pub struct ProjectSession {
    inner: aic_core::ProjectSession,
}

#[uniffi::export]
impl ProjectSession {
    /// Configure subsequent saves; this does not touch project files.
    pub fn set_retention_limit(&self, limit: Option<u32>) {
        self.inner.set_retention_limit(limit);
    }

    pub fn snapshot_json(&self) -> Result<String, CoreError> {
        Ok(self.inner.snapshot_json()?)
    }
    pub fn diagnostics_json(&self) -> Result<String, CoreError> {
        Ok(self.inner.diagnostics_json()?)
    }
    #[expect(clippy::too_many_arguments, reason = "Preserve the flat UniFFI API")]
    pub fn transform_point_json(
        &self,
        node_id: String,
        x: f64,
        y: f64,
        zoom: f64,
        pan_x: f64,
        pan_y: f64,
        inverse: bool,
        source_pixels: bool,
    ) -> Result<String, CoreError> {
        Ok(self.inner.transform_point_json(
            &node_id,
            x,
            y,
            zoom,
            pan_x,
            pan_y,
            inverse,
            source_pixels,
        )?)
    }
    #[expect(clippy::too_many_arguments, reason = "Preserve the flat UniFFI API")]
    pub fn transform_point_at_breakpoint_json(
        &self,
        node_id: String,
        x: f64,
        y: f64,
        zoom: f64,
        pan_x: f64,
        pan_y: f64,
        inverse: bool,
        source_pixels: bool,
        breakpoint: f64,
    ) -> Result<String, CoreError> {
        Ok(self.inner.transform_point_at_breakpoint_json(
            &node_id,
            x,
            y,
            zoom,
            pan_x,
            pan_y,
            inverse,
            source_pixels,
            breakpoint,
        )?)
    }
    pub fn chats_json(&self) -> Result<String, CoreError> {
        Ok(self.inner.chats_json()?)
    }
    pub fn create_chat(&self, title: String, mode: String) -> Result<String, CoreError> {
        Ok(self.inner.create_chat(&title, &mode)?)
    }
    pub fn chat_messages_json(&self, chat_id: String) -> Result<String, CoreError> {
        Ok(self.inner.chat_messages_json(&chat_id)?)
    }
    pub fn append_chat_message(
        &self,
        chat_id: String,
        message_json: String,
    ) -> Result<String, CoreError> {
        Ok(self.inner.append_chat_message(&chat_id, &message_json)?)
    }
    pub fn rename_chat(&self, chat_id: String, title: String) -> Result<String, CoreError> {
        Ok(self.inner.rename_chat(&chat_id, &title)?)
    }
    pub fn archive_chat(&self, chat_id: String, archived: bool) -> Result<String, CoreError> {
        Ok(self.inner.archive_chat(&chat_id, archived)?)
    }
    pub fn set_chat_mode(&self, chat_id: String, mode: String) -> Result<String, CoreError> {
        Ok(self.inner.set_chat_mode(&chat_id, &mode)?)
    }
    pub fn search_chats_json(&self, query: String) -> Result<String, CoreError> {
        Ok(self.inner.search_chats_json(&query)?)
    }
    pub fn fork_chat(
        &self,
        chat_id: String,
        message_id: String,
        title: String,
    ) -> Result<String, CoreError> {
        Ok(self.inner.fork_chat(&chat_id, &message_id, &title)?)
    }
    pub fn delete_chat(&self, chat_id: String) -> Result<(), CoreError> {
        Ok(self.inner.delete_chat(&chat_id)?)
    }
    pub fn import_asset(&self, source_path: String) -> Result<String, CoreError> {
        Ok(self.inner.import_asset(source_path)?)
    }
    pub fn relink_asset(&self, asset_id: String, source_path: String) -> Result<String, CoreError> {
        Ok(self.inner.relink_asset(&asset_id, source_path)?)
    }
    pub fn apply_transaction(&self, transaction_json: String) -> Result<String, CoreError> {
        Ok(self.inner.apply_transaction(&transaction_json)?)
    }
    pub fn undo(&self) -> Result<String, CoreError> {
        Ok(self.inner.undo()?)
    }
    pub fn redo(&self) -> Result<String, CoreError> {
        Ok(self.inner.redo()?)
    }
    pub fn rename(&self, name: String) -> Result<(), CoreError> {
        Ok(self.inner.rename(&name)?)
    }
    pub fn duplicate_to(
        &self,
        destination: String,
        name: String,
    ) -> Result<Arc<ProjectSession>, CoreError> {
        Ok(Arc::new(ProjectSession {
            inner: self.inner.duplicate_to(destination, &name)?,
        }))
    }
    pub fn create_checkpoint(&self) -> Result<String, CoreError> {
        Ok(self.inner.create_checkpoint()?)
    }
    pub fn checkpoints_json(&self) -> Result<String, CoreError> {
        Ok(self.inner.checkpoints_json()?)
    }
    pub fn restore_checkpoint(&self, id: String) -> Result<String, CoreError> {
        Ok(self.inner.restore_checkpoint(&id)?)
    }
    pub fn save(&self) -> Result<(), CoreError> {
        Ok(self.inner.save()?)
    }
    pub fn save_controlled(&self, control: Arc<OperationControl>) -> Result<(), CoreError> {
        Ok(self.inner.save_controlled(control.inner.clone())?)
    }
    pub fn save_and_snapshot_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .save_and_snapshot_controlled(control.inner.clone())?)
    }
    pub fn apply_transaction_and_save_controlled(
        &self,
        transaction_json: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .apply_transaction_and_save_controlled(&transaction_json, control.inner.clone())?)
    }
    pub fn undo_and_save_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self.inner.undo_and_save_controlled(control.inner.clone())?)
    }
    pub fn redo_and_save_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self.inner.redo_and_save_controlled(control.inner.clone())?)
    }
    pub fn rename_and_save_controlled(
        &self,
        name: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .rename_and_save_controlled(&name, control.inner.clone())?)
    }
    pub fn create_portable_snapshot_controlled(
        &self,
        destination: String,
        control: Arc<OperationControl>,
    ) -> Result<(), CoreError> {
        Ok(self
            .inner
            .create_portable_snapshot_controlled(destination, control.inner.clone())?)
    }
    pub fn duplicate_to_controlled(
        &self,
        destination: String,
        name: String,
        control: Arc<OperationControl>,
    ) -> Result<Arc<ProjectSession>, CoreError> {
        Ok(Arc::new(ProjectSession {
            inner: self
                .inner
                .duplicate_to_controlled(destination, &name, control.inner.clone())?,
        }))
    }
    pub fn create_checkpoint_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .create_checkpoint_controlled(control.inner.clone())?)
    }
    pub fn restore_checkpoint_controlled(
        &self,
        id: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .restore_checkpoint_controlled(&id, control.inner.clone())?)
    }
    pub fn import_asset_controlled(
        &self,
        path: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .import_asset_controlled(path, control.inner.clone())?)
    }
    pub fn relink_asset_controlled(
        &self,
        id: String,
        path: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .relink_asset_controlled(&id, path, control.inner.clone())?)
    }
    pub fn diagnostics_json_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .diagnostics_json_controlled(control.inner.clone())?)
    }
    pub fn chats_json_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self.inner.chats_json_controlled(control.inner.clone())?)
    }
    pub fn chat_messages_json_controlled(
        &self,
        id: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .chat_messages_json_controlled(&id, control.inner.clone())?)
    }
    pub fn append_chat_message_controlled(
        &self,
        id: String,
        json: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .append_chat_message_controlled(&id, &json, control.inner.clone())?)
    }
    pub fn resolve_external_conflict_controlled(
        &self,
        request_json: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .resolve_external_conflict_controlled(&request_json, control.inner.clone())?)
    }
    pub fn resolve_recovery_conflict_controlled(
        &self,
        request_json: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .resolve_recovery_conflict_controlled(&request_json, control.inner.clone())?)
    }
    pub fn checkpoints_json_controlled(
        &self,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .checkpoints_json_controlled(control.inner.clone())?)
    }
    pub fn search_chats_json_controlled(
        &self,
        query: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .search_chats_json_controlled(&query, control.inner.clone())?)
    }
    pub fn fork_chat_controlled(
        &self,
        chat_id: String,
        message_id: String,
        title: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .fork_chat_controlled(&chat_id, &message_id, &title, control.inner.clone())?)
    }
    pub fn delete_chat_controlled(
        &self,
        chat_id: String,
        control: Arc<OperationControl>,
    ) -> Result<(), CoreError> {
        Ok(self
            .inner
            .delete_chat_controlled(&chat_id, control.inner.clone())?)
    }
    pub fn create_chat_controlled(
        &self,
        title: String,
        mode: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .create_chat_controlled(&title, &mode, control.inner.clone())?)
    }
    pub fn rename_chat_controlled(
        &self,
        chat_id: String,
        title: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .rename_chat_controlled(&chat_id, &title, control.inner.clone())?)
    }
    pub fn archive_chat_controlled(
        &self,
        chat_id: String,
        archived: bool,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .archive_chat_controlled(&chat_id, archived, control.inner.clone())?)
    }
    pub fn set_chat_mode_controlled(
        &self,
        chat_id: String,
        mode: String,
        control: Arc<OperationControl>,
    ) -> Result<String, CoreError> {
        Ok(self
            .inner
            .set_chat_mode_controlled(&chat_id, &mode, control.inner.clone())?)
    }
}

#[uniffi::export]
pub fn open_project(path: String) -> Result<Arc<ProjectSession>, CoreError> {
    Ok(Arc::new(ProjectSession {
        inner: aic_core::open_project(path)?,
    }))
}

#[uniffi::export]
pub fn create_project(path: String, name: String) -> Result<Arc<ProjectSession>, CoreError> {
    Ok(Arc::new(ProjectSession {
        inner: aic_core::create_project(path, &name)?,
    }))
}

#[uniffi::export]
pub fn open_project_controlled(
    path: String,
    control: Arc<OperationControl>,
) -> Result<Arc<ProjectSession>, CoreError> {
    Ok(Arc::new(ProjectSession {
        inner: aic_core::open_project_controlled(path, control.inner.clone())?,
    }))
}

#[uniffi::export]
pub fn create_project_controlled(
    path: String,
    name: String,
    control: Arc<OperationControl>,
) -> Result<Arc<ProjectSession>, CoreError> {
    Ok(Arc::new(ProjectSession {
        inner: aic_core::create_project_controlled(path, &name, control.inner.clone())?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_limit_code_survives_the_bridge() {
        let dir = std::env::temp_dir().join(format!(
            "aic-bridge-limit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let session = create_project(
            dir.join("project").to_string_lossy().into_owned(),
            "Assets".into(),
        )
        .unwrap();
        let before = session.snapshot_json().unwrap();
        let source = dir.join("oversized");
        std::fs::File::create(&source)
            .unwrap()
            .set_len(50 * 1024 * 1024 + 1)
            .unwrap();
        let error = session
            .import_asset_controlled(
                source.to_string_lossy().into_owned(),
                new_operation_control(),
            )
            .unwrap_err();
        let CoreError::Failure { code, message } = error;
        assert_eq!(code, "IMPORT_LIMIT_EXCEEDED");
        assert!(message.contains("50 MiB"));
        assert_eq!(session.snapshot_json().unwrap(), before);
    }
}
