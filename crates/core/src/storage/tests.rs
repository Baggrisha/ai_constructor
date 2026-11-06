use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    AFTER_CONFLICT_SCAN, AFTER_SCAN, BEFORE_MANIFEST_REPLACE, BEFORE_REPLACE, INTERRUPT_AFTER,
    PARTIAL_CLEANUP, PARTIAL_JOURNAL_WRITE,
};
use crate::{create_project, open_project, ProjectSession, AFTER_CHECKPOINT_SAVE};

fn snapshot(session: &ProjectSession) -> Value {
    serde_json::from_str(&session.snapshot_json().unwrap()).unwrap()
}

fn change(session: &ProjectSession) {
    let state = snapshot(session);
    let root = &state["document"]["pages"][0]["rootId"];
    let transaction = json!({
        "transactionId": Uuid::new_v4(),
        "baseRevision": state["document"]["revision"],
        "baseFingerprint": state["fingerprint"],
        "origin": "user", "scopeNodeIds": [root],
        "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Changed"}]
    });
    session.apply_transaction(&transaction.to_string()).unwrap();
}

#[test]
fn interruption_at_each_publication_recovers_old_or_new_state() {
    // A save here replaces document.json and adds two history files, followed by project.json.
    for point in 0..=4 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let session = create_project(&path, "Example").unwrap();
        change(&session);
        INTERRUPT_AFTER.with(|value| value.set(Some(point)));
        assert!(
            session.save().is_err(),
            "interruption point {point} was not reached"
        );
        drop(session);
        let restored = open_project(&path).unwrap();
        let state = snapshot(&restored);
        assert_eq!(state["readOnly"], false, "point {point}");
        let name = if point == 4 { "Changed" } else { "Frame" };
        assert_eq!(state["document"]["nodes"][0]["name"], name, "point {point}");
    }
}

#[test]
fn unknown_file_version_during_recovery_is_preserved_and_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    INTERRUPT_AFTER.with(|value| value.set(Some(1)));
    assert!(session.save().is_err());
    drop(session);
    let doc = path.join("document.json");
    std::fs::write(&doc, b"external edit").unwrap();
    let restored = open_project(&path).unwrap();
    assert_eq!(snapshot(&restored)["readOnly"], true);
    assert_eq!(std::fs::read(doc).unwrap(), b"external edit");
    assert!(std::fs::read_dir(path.join("conflicts"))
        .unwrap()
        .next()
        .is_some());
    assert!(restored.save().is_err());
}

#[test]
fn unknown_commit_marker_preserves_both_versions_and_opens_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    INTERRUPT_AFTER.with(|value| value.set(Some(1)));
    assert!(session.save().is_err());
    drop(session);
    let marker = path.join("project.json");
    std::fs::write(&marker, b"unknown manifest").unwrap();
    let restored = open_project(&path).unwrap();
    assert_eq!(snapshot(&restored)["readOnly"], true);
    assert_eq!(std::fs::read(&marker).unwrap(), b"unknown manifest");
    let conflict = std::fs::read_dir(path.join("conflicts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(conflict.join("old-manifest").is_file());
    assert!(conflict.join("new-manifest").is_file());
}

#[test]
fn invalid_operation_rolls_back_all_operations() {
    let temp = tempfile::tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Example").unwrap();
    let before = snapshot(&session);
    let root = &before["document"]["pages"][0]["rootId"];
    let tx = json!({
        "transactionId": Uuid::new_v4(), "baseRevision": before["document"]["revision"],
        "baseFingerprint": before["fingerprint"], "origin": "user", "scopeNodeIds": [root],
        "operations": [
            {"type": "setProperty", "nodeId": root, "path": "/name", "value": "Changed"},
            {"type": "setViewport", "nodeId": root, "width": -1, "height": 900}
        ]
    });
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
}

#[test]
fn external_edit_after_scan_is_not_overwritten_or_committed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    let original_manifest = std::fs::read(path.join("project.json")).unwrap();
    let document = path.join("document.json");
    let external_document = document.clone();
    AFTER_SCAN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(external_document, b"external edit").unwrap();
        }));
    });

    assert!(session.save().is_err());
    assert_eq!(std::fs::read(document).unwrap(), b"external edit");
    assert_eq!(
        std::fs::read(path.join("project.json")).unwrap(),
        original_manifest
    );
}

