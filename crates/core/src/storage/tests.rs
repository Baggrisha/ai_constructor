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
