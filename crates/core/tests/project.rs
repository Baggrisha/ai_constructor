use aic_core::{create_project, open_project};
use serde_json::{json, Value};
use tempfile::tempdir;
use uuid::Uuid;

fn snapshot(session: &aic_core::ProjectSession) -> Value {
    serde_json::from_str(&session.snapshot_json().unwrap()).unwrap()
}

fn rename(session: &aic_core::ProjectSession, name: &str) {
    let snap = snapshot(session);
    let root = snap["document"]["pages"][0]["rootId"].as_str().unwrap();
    let tx = json!({
        "transactionId": Uuid::new_v4(),
        "baseRevision": snap["document"]["revision"],
        "baseFingerprint": snap["fingerprint"],
        "origin": "user",
        "scopeNodeIds": [root],
        "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": name}]
    });
    session.apply_transaction(&tx.to_string()).unwrap();
}

#[test]
fn projects_are_independent_and_restore_with_undo_redo() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let first = create_project(&a, "A").unwrap();
    let initial_manifest: Value =
        serde_json::from_slice(&std::fs::read(a.join("project.json")).unwrap()).unwrap();
    assert_eq!(
        snapshot(&first)["fingerprint"],
        initial_manifest["snapshotHash"]
    );
    let second = create_project(&b, "B").unwrap();
    rename(&first, "New frame");
    first.save().unwrap();
    let saved_manifest: Value =
        serde_json::from_slice(&std::fs::read(a.join("project.json")).unwrap()).unwrap();
    assert_eq!(
        snapshot(&first)["fingerprint"],
        saved_manifest["snapshotHash"]
    );
    drop(first);
    drop(second);
    let first = open_project(&a).unwrap();
    let second = open_project(&b).unwrap();
    assert_eq!(
        snapshot(&first)["document"]["nodes"][0]["name"],
        "New frame"
    );
    assert_eq!(snapshot(&second)["document"]["nodes"][0]["name"], "Frame");
    first.undo().unwrap();
    assert_eq!(snapshot(&first)["document"]["nodes"][0]["name"], "Frame");
    first.redo().unwrap();
    assert_eq!(
        snapshot(&first)["document"]["nodes"][0]["name"],
        "New frame"
    );
    first.save().unwrap();
    assert_eq!(
        snapshot(&open_project(&a).unwrap())["document"]["nodes"][0]["name"],
        "New frame"
    );
}

#[test]
fn stale_transaction_and_external_edit_do_not_destroy_data() {
    let dir = tempdir().unwrap();
    let session = create_project(dir.path().join("project"), "A").unwrap();
    let old = snapshot(&session);
    rename(&session, "First");
    let tx = json!({
        "transactionId": Uuid::new_v4(), "baseRevision": old["document"]["revision"],
        "baseFingerprint": old["fingerprint"], "origin": "user",
        "scopeNodeIds": [old["document"]["pages"][0]["rootId"]],
        "operations": [{"type": "setProperty", "nodeId": old["document"]["pages"][0]["rootId"], "path": "/name", "value": "Second"}]
    });
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    let doc = dir.path().join("project/document.json");
    std::fs::write(&doc, b"external data").unwrap();
    assert!(session.save().is_err());
    assert_eq!(std::fs::read(doc).unwrap(), b"external data");
}

#[test]
fn future_schema_is_read_only() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let session = create_project(&path, "Future").unwrap();
    drop(session);
    // Simulate a newer writer by changing both the document and its manifest.
    let doc_path = path.join("document.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&doc_path).unwrap()).unwrap();
    doc["schemaVersion"] = json!(99);
    let mut bytes = serde_json::to_vec_pretty(&doc).unwrap();
    bytes.push(b'\n');
    std::fs::write(&doc_path, &bytes).unwrap();
    use sha2::{Digest, Sha256};
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]["document.json"] = json!(format!("{:x}", Sha256::digest(&bytes)));
    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    bytes.push(b'\n');
    std::fs::write(manifest_path, bytes).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], true);
    assert!(session.save().is_err());
}

