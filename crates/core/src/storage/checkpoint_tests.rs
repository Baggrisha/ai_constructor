use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde_json::{json, Value};

use super::{
    commit_with_deletions, hash, json as json_bytes, load, BEFORE_REPLACE, INTERRUPT_AFTER,
};
use crate::{create_project, open_project, ProjectSession};

fn persist_files(path: &Path, files: &[(&str, Option<&[u8]>)]) {
    let loaded = load(path).unwrap();
    let mut writes = BTreeMap::new();
    let mut deletes = BTreeSet::new();
    for (name, bytes) in files {
        match bytes {
            Some(bytes) => {
                writes.insert((*name).to_string(), bytes.to_vec());
            }
            None => {
                deletes.insert((*name).to_string());
            }
        }
    }
    commit_with_deletions(
        path,
        &loaded.manifest_bytes,
        &loaded.manifest,
        &writes,
        &deletes,
    )
    .unwrap();
}

fn snapshot(session: &ProjectSession) -> Value {
    serde_json::from_str(&session.snapshot_json().unwrap()).unwrap()
}

fn rename(session: &ProjectSession, name: &str) {
    let current = snapshot(session);
    let root = &current["document"]["pages"][0]["rootId"];
    let transaction = json!({
        "transactionId": uuid::Uuid::new_v4(), "baseRevision": current["document"]["revision"],
        "baseFingerprint": current["fingerprint"], "origin": "user", "scopeNodeIds": [root],
        "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": name}]
    });
    session.apply_transaction(&transaction.to_string()).unwrap();
}

fn asset_path(bytes: &[u8]) -> String {
    format!("assets/{}.bin", hash(bytes))
}

fn chat() -> String {
    format!("{{\"attachment\":\"{}\"}}\n", asset_path(b"later asset"))
}

fn setup(path: &Path) -> String {
    drop(create_project(path, "Example").unwrap());
    let checkpoint_asset = asset_path(b"checkpoint asset");
    let later_asset = asset_path(b"later asset");
    let shared_asset = asset_path(b"shared asset");
    persist_files(
        path,
        &[
            ("src/user/main.js", Some(b"checkpoint main")),
            ("src/user/restored.js", Some(b"checkpoint restored")),
            ("src/user/nested/deep/file.js", Some(b"nested checkpoint")),
            ("src/generated/view.js", Some(b"checkpoint generated")),
            (checkpoint_asset.as_str(), Some(b"checkpoint asset")),
            (shared_asset.as_str(), Some(b"shared asset")),
            ("chats/chat/messages.jsonl", Some(b"old chat\n")),
        ],
    );
    let session = open_project(path).unwrap();
    let checkpoint = session.create_checkpoint().unwrap();
    drop(session);
    persist_files(
        path,
        &[
            ("src/user/main.js", Some(b"later main")),
            ("src/user/restored.js", None),
            ("src/user/nested/deep/file.js", None),
            ("src/user/new.js", Some(b"later new")),
            ("src/generated/view.js", Some(b"later generated")),
            (checkpoint_asset.as_str(), None),
            (later_asset.as_str(), Some(b"later asset")),
            ("chats/chat/messages.jsonl", Some(chat().as_bytes())),
        ],
    );
    std::fs::remove_dir(path.join("src/user/nested/deep")).unwrap();
    std::fs::remove_dir(path.join("src/user/nested")).unwrap();
    checkpoint
}

fn assert_later_files(path: &Path) {
    assert_eq!(
        std::fs::read(path.join("src/user/main.js")).unwrap(),
        b"later main"
    );
    assert!(!path.join("src/user/restored.js").exists());
    assert!(!path.join("src/user/nested/deep/file.js").exists());
    assert_eq!(
        std::fs::read(path.join("src/user/new.js")).unwrap(),
        b"later new"
    );
    assert_eq!(
        std::fs::read(path.join("src/generated/view.js")).unwrap(),
        b"later generated"
    );
    assert!(!path.join(asset_path(b"checkpoint asset")).exists());
    assert_eq!(
        std::fs::read(path.join(asset_path(b"later asset"))).unwrap(),
        b"later asset"
    );
    assert_eq!(
        std::fs::read(path.join(asset_path(b"shared asset"))).unwrap(),
        b"shared asset"
    );
    assert_eq!(
        std::fs::read(path.join("chats/chat/messages.jsonl")).unwrap(),
        chat().as_bytes()
    );
}