#[test]
fn partial_journal_write_does_not_block_opening_previous_project() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    PARTIAL_JOURNAL_WRITE.with(|flag| flag.set(true));

    assert!(session.save().is_err());
    drop(session);
    let reopened = open_project(&path).unwrap();
    let state = snapshot(&reopened);
    assert_eq!(state["readOnly"], false);
    assert_eq!(state["document"]["nodes"][0]["name"], "Frame");
}

#[test]
fn partial_cleanup_after_commit_does_not_block_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    PARTIAL_CLEANUP.with(|flag| flag.set(true));

    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    let state = snapshot(&reopened);
    assert_eq!(state["readOnly"], false);
    assert_eq!(state["document"]["nodes"][0]["name"], "Changed");
}

#[test]
fn checkpoint_revision_matches_its_document_after_concurrent_edit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = std::sync::Arc::new(create_project(&path, "Example").unwrap());
    let worker = std::sync::Arc::clone(&session);
    AFTER_CHECKPOINT_SAVE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::thread::spawn(move || change(&worker)).join().unwrap();
        }));
    });

    let id = session.create_checkpoint().unwrap();
    let metadata: Value = serde_json::from_slice(
        &std::fs::read(path.join(format!("checkpoints/{id}.json"))).unwrap(),
    )
    .unwrap();
    let hash = metadata["documentHash"].as_str().unwrap();
    let document: Value = serde_json::from_slice(
        &std::fs::read(path.join(format!("checkpoints/blobs/{hash}"))).unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["revision"], document["revision"]);
    assert_eq!(document["nodes"][0]["name"], "Frame");
    assert_eq!(
        snapshot(&session)["document"]["nodes"][0]["name"],
        "Changed"
    );
}

fn retained_contains(path: &std::path::Path, expected: &[u8]) -> bool {
    walkdir::WalkDir::new(path.join(".aic-local/retained"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .any(|entry| std::fs::read(entry.path()).is_ok_and(|bytes| bytes == expected))
}

#[test]
fn race_between_check_and_replace_preserves_external_edit_on_save() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    let document = path.join("document.json");
    BEFORE_REPLACE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(document, b"external edit").unwrap();
        }));
    });

    assert!(session.save().is_err());
    assert!(retained_contains(&path, b"external edit"));
    drop(session);
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], true);
}

#[test]
fn race_between_check_and_replace_preserves_external_edit_on_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    INTERRUPT_AFTER.with(|value| value.set(Some(1)));
    assert!(session.save().is_err());
    drop(session);
    let document = path.join("document.json");
    BEFORE_REPLACE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(document, b"external edit").unwrap();
        }));
    });

    assert!(open_project(&path).is_err());
    assert!(retained_contains(&path, b"external edit"));
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], true);
}

#[test]
fn write_through_old_open_descriptors_survives_completed_save() {
    use std::io::Write;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    let mut old_document = std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("document.json"))
        .unwrap();
    let mut old_manifest = std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("project.json"))
        .unwrap();
    change(&session);
    session.save().unwrap();

    old_document.set_len(0).unwrap();
    old_document.write_all(b"late external document").unwrap();
    old_document.sync_all().unwrap();
    old_manifest.set_len(0).unwrap();
    old_manifest.write_all(b"late external manifest").unwrap();
    old_manifest.sync_all().unwrap();
    assert!(retained_contains(&path, b"late external document"));
    assert!(retained_contains(&path, b"late external manifest"));
    assert_eq!(
        snapshot(&open_project(&path).unwrap())["document"]["nodes"][0]["name"],
        "Changed"
    );
}