#[test]
fn checkpoint_restores_document_as_a_new_revision_after_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    rename(&session, "Checkpoint value");
    session.save().unwrap();
    let checkpoint = session.create_checkpoint().unwrap();
    rename(&session, "Later value");
    session.save().unwrap();
    let before_revision = snapshot(&session)["document"]["revision"].as_u64().unwrap();
    drop(session);
    let session = open_project(&path).unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    let restored = snapshot(&session);
    assert_eq!(restored["document"]["nodes"][0]["name"], "Checkpoint value");
    assert_eq!(restored["document"]["revision"], before_revision + 1);
    session.save().unwrap();
    assert_eq!(
        snapshot(&open_project(&path).unwrap())["document"]["nodes"][0]["name"],
        "Checkpoint value"
    );
}

#[test]
fn duplicate_has_a_new_project_identity_and_independent_state() {
    let dir = tempdir().unwrap();
    let original = create_project(dir.path().join("original"), "Original").unwrap();
    rename(&original, "Original frame");
    original.save().unwrap();
    let duplicate = original
        .duplicate_to(dir.path().join("copy"), "Copy")
        .unwrap();
    assert_ne!(
        snapshot(&original)["document"]["projectId"],
        snapshot(&duplicate)["document"]["projectId"]
    );
    assert_eq!(
        snapshot(&duplicate)["document"]["nodes"][0]["name"],
        "Original frame"
    );
    rename(&duplicate, "Copy frame");
    duplicate.save().unwrap();
    assert_eq!(
        snapshot(&original)["document"]["nodes"][0]["name"],
        "Original frame"
    );
    assert_eq!(
        snapshot(&open_project(dir.path().join("copy")).unwrap())["document"]["nodes"][0]["name"],
        "Copy frame"
    );
}

#[test]
fn same_revision_with_another_fingerprint_is_rejected() {
    let dir = tempdir().unwrap();
    let session = create_project(dir.path().join("project"), "A").unwrap();
    let state = snapshot(&session);
    let root = &state["document"]["pages"][0]["rootId"];
    let tx = json!({
        "transactionId": Uuid::new_v4(), "baseRevision": state["document"]["revision"],
        "baseFingerprint": "0000000000000000000000000000000000000000000000000000000000000000",
        "origin": "user", "scopeNodeIds": [root],
        "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Wrong branch"}]
    });
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), state);
}

#[test]
fn tracked_files_can_be_copied_without_local_cache() {
    use sha2::{Digest, Sha256};
    let dir = tempdir().unwrap();
    let source = dir.path().join("source");
    let copy = dir.path().join("copy");
    let session = create_project(&source, "Portable").unwrap();
    rename(&session, "Saved frame");
    session.save().unwrap();
    drop(session);
    // A completed chat written by a separate compliant writer is just tracked JSON/JSONL.
    let chat_path = "chats/46f81486-0f95-4f9c-83e9-1e8282aaecb4/messages.jsonl";
    std::fs::create_dir_all(source.join(chat_path).parent().unwrap()).unwrap();
    let message = b"{\"role\":\"user\",\"text\":\"hello\"}\n";
    std::fs::write(source.join(chat_path), message).unwrap();
    let manifest_path = source.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"][chat_path] = json!(format!("{:x}", Sha256::digest(message)));
    manifest["storageGeneration"] = json!(manifest["storageGeneration"].as_u64().unwrap() + 1);
    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    bytes.push(b'\n');
    std::fs::write(&manifest_path, &bytes).unwrap();
    std::fs::create_dir(&copy).unwrap();
    std::fs::write(copy.join("project.json"), bytes).unwrap();
    for path in manifest["files"].as_object().unwrap().keys() {
        let dest = copy.join(path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(source.join(path), dest).unwrap();
    }
    let restored = open_project(&copy).unwrap();
    assert_eq!(snapshot(&restored)["readOnly"], false);
    assert_eq!(
        snapshot(&restored)["document"]["nodes"][0]["name"],
        "Saved frame"
    );
    assert_eq!(std::fs::read(copy.join(chat_path)).unwrap(), message);
}