fn assert_checkpoint_files(path: &Path) {
    assert_eq!(
        std::fs::read(path.join("src/user/main.js")).unwrap(),
        b"checkpoint main"
    );
    assert_eq!(
        std::fs::read(path.join("src/user/restored.js")).unwrap(),
        b"checkpoint restored"
    );
    assert_eq!(
        std::fs::read(path.join("src/user/nested/deep/file.js")).unwrap(),
        b"nested checkpoint"
    );
    assert!(!path.join("src/user/new.js").exists());
    assert_eq!(
        std::fs::read(path.join("src/generated/view.js")).unwrap(),
        b"checkpoint generated"
    );
    assert_eq!(
        std::fs::read(path.join(asset_path(b"checkpoint asset"))).unwrap(),
        b"checkpoint asset"
    );
    assert_eq!(
        std::fs::read(path.join(asset_path(b"later asset"))).unwrap(),
        b"later asset"
    );
    assert_eq!(
        std::fs::read(path.join(asset_path(b"shared asset"))).unwrap(),
        b"shared asset"
    );
    assert_eq!(
        std::fs::read(path.join("chats/chat/messages.jsonl")).unwrap(),
        chat().as_bytes()
    );
}

#[test]
fn restore_replaces_adds_and_deletes_files_in_one_new_revision() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    let shared_asset = path.join(asset_path(b"shared asset"));
    let modified = std::fs::metadata(&shared_asset)
        .unwrap()
        .modified()
        .unwrap();
    let result: Value =
        serde_json::from_str(&session.restore_checkpoint(&checkpoint).unwrap()).unwrap();
    assert_eq!(
        result["document"]["revision"].as_u64().unwrap(),
        before["document"]["revision"].as_u64().unwrap() + 1
    );
    assert_checkpoint_files(&path);
    assert_eq!(
        std::fs::metadata(shared_asset).unwrap().modified().unwrap(),
        modified
    );
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], false);
    assert_eq!(
        snapshot(&reopened)["document"]["revision"],
        result["document"]["revision"]
    );
    reopened.undo().unwrap();
    assert_later_files(&path);
    drop(reopened);
    let reopened = open_project(&path).unwrap();
    reopened.redo().unwrap();
    assert_checkpoint_files(&path);
    rename(&reopened, "Later edit");
    reopened.undo().unwrap();
    reopened.undo().unwrap();
    assert_later_files(&path);
    reopened.redo().unwrap();
    reopened.redo().unwrap();
    assert_eq!(
        snapshot(&reopened)["document"]["nodes"][0]["name"],
        "Later edit"
    );
    assert_checkpoint_files(&path);
}

#[test]
fn missing_or_corrupt_checkpoint_blob_blocks_every_change() {
    for corrupt in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let checkpoint = setup(&path);
        let blob = format!("checkpoints/blobs/{}", hash(b"checkpoint main"));
        persist_files(
            &path,
            &[(
                blob.as_str(),
                if corrupt {
                    Some(b"corrupt".as_slice())
                } else {
                    None
                },
            )],
        );
        let session = open_project(&path).unwrap();
        let before = snapshot(&session);
        let manifest = std::fs::read(path.join("project.json")).unwrap();
        let error = session.restore_checkpoint(&checkpoint).unwrap_err();
        assert_eq!(error.code, "REVISION_CONFLICT");
        assert_eq!(snapshot(&session), before);
        assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
        assert_later_files(&path);
    }
}

#[test]
fn checkpoint_cannot_target_history_chat_or_unsafe_paths() {
    for invalid_path in [
        "chats/chat/messages.jsonl",
        "src/user/../../chats/chat/messages.jsonl",
        "src/user//main.js",
        "src/user/main.js/child",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let checkpoint = setup(&path);
        let meta_path = format!("checkpoints/{checkpoint}.json");
        let mut meta: Value =
            serde_json::from_slice(&std::fs::read(path.join(&meta_path)).unwrap()).unwrap();
        meta["dependencies"][invalid_path] = json!(hash(b"checkpoint main"));
        meta["executableModes"][invalid_path] = json!(0);
        persist_files(
            &path,
            &[(meta_path.as_str(), Some(&json_bytes(&meta).unwrap()))],
        );
        let session = open_project(&path).unwrap();
        let before = snapshot(&session);
        assert!(session.restore_checkpoint(&checkpoint).is_err());
        assert_eq!(snapshot(&session), before);
        assert_later_files(&path);
    }
}