#[test]
fn interrupted_swap_keeps_unknown_external_version_and_opens_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    let document = path.join("document.json");
    BEFORE_REPLACE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(document, b"external edit").unwrap();
        }));
    });
    INTERRUPT_AFTER.with(|value| value.set(Some(1000)));
    assert!(session.save().is_err());
    drop(session);

    assert!(retained_contains(&path, b"external edit"));
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], true);
}

#[test]
fn new_destination_is_never_replaced_when_it_appears_concurrently() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let _session = create_project(&path, "Example").unwrap();
    let destination = path.join("src/new.txt");
    std::fs::write(&destination, b"external edit").unwrap();
    let retained = path.join(".aic-local/retained/no-replace/entry-0");
    std::fs::create_dir_all(retained.parent().unwrap()).unwrap();

    assert!(super::replace_with(
        &path,
        "src/new.txt",
        None,
        Some(b"editor version"),
        &retained
    )
    .is_err());
    assert_eq!(std::fs::read(destination).unwrap(), b"external edit");
}

#[test]
fn external_manifest_race_is_retained_and_project_reopens_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    let manifest = path.join("project.json");
    BEFORE_MANIFEST_REPLACE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(manifest, b"external manifest").unwrap();
        }));
    });

    assert!(session.save().is_err());
    assert!(retained_contains(&path, b"external manifest"));
    drop(session);
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], true);
}

#[test]
fn unsupported_swap_fails_without_overwriting_project() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    let document = std::fs::read(path.join("document.json")).unwrap();
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    change(&session);
    super::atomic::FAIL_SWAP.with(|flag| flag.set(true));

    assert!(session.save().is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), document);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    drop(session);
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], false);
}

#[test]
fn retained_journal_has_all_referenced_verified_copies_after_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    session.save().unwrap();

    let retained = std::fs::read_dir(path.join(".aic-local/retained"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let journal: Value =
        serde_json::from_slice(&std::fs::read(retained.join("journal.json")).unwrap()).unwrap();
    for (name, hash) in [
        ("old-manifest", "oldManifestHash"),
        ("new-manifest", "newManifestHash"),
    ] {
        let bytes = std::fs::read(retained.join(name)).unwrap();
        assert_eq!(super::hash(&bytes), journal[hash].as_str().unwrap());
    }
    for entry in journal["entries"].as_array().unwrap() {
        for (name, hash) in [("oldCopy", "oldHash"), ("newCopy", "newHash")] {
            if let (Some(name), Some(expected)) = (entry[name].as_str(), entry[hash].as_str()) {
                let bytes = std::fs::read(retained.join(name)).unwrap();
                assert_eq!(super::hash(&bytes), expected);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn killed_process_after_swap_preserves_external_inode() {
    const CHILD_PATH: &str = "AIC_TEST_KILL_AFTER_SWAP_PROJECT";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let path = std::path::PathBuf::from(path);
        let session = open_project(&path).unwrap();
        change(&session);
        let document = path.join("document.json");
        BEFORE_REPLACE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::write(document, b"external edit").unwrap();
            }));
        });
        super::KILL_AFTER.with(|point| point.set(Some(1000)));
        let _ = session.save();
        panic!("child did not reach the kill point");
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::tests::killed_process_after_swap_preserves_external_inode",
        ])
        .env(CHILD_PATH, &path)
        .status()
        .unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(child.signal(), Some(libc::SIGKILL));
    assert!(retained_contains(&path, b"external edit"));
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], true);
}

