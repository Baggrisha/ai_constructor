use std::{
    cell::{Cell, RefCell},
    path::Path,
    sync::Arc,
};

use crate::{cancellation::OperationControl, CoreError, ProjectSession, Result};

thread_local! {
    static ACTIVE: RefCell<Option<Arc<OperationControl>>> = const { RefCell::new(None) };
    static STAGING: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn check() -> Result<()> {
    ACTIVE.with(|slot| match slot.borrow().as_ref() {
        Some(control) => control.check(),
        None => Ok(()),
    })
}

pub(crate) fn begin_commit() -> Result<()> {
    if STAGING.with(|depth| depth.get() > 0) {
        return check();
    }
    ACTIVE.with(|slot| match slot.borrow().as_ref() {
        Some(control) => control.begin_commit(),
        None => Ok(()),
    })
}

pub(crate) fn staging<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    struct ResetStage;
    impl Drop for ResetStage {
        fn drop(&mut self) {
            STAGING.with(|depth| depth.set(depth.get() - 1));
        }
    }
    check()?;
    STAGING.with(|depth| depth.set(depth.get() + 1));
    let _reset = ResetStage;
    operation()
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        ACTIVE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

fn run<T>(control: Arc<OperationControl>, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    ACTIVE.with(|slot| -> Result<()> {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(CoreError::invalid(
                "Nested controlled operations are not supported",
            ));
        }
        control.claim()?;
        *slot = Some(Arc::clone(&control));
        Ok(())
    })?;
    let _reset = Reset;
    let result = control.check().and_then(|()| operation());
    control.finish(result)
}

pub fn open_project_controlled(
    path: impl AsRef<Path>,
    control: Arc<OperationControl>,
) -> Result<ProjectSession> {
    run(control, || crate::open_project(path))
}

pub fn create_project_controlled(
    path: impl AsRef<Path>,
    name: &str,
    control: Arc<OperationControl>,
) -> Result<ProjectSession> {
    run(control, || crate::create_project(path, name))
}

impl ProjectSession {
    pub fn save_controlled(&self, control: Arc<OperationControl>) -> Result<()> {
        run(control, || self.save())
    }
    pub fn save_and_snapshot_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        run(control, || {
            self.save()?;
            self.snapshot_json()
        })
    }
    fn edit_and_save_controlled(
        &self,
        control: Arc<OperationControl>,
        edit: impl FnOnce() -> Result<()>,
    ) -> Result<String> {
        run(control, || {
            // Wait cancellably before the in-memory edit becomes irreversible.
            // Save checks for external changes again before journal publication.
            drop(super::lock(&self.path)?);
            edit()?;
            self.save()?;
            self.snapshot_json()
        })
    }
    pub fn apply_transaction_and_save_controlled(
        &self,
        json: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        self.edit_and_save_controlled(control, || self.apply_transaction(json).map(|_| ()))
    }
    pub fn undo_and_save_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        self.edit_and_save_controlled(control, || self.undo().map(|_| ()))
    }
    pub fn redo_and_save_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        self.edit_and_save_controlled(control, || self.redo().map(|_| ()))
    }
    pub fn rename_and_save_controlled(
        &self,
        name: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        self.edit_and_save_controlled(control, || self.rename(name))
    }
    pub fn create_portable_snapshot_controlled(
        &self,
        destination: impl AsRef<Path>,
        control: Arc<OperationControl>,
    ) -> Result<()> {
        run(control, || self.create_portable_snapshot(destination))
    }
    pub fn duplicate_to_controlled(
        &self,
        destination: impl AsRef<Path>,
        name: &str,
        control: Arc<OperationControl>,
    ) -> Result<ProjectSession> {
        run(control, || self.duplicate_to(destination, name))
    }
    pub fn create_checkpoint_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        run(control, || self.create_checkpoint())
    }
    pub fn restore_checkpoint_controlled(
        &self,
        id: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.restore_checkpoint(id))
    }
    pub fn import_asset_controlled(
        &self,
        path: impl AsRef<Path>,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.import_asset(path))
    }
    pub fn relink_asset_controlled(
        &self,
        id: &str,
        path: impl AsRef<Path>,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.relink_asset(id, path))
    }
    pub fn diagnostics_json_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        run(control, || self.diagnostics_json())
    }
    pub fn chats_json_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        run(control, || self.chats_json())
    }
    pub fn chat_messages_json_controlled(
        &self,
        id: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.chat_messages_json(id))
    }
    pub fn append_chat_message_controlled(
        &self,
        id: &str,
        json: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.append_chat_message(id, json))
    }
    pub fn resolve_external_conflict_controlled(
        &self,
        request_json: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.resolve_external_conflict(request_json))
    }
    pub fn resolve_recovery_conflict_controlled(
        &self,
        request_json: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.resolve_recovery_conflict(request_json))
    }
    pub fn search_chats_json_controlled(
        &self,
        query: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.search_chats_json(query))
    }
    pub fn fork_chat_controlled(
        &self,
        chat_id: &str,
        message_id: &str,
        title: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.fork_chat(chat_id, message_id, title))
    }
    pub fn delete_chat_controlled(
        &self,
        chat_id: &str,
        control: Arc<OperationControl>,
    ) -> Result<()> {
        run(control, || self.delete_chat(chat_id))
    }
    pub fn create_chat_controlled(
        &self,
        title: &str,
        mode: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.create_chat(title, mode))
    }
    pub fn rename_chat_controlled(
        &self,
        chat_id: &str,
        title: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.rename_chat(chat_id, title))
    }
    pub fn archive_chat_controlled(
        &self,
        chat_id: &str,
        archived: bool,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.archive_chat(chat_id, archived))
    }
    pub fn set_chat_mode_controlled(
        &self,
        chat_id: &str,
        mode: &str,
        control: Arc<OperationControl>,
    ) -> Result<String> {
        run(control, || self.set_chat_mode(chat_id, mode))
    }
    pub fn checkpoints_json_controlled(&self, control: Arc<OperationControl>) -> Result<String> {
        run(control, || self.checkpoints_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancellation::{CancelOutcome, OperationStatus};

    #[test]
    fn rejected_nested_operation_leaves_its_control_usable() {
        let outer = Arc::new(OperationControl::new());
        let inner = Arc::new(OperationControl::new());
        run(Arc::clone(&outer), || {
            let mut ran = false;
            let error = run(Arc::clone(&inner), || {
                ran = true;
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error.code, "INVALID_DOCUMENT");
            assert!(!ran);
            assert_eq!(inner.status(), OperationStatus::Preparing);
            Ok(())
        })
        .unwrap();
        assert_eq!(outer.status(), OperationStatus::Succeeded);
        assert_eq!(run(Arc::clone(&inner), || Ok(42)).unwrap(), 42);
        assert_eq!(inner.status(), OperationStatus::Succeeded);
    }

    #[test]
    fn rejected_nested_control_can_be_canceled_and_finishes_canceled() {
        let outer = Arc::new(OperationControl::new());
        let inner = Arc::new(OperationControl::new());
        run(outer, || {
            assert!(run(Arc::clone(&inner), || Ok(())).is_err());
            assert_eq!(inner.request_cancel(), CancelOutcome::Accepted);
            Ok(())
        })
        .unwrap();
        let error = run(Arc::clone(&inner), || Ok(())).unwrap_err();
        assert_eq!(error.code, "CANCELED");
        assert_eq!(inner.status(), OperationStatus::Canceled);
    }
}