#[test]
fn external_edit_before_restore_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let session = open_project(&path).unwrap();
    let document = path.join("src/user/main.js");
    std::fs::write(&document, b"external edit").unwrap();
    let before = snapshot(&session);
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "REVISION_CONFLICT"
    );
    assert_eq!(std::fs::read(document).unwrap(), b"external edit");
    assert_eq!(snapshot(&session), before);
}

#[test]
fn external_edit_during_restore_is_not_lost() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let session = open_project(&path).unwrap();
    let document = path.join("src/user/main.js");
    let changed = document.clone();
    BEFORE_REPLACE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(changed, b"external during restore").unwrap()
        }));
    });
    assert!(session.restore_checkpoint(&checkpoint).is_err());
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], true);
    assert_eq!(std::fs::read(document).unwrap(), b"external during restore");
}

#[test]
fn every_interrupted_restore_recovers_all_old_or_all_new_files() {
    let mut interrupted = 0;
    let mut recovered_new = false;
    for point in 0..64 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let checkpoint = setup(&path);
        let session = open_project(&path).unwrap();
        let before_revision = snapshot(&session)["document"]["revision"].as_u64().unwrap();
        INTERRUPT_AFTER.with(|value| value.set(Some(point)));
        let result = session.restore_checkpoint(&checkpoint);
        INTERRUPT_AFTER.with(|value| value.set(None));
        drop(session);
        if result.is_ok() {
            break;
        }
        interrupted += 1;
        let recovered = open_project(&path).unwrap();
        let state = snapshot(&recovered);
        assert_eq!(state["readOnly"], false, "point {point}");
        match state["document"]["revision"].as_u64().unwrap() - before_revision {
            0 => assert_later_files(&path),
            1 => {
                recovered_new = true;
                assert_checkpoint_files(&path);
            }
            delta => panic!("unexpected revision delta {delta} at point {point}"),
        }
    }
    assert!(interrupted > 4);
    assert!(recovered_new, "the committed marker must also be tested");
}

#[test]
fn asset_at_same_address_with_different_contents_blocks_restore() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let asset = asset_path(b"checkpoint asset");
    persist_files(&path, &[(asset.as_str(), Some(b"other contents"))]);
    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "REVISION_CONFLICT"
    );
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_eq!(std::fs::read(path.join(asset)).unwrap(), b"other contents");
    assert_eq!(
        std::fs::read(path.join("src/user/main.js")).unwrap(),
        b"later main"
    );
}

#[test]
fn checkpoint_reuses_existing_content_addressed_blobs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let _checkpoint = setup(&path);
    let session = open_project(&path).unwrap();
    session.create_checkpoint().unwrap();
    let count = std::fs::read_dir(path.join("checkpoints/blobs"))
        .unwrap()
        .count();
    let asset = path.join(asset_path(b"later asset"));
    let modified = std::fs::metadata(&asset).unwrap().modified().unwrap();
    session.create_checkpoint().unwrap();
    assert_eq!(
        std::fs::read_dir(path.join("checkpoints/blobs"))
            .unwrap()
            .count(),
        count
    );
    assert_eq!(
        std::fs::metadata(&asset).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn checkpoint_missing_a_document_reference_is_rejected_without_writes() {
    for missing_asset in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        drop(create_project(&path, "Example").unwrap());
        let asset = asset_path(b"referenced asset");
        let module = "src/user/component.ts";
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
        document["assets"] = json!([{"id": uuid::Uuid::new_v4(), "path": asset.as_str()}]);
        document["codeModules"] =
            json!([{"id": uuid::Uuid::new_v4(), "path": module, "props": {}, "events": []}]);
        let document_bytes = json_bytes(&document).unwrap();
        persist_files(
            &path,
            &[
                ("document.json", Some(&document_bytes)),
                (asset.as_str(), Some(b"referenced asset")),
                (module, Some(b"export const value = 1")),
            ],
        );
        let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
        let meta_path = format!("checkpoints/{checkpoint}.json");
        let mut meta: Value =
            serde_json::from_slice(&std::fs::read(path.join(&meta_path)).unwrap()).unwrap();
        meta["dependencies"]
            .as_object_mut()
            .unwrap()
            .remove(if missing_asset {
                asset.as_str()
            } else {
                module
            });
        let metadata = json_bytes(&meta).unwrap();
        persist_files(&path, &[(meta_path.as_str(), Some(&metadata))]);
        let session = open_project(&path).unwrap();
        let before = snapshot(&session);
        let manifest = std::fs::read(path.join("project.json")).unwrap();
        let error = session.restore_checkpoint(&checkpoint).unwrap_err();
        if missing_asset {
            assert_eq!(error.code, "MISSING_ASSET");
        } else {
            assert_eq!(error.code, "INVALID_DOCUMENT");
        }
        assert_eq!(snapshot(&session), before);
        assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
        assert_eq!(
            std::fs::read(path.join(asset)).unwrap(),
            b"referenced asset"
        );
        assert_eq!(
            std::fs::read(path.join(module)).unwrap(),
            b"export const value = 1"
        );
    }
}