#[test]
fn conflict_metadata_hash_matches_the_copied_external_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    INTERRUPT_AFTER.with(|value| value.set(Some(1)));
    assert!(session.save().is_err());
    drop(session);
    let document = path.join("document.json");
    std::fs::write(&document, b"external A").unwrap();
    AFTER_CONFLICT_SCAN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(document, b"external B").unwrap();
        }));
    });

    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], true);
    let conflict = std::fs::read_dir(path.join("conflicts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let metadata: Value =
        serde_json::from_slice(&std::fs::read(conflict.join("metadata.json")).unwrap()).unwrap();
    let entry = metadata["externalFiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == "document.json")
        .unwrap();
    let bytes = std::fs::read(conflict.join(entry["copy"].as_str().unwrap())).unwrap();
    assert_eq!(bytes, b"external B");
    assert_eq!(entry["hash"].as_str().unwrap(), super::hash(&bytes));
}

#[test]
fn rollback_deletion_retains_the_actual_concurrently_changed_inode() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let _session = create_project(&path, "Example").unwrap();
    let destination = path.join("src/new.txt");
    std::fs::write(&destination, b"expected editor version").unwrap();
    let expected = super::hash(b"expected editor version");
    std::fs::write(&destination, b"external deletion race").unwrap();
    let retained = path.join(".aic-local/retained/delete-race/entry-0");
    std::fs::create_dir_all(retained.parent().unwrap()).unwrap();

    assert!(super::replace_with(&path, "src/new.txt", Some(&expected), None, &retained).is_err());
    assert!(!destination.exists());
    assert!(retained_contains(&path, b"external deletion race"));
}

#[test]
fn interrupted_external_asset_repair_restores_observed_or_new_version() {
    for point in 0..=6 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let old = temp.path().join("old.svg");
        let new = temp.path().join("new.svg");
        std::fs::write(
            &old,
            br#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#,
        )
        .unwrap();
        std::fs::write(
            &new,
            br#"<svg xmlns="http://www.w3.org/2000/svg"><circle/></svg>"#,
        )
        .unwrap();
        let session = create_project(&path, "Example").unwrap();
        let asset: Value = serde_json::from_str(&session.import_asset(&old).unwrap()).unwrap();
        let original_path = asset["path"].as_str().unwrap().to_string();
        std::fs::write(path.join(&original_path), b"external asset bytes").unwrap();
        INTERRUPT_AFTER.with(|value| value.set(Some(point)));
        assert!(
            session
                .relink_asset(asset["id"].as_str().unwrap(), &new)
                .is_err(),
            "point {point}"
        );
        drop(session);
        let restored = open_project(&path).unwrap();
        let state = snapshot(&restored);
        if point == 6 {
            assert_eq!(state["readOnly"], false, "point {point}");
            assert_ne!(state["document"]["assets"][0]["path"], original_path);
        } else {
            assert_eq!(state["readOnly"], true, "point {point}");
            assert_eq!(
                std::fs::read(path.join(&original_path)).unwrap(),
                b"external asset bytes"
            );
            assert_eq!(state["document"]["assets"][0]["path"], original_path);
        }
        let diagnostics: Value =
            serde_json::from_str(&restored.diagnostics_json().unwrap()).unwrap();
        let id = diagnostics["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|conflict| {
                conflict["reason"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("External asset missing")
            })
            .unwrap()["id"]
            .as_str()
            .unwrap();
        assert_eq!(
            std::fs::read(path.join("conflicts").join(id).join("external")).unwrap(),
            b"external asset bytes"
        );
    }
}

#[test]
fn cancellation_before_publication_preserves_saved_state() {
    use crate::cancellation::{CancelOutcome, OperationControl, OperationStatus};
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    let marker = std::fs::read(path.join("project.json")).unwrap();
    let document = std::fs::read(path.join("document.json")).unwrap();
    let control = Arc::new(OperationControl::new());
    let cancel = control.clone();
    AFTER_SCAN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert_eq!(cancel.request_cancel(), CancelOutcome::Accepted);
        }))
    });
    let error = session.save_controlled(control.clone()).unwrap_err();
    assert_eq!(error.code, "CANCELED");
    assert_eq!(control.status(), OperationStatus::Canceled);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), marker);
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), document);
}

