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

#[derive(uniffi::Object)]
pub struct ProjectSession {
    inner: aic_core::ProjectSession,
}

#[uniffi::export]
impl ProjectSession {
    pub fn snapshot_json(&self) -> Result<String, CoreError> {
        Ok(self.inner.snapshot_json()?)
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