#[test]
fn failed_restore_does_not_save_prior_unsaved_edits() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let blob = format!("checkpoints/blobs/{}", hash(b"checkpoint main"));
    persist_files(&path, &[(blob.as_str(), None)]);
    let session = open_project(&path).unwrap();
    rename(&session, "Unsaved edit");
    let before = snapshot(&session);
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    assert!(session.restore_checkpoint(&checkpoint).is_err());
    assert_eq!(snapshot(&session), before);
    assert_eq!(snapshot(&session)["dirty"], true);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_later_files(&path);
}

#[test]
fn failed_publication_locks_session_until_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let session = open_project(&path).unwrap();
    INTERRUPT_AFTER.with(|point| point.set(Some(1)));
    assert!(session.restore_checkpoint(&checkpoint).is_err());
    assert_eq!(snapshot(&session)["readOnly"], true);
    assert!(session.save().is_err());
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], false);
    assert_later_files(&path);
}

#[test]
fn file_directory_collision_is_rejected_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    persist_files(&path, &[("src/user/swap", Some(b"checkpoint file"))]);
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    persist_files(&path, &[("src/user/swap", None)]);
    persist_files(&path, &[("src/user/swap/child", Some(b"later child"))]);
    let session = open_project(&path).unwrap();
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    let before = snapshot(&session);
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "UNSUPPORTED_FEATURE"
    );
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_eq!(
        std::fs::read(path.join("src/user/swap/child")).unwrap(),
        b"later child"
    );
}

#[cfg(unix)]
#[test]
fn checkpoint_restore_undo_and_redo_preserve_known_executable_modes() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let source = path.join("src/user/main.js");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let session = open_project(&path).unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), b"checkpoint main");
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o111,
        0
    );
    session.undo().unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), b"later main");
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o111,
        0o111
    );
    drop(session);
    let session = open_project(&path).unwrap();
    session.redo().unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), b"checkpoint main");
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[cfg(unix)]
#[test]
fn removed_executable_is_restored_with_its_checkpoint_mode() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let source = "src/user/build.sh";
    persist_files(&path, &[(source, Some(b"#!/bin/sh\necho hello\n"))]);
    std::fs::set_permissions(path.join(source), std::fs::Permissions::from_mode(0o755)).unwrap();
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    let meta: Value = serde_json::from_slice(
        &std::fs::read(path.join(format!("checkpoints/{checkpoint}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["executableModes"][source].as_u64(), Some(0o111));
    persist_files(
        &path,
        &[(source, None), ("src/user/new.ts", Some(b"later"))],
    );
    let session = open_project(&path).unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(
        std::fs::metadata(path.join(source))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert!(!path.join("src/user/new.ts").exists());
    session.undo().unwrap();
    assert!(!path.join(source).exists());
    assert_eq!(
        std::fs::read(path.join("src/user/new.ts")).unwrap(),
        b"later"
    );
    session.redo().unwrap();
    assert_eq!(
        std::fs::metadata(path.join(source))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
}

#[cfg(unix)]
#[test]
fn mode_only_restore_is_undoable_and_survives_reopen() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let source = path.join("src/user/build.sh");
    persist_files(&path, &[("src/user/build.sh", Some(b"same bytes"))]);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
    let session = open_project(&path).unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o751
    );
    session.undo().unwrap();
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o640
    );
    drop(session);
    let session = open_project(&path).unwrap();
    session.redo().unwrap();
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
        0o751
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"same bytes");
}

#[cfg(unix)]
#[test]
fn interrupted_checkpoint_mode_restore_recovers_whole_file_and_mode() {
    use std::os::unix::fs::PermissionsExt;
    let mut recovered_after = false;
    for point in 0..32 {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        drop(create_project(&path, "Example").unwrap());
        let relative = "src/user/build.sh";
        let source = path.join(relative);
        persist_files(&path, &[(relative, Some(b"checkpoint"))]);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
        persist_files(&path, &[(relative, Some(b"later"))]);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
        let session = open_project(&path).unwrap();
        INTERRUPT_AFTER.with(|value| value.set(Some(point)));
        let result = session.restore_checkpoint(&checkpoint);
        INTERRUPT_AFTER.with(|value| value.set(None));
        drop(session);
        if result.is_ok() {
            break;
        }
        let reopened = open_project(&path).unwrap();
        assert_eq!(snapshot(&reopened)["readOnly"], false, "point {point}");
        let bytes = std::fs::read(&source).unwrap();
        let mode = std::fs::metadata(&source).unwrap().permissions().mode() & 0o777;
        match bytes.as_slice() {
            b"checkpoint" => {
                assert_eq!(mode, 0o751, "point {point}");
                recovered_after = true;
            }
            b"later" => assert_eq!(mode, 0o640, "point {point}"),
            _ => panic!("unexpected file at point {point}"),
        }
    }
    assert!(recovered_after);
}

#[test]
fn legacy_checkpoint_without_mode_metadata_refuses_to_recreate_missing_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let metadata_path = format!("checkpoints/{checkpoint}.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(path.join(&metadata_path)).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("executableModes");
    let bytes = json_bytes(&metadata).unwrap();
    persist_files(&path, &[(metadata_path.as_str(), Some(&bytes))]);
    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "CHECKPOINT_MODE_CONFIRMATION_REQUIRED"
    );
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_later_files(&path);
    session
        .restore_checkpoint_with_missing_modes(&checkpoint)
        .unwrap();
    assert_checkpoint_files(&path);
    session.undo().unwrap();
    assert_later_files(&path);
}