#[test]
fn cancellation_after_publication_boundary_is_too_late() {
    use crate::cancellation::{CancelOutcome, OperationControl, OperationStatus};
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    let control = Arc::new(OperationControl::new());
    let cancel = control.clone();
    BEFORE_REPLACE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert_eq!(cancel.request_cancel(), CancelOutcome::TooLate);
        }))
    });
    session.save_controlled(control.clone()).unwrap();
    assert_eq!(control.status(), OperationStatus::Succeeded);
    drop(session);
    assert_eq!(
        snapshot(&open_project(path).unwrap())["document"]["nodes"][0]["name"],
        "Changed"
    );
}

#[test]
fn editing_operations_cancel_while_waiting_for_project_lock_without_mutation() {
    use crate::cancellation::{CancelOutcome, OperationControl, OperationStatus};
    use std::{
        sync::{mpsc, Arc},
        time::Duration,
    };

    for operation in ["apply", "undo", "redo", "rename"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let session = Arc::new(create_project(&path, "Example").unwrap());
        change(&session);
        session.save().unwrap();
        change(&session);
        session.save().unwrap();
        session.undo().unwrap();
        session.save().unwrap();
        let before = snapshot(&session);
        let marker = std::fs::read(path.join("project.json")).unwrap();
        let doc = std::fs::read(path.join("document.json")).unwrap();
        let tx = json!({
            "transactionId": uuid::Uuid::new_v4(),
            "baseRevision": before["document"]["revision"],
            "baseFingerprint": before["fingerprint"],
            "origin": "user",
            "scopeNodeIds": [before["document"]["pages"][0]["rootId"]],
            "operations": [{"type": "setProperty", "nodeId": before["document"]["pages"][0]["rootId"], "path": "/name", "value": "Canceled"}]
        }).to_string();
        let lock = super::lock(&path).unwrap();
        let control = Arc::new(OperationControl::new());
        let worker_control = control.clone();
        let worker_session = session.clone();
        let (started, running) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            let value = match operation {
                "apply" => {
                    worker_session.apply_transaction_and_save_controlled(&tx, worker_control)
                }
                "undo" => worker_session.undo_and_save_controlled(worker_control),
                "redo" => worker_session.redo_and_save_controlled(worker_control),
                "rename" => worker_session.rename_and_save_controlled("Canceled", worker_control),
                _ => unreachable!(),
            };
            finished.send(value).unwrap();
        });
        running.recv().unwrap();
        assert!(
            result.recv_timeout(Duration::from_millis(100)).is_err(),
            "{operation}"
        );
        assert_eq!(
            control.request_cancel(),
            CancelOutcome::Accepted,
            "{operation}"
        );
        let error = result
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "CANCELED", "{operation}");
        assert_eq!(control.status(), OperationStatus::Canceled, "{operation}");
        worker.join().unwrap();
        drop(lock);
        assert_eq!(snapshot(&session), before, "{operation}");
        assert_eq!(std::fs::read(path.join("project.json")).unwrap(), marker);
        assert_eq!(std::fs::read(path.join("document.json")).unwrap(), doc);
    }
}

#[test]
fn editing_reports_too_late_after_memory_publication_and_keeps_failed_save() {
    use crate::cancellation::{CancelOutcome, OperationControl, OperationStatus};
    use std::sync::Arc;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    let before = snapshot(&session);
    let root = &before["document"]["pages"][0]["rootId"];
    let tx = json!({
        "transactionId": Uuid::new_v4(),
        "baseRevision": before["document"]["revision"],
        "baseFingerprint": before["fingerprint"],
        "origin": "user", "scopeNodeIds": [root],
        "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Still in memory"}]
    });
    let control = Arc::new(OperationControl::new());
    let cancel = control.clone();
    AFTER_SCAN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert_eq!(cancel.request_cancel(), CancelOutcome::TooLate);
        }))
    });
    INTERRUPT_AFTER.with(|point| point.set(Some(0)));
    let error = session
        .apply_transaction_and_save_controlled(&tx.to_string(), control.clone())
        .unwrap_err();
    assert_eq!(error.code, "IO_ERROR");
    assert_eq!(control.status(), OperationStatus::Failed);
    let after = snapshot(&session);
    assert_eq!(after["dirty"], true);
    assert_eq!(after["document"]["nodes"][0]["name"], "Still in memory");
}