#[test]
fn legacy_checkpoint_confirmation_is_bound_to_the_session_generation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let metadata_path = format!("checkpoints/{checkpoint}.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(path.join(&metadata_path)).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("executableModes");
    let bytes = json_bytes(&metadata).unwrap();
    persist_files(&path, &[(metadata_path.as_str(), Some(&bytes))]);
    let session = open_project(&path).unwrap();
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "CHECKPOINT_MODE_CONFIRMATION_REQUIRED"
    );
    rename(&session, "Changed after question");
    let before = snapshot(&session);
    assert_eq!(
        session
            .restore_checkpoint_with_missing_modes(&checkpoint)
            .unwrap_err()
            .code,
        "REVISION_CONFLICT"
    );
    assert_eq!(snapshot(&session), before);
    assert_later_files(&path);
}

#[test]
fn legacy_document_only_checkpoint_remains_restorable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    let checkpoint = session.create_checkpoint().unwrap();
    drop(session);
    let metadata_path = format!("checkpoints/{checkpoint}.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(path.join(&metadata_path)).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("executableModes");
    let bytes = json_bytes(&metadata).unwrap();
    persist_files(&path, &[(metadata_path.as_str(), Some(&bytes))]);
    let session = open_project(&path).unwrap();
    rename(&session, "Changed");
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(snapshot(&session)["document"]["nodes"][0]["name"], "Frame");
}

fn case_insensitive(directory: &Path) -> bool {
    let probe = directory.join("CaseProbe");
    std::fs::create_dir(&probe).unwrap();
    let insensitive = directory.join("caseprobe").exists();
    std::fs::remove_dir(probe).unwrap();
    insensitive
}

#[test]
fn differently_cased_existing_parent_is_rejected_before_any_publication() {
    let temp = tempfile::tempdir().unwrap();
    if !case_insensitive(temp.path()) {
        return;
    }
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    persist_files(&path, &[("src/user/Views/old.ts", Some(b"checkpoint"))]);
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    persist_files(&path, &[("src/user/Views/old.ts", None)]);
    std::fs::remove_dir(path.join("src/user/Views")).unwrap();
    persist_files(&path, &[("src/user/views/new.ts", Some(b"later"))]);
    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "UNSUPPORTED_FEATURE"
    );
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_eq!(
        std::fs::read(path.join("src/user/views/new.ts")).unwrap(),
        b"later"
    );
    assert!(!path.join("src/user/views/old.ts").exists());
    drop(session);
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], false);
}