#[test]
fn controlled_undo_rejects_a_stale_disk_without_mutating_local_state() {
    use crate::cancellation::{OperationControl, OperationStatus};
    use std::sync::Arc;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    change(&session);
    session.save().unwrap();
    let before = snapshot(&session);
    std::fs::write(path.join(".gitignore"), b"external change\n").unwrap();
    let control = Arc::new(OperationControl::new());
    let error = session
        .undo_and_save_controlled(control.clone())
        .unwrap_err();
    assert_eq!(error.code, "REVISION_CONFLICT");
    assert_eq!(control.status(), OperationStatus::Failed);
    assert_eq!(snapshot(&session), before);
    assert_eq!(
        std::fs::read(path.join(".gitignore")).unwrap(),
        b"external change\n"
    );
}

#[test]
fn redo_and_rename_reject_a_stale_disk_without_mutating_local_state() {
    for operation in ["redo", "rename"] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let session = create_project(&path, "Example").unwrap();
        change(&session);
        session.save().unwrap();
        session.undo().unwrap();
        session.save().unwrap();
        let before = snapshot(&session);
        std::fs::write(path.join(".gitignore"), b"external change\n").unwrap();
        let error = match operation {
            "redo" => session.redo().map(|_| ()),
            "rename" => session.rename("Changed"),
            _ => unreachable!(),
        }
        .unwrap_err();
        assert_eq!(error.code, "REVISION_CONFLICT", "{operation}");
        assert_eq!(snapshot(&session), before, "{operation}");
        assert_eq!(
            std::fs::read(path.join(".gitignore")).unwrap(),
            b"external change\n"
        );
    }
}

#[test]
fn resolution_journal_recovers_missing_or_invalid_external_marker() {
    for missing in [false, true] {
        for point in 0..=2 {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("project");
            let session = create_project(&path, "Example").unwrap();
            let marker_path = path.join("project.json");
            if missing {
                std::fs::remove_file(&marker_path).unwrap();
            } else {
                std::fs::write(&marker_path, b"external invalid marker").unwrap();
            }
            session.diagnostics_json().unwrap();
            let (base, mut next, observed, marker, mut document) = {
                let state = session.state().unwrap();
                (
                    state.manifest_bytes.clone(),
                    state.manifest.clone(),
                    super::scan(&path).unwrap(),
                    super::file_bytes(&path, "project.json").unwrap(),
                    state.document.clone(),
                )
            };
            document["revision"] = json!(1);
            document["nodes"][0]["name"] = json!("Resolved");
            next.revision = 1;
            let changes = std::collections::BTreeMap::from([(
                "document.json".to_string(),
                super::json(&document).unwrap(),
            )]);
            INTERRUPT_AFTER.with(|value| value.set(Some(point)));
            assert!(
                super::commit_resolution(
                    &path,
                    &base,
                    &next,
                    &changes,
                    &std::collections::BTreeSet::new(),
                    &observed,
                    marker.as_deref()
                )
                .is_err(),
                "missing={missing}, point={point}"
            );
            drop(session);
            let reopened = open_project(&path).unwrap();
            let state = snapshot(&reopened);
            if point == 2 {
                assert_eq!(state["readOnly"], false);
                assert_eq!(state["document"]["nodes"][0]["name"], "Resolved");
            } else {
                assert_eq!(state["readOnly"], true);
                assert_eq!(state["document"]["nodes"][0]["name"], "Frame");
                if missing {
                    assert!(!marker_path.exists());
                } else {
                    assert_eq!(
                        std::fs::read(&marker_path).unwrap(),
                        b"external invalid marker"
                    );
                }
            }
        }
    }
}