#[test]
fn differently_cased_missing_checkpoint_directories_are_rejected_preflight() {
    let temp = tempfile::tempdir().unwrap();
    if !case_insensitive(temp.path()) {
        return;
    }
    let path = temp.path().join("project");
    let checkpoint = setup(&path);
    let metadata_path = format!("checkpoints/{checkpoint}.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(path.join(&metadata_path)).unwrap()).unwrap();
    metadata["dependencies"]["src/user/Views/one.ts"] = json!(hash(b"checkpoint main"));
    metadata["dependencies"]["src/user/views/two.ts"] = json!(hash(b"checkpoint main"));
    metadata["executableModes"]["src/user/Views/one.ts"] = json!(0);
    metadata["executableModes"]["src/user/views/two.ts"] = json!(0);
    let bytes = json_bytes(&metadata).unwrap();
    persist_files(&path, &[(metadata_path.as_str(), Some(&bytes))]);
    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    assert_eq!(
        session.restore_checkpoint(&checkpoint).unwrap_err().code,
        "UNSUPPORTED_FEATURE"
    );
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_later_files(&path);
}

#[cfg(unix)]
#[test]
fn changed_executable_flag_is_not_silently_ignored_when_content_matches() {
    use std::os::unix::fs::PermissionsExt;
    for (initial_mode, changed_mode) in [(0o644, 0o755), (0o755, 0o644), (0o744, 0o755)] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        drop(create_project(&path, "Example").unwrap());
        let source = path.join("src/user/build.sh");
        persist_files(&path, &[("src/user/build.sh", Some(b"#!/bin/sh\n"))]);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(initial_mode)).unwrap();
        let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(changed_mode)).unwrap();
        let session = open_project(&path).unwrap();
        session.restore_checkpoint(&checkpoint).unwrap();
        assert_eq!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            (changed_mode & !0o111) | (initial_mode & 0o111)
        );
        session.undo().unwrap();
        assert_eq!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            changed_mode
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_casefold_directory_blocks_restore_when_supported_by_filesystem() {
    use std::os::fd::AsRawFd;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let directory = path.join("src/user/folded");
    std::fs::create_dir(&directory).unwrap();
    let handle = std::fs::File::open(&directory).unwrap();
    let mut flags: libc::c_int = 0;
    if unsafe { libc::ioctl(handle.as_raw_fd(), libc::FS_IOC_GETFLAGS as _, &mut flags) } < 0 {
        return;
    }
    flags |= 0x4000_0000;
    if unsafe { libc::ioctl(handle.as_raw_fd(), libc::FS_IOC_SETFLAGS as _, &flags) } < 0 {
        return;
    }
    persist_files(&path, &[("src/user/folded/view.ts", Some(b"checkpoint"))]);
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    persist_files(&path, &[("src/user/folded/view.ts", Some(b"later"))]);
    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    let manifest = std::fs::read(path.join("project.json")).unwrap();
    let error = session.restore_checkpoint(&checkpoint).unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_FEATURE");
    assert!(error.message.contains("Casefolded directory"));
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("project.json")).unwrap(), manifest);
    assert_eq!(
        std::fs::read(path.join("src/user/folded/view.ts")).unwrap(),
        b"later"
    );
}

#[test]
fn case_distinct_checkpoint_directories_restore_on_case_sensitive_filesystems() {
    let temp = tempfile::tempdir().unwrap();
    if case_insensitive(temp.path()) {
        return;
    }
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    #[cfg(target_os = "linux")]
    if super::require_non_casefold_directory(&path).is_err() {
        return;
    }
    persist_files(
        &path,
        &[
            ("src/user/Views/one.ts", Some(b"one")),
            ("src/user/views/two.ts", Some(b"two")),
        ],
    );
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    persist_files(
        &path,
        &[
            ("src/user/Views/one.ts", None),
            ("src/user/views/two.ts", None),
        ],
    );
    std::fs::remove_dir(path.join("src/user/Views")).unwrap();
    std::fs::remove_dir(path.join("src/user/views")).unwrap();
    let session = open_project(&path).unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(
        std::fs::read(path.join("src/user/Views/one.ts")).unwrap(),
        b"one"
    );
    assert_eq!(
        std::fs::read(path.join("src/user/views/two.ts")).unwrap(),
        b"two"
    );
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], false);
}

#[cfg(unix)]
#[test]
fn legacy_checkpoint_preserves_the_current_mode_of_existing_files() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let source = "src/user/build.sh";
    persist_files(&path, &[(source, Some(b"checkpoint"))]);
    std::fs::set_permissions(path.join(source), std::fs::Permissions::from_mode(0o755)).unwrap();
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    let metadata_path = format!("checkpoints/{checkpoint}.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(path.join(&metadata_path)).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("executableModes");
    let bytes = json_bytes(&metadata).unwrap();
    persist_files(
        &path,
        &[
            (metadata_path.as_str(), Some(&bytes)),
            (source, Some(b"later")),
        ],
    );
    std::fs::set_permissions(path.join(source), std::fs::Permissions::from_mode(0o751)).unwrap();
    let session = open_project(&path).unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(std::fs::read(path.join(source)).unwrap(), b"checkpoint");
    assert_eq!(
        std::fs::metadata(path.join(source))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    session.undo().unwrap();
    assert_eq!(std::fs::read(path.join(source)).unwrap(), b"later");
    assert_eq!(
        std::fs::metadata(path.join(source))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
}

#[cfg(unix)]
#[test]
fn legacy_document_restore_does_not_touch_unchanged_executable() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let source = "src/user/build.sh";
    persist_files(&path, &[(source, Some(b"unchanged"))]);
    std::fs::set_permissions(path.join(source), std::fs::Permissions::from_mode(0o755)).unwrap();
    let checkpoint = open_project(&path).unwrap().create_checkpoint().unwrap();
    let metadata_path = format!("checkpoints/{checkpoint}.json");
    let mut metadata: Value =
        serde_json::from_slice(&std::fs::read(path.join(&metadata_path)).unwrap()).unwrap();
    metadata.as_object_mut().unwrap().remove("executableModes");
    let bytes = json_bytes(&metadata).unwrap();
    persist_files(&path, &[(metadata_path.as_str(), Some(&bytes))]);
    let session = open_project(&path).unwrap();
    rename(&session, "Changed");
    session.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(snapshot(&session)["document"]["nodes"][0]["name"], "Frame");
    assert_eq!(std::fs::read(path.join(source)).unwrap(), b"unchanged");
    assert_eq!(
        std::fs::metadata(path.join(source))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
}

#[test]
fn every_interrupted_schema_migration_recovers_whole_schema_one_or_two() {
    let mut interrupted = 0;
    let mut migrated = 0;
    for point in 0..8 {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let support = temp.path().join("support");
        drop(crate::create_project(&root, "Historical").unwrap());
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(root.join("document.json")).unwrap()).unwrap();
        document["schemaVersion"] = json!(1);
        let document = json_bytes(&document).unwrap();
        let files = super::scan(&root).unwrap();
        let mut marker: Value =
            serde_json::from_slice(&std::fs::read(root.join("project.json")).unwrap()).unwrap();
        marker["schemaVersion"] = json!(1);
        marker["files"]["document.json"] = json!(hash(&document));
        marker["snapshotHash"] = json!(super::fingerprint(&document, &files));
        let marker = json_bytes(&marker).unwrap();
        std::fs::write(root.join("document.json"), &document).unwrap();
        std::fs::write(root.join("project.json"), &marker).unwrap();
        std::env::set_var("AIC_APPLICATION_SUPPORT", &support);
        INTERRUPT_AFTER.with(|slot| slot.set(Some(point)));
        let session = open_project(&root);
        INTERRUPT_AFTER.with(|slot| slot.set(None));
        match session {
            Ok(session) => {
                migrated += 1;
                drop(session);
            }
            Err(_) => interrupted += 1,
        }
        let reopened = open_project(&root).unwrap();
        let state: Value = serde_json::from_str(&reopened.snapshot_json().unwrap()).unwrap();
        assert_eq!(state["readOnly"], false, "point {point}");
        let version = state["document"]["schemaVersion"].clone();
        assert!(version == json!(1) || version == json!(2), "point {point}");
        assert_eq!(state["document"]["revision"], 0, "point {point}");
        if version == json!(2) {
            let backups = std::fs::read_dir(support.join("AIConstructor/Backups"))
                .expect("migration backup")
                .count();
            assert_eq!(backups, 1, "point {point}");
        }
    }
    assert!(interrupted > 0, "an interrupted run must be exercised");
    assert!(migrated > 0, "the completed run must be exercised");
}
