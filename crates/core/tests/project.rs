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
fn generated_gitignore_excludes_outputs_but_keeps_project_data_trackable() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    drop(create_project(&path, "A").unwrap());
    assert!(std::process::Command::new("git")
        .args(["-C", path.to_str().unwrap(), "init", "-q"])
        .status()
        .unwrap()
        .success());
    for (name, ignored) in [
        ("build/app", true),
        ("dist/app", true),
        ("target/bin", true),
        ("node_modules/lib", true),
        (".aic-local/data", true),
        (".cache/data", true),
        (".env.local", true),
        ("document.json", false),
        ("src/user/index.ts", false),
        ("src/user/build/index.ts", false),
        ("history/transactions/a.json", false),
        ("chats/a/messages.jsonl", false),
    ] {
        let status = std::process::Command::new("git")
            .args([
                "-C",
                path.to_str().unwrap(),
                "check-ignore",
                "--no-index",
                "-q",
                "--",
                name,
            ])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(if ignored { 0 } else { 1 }), "{name}");
    }
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
fn historical_schema_one_editable_project_opens_without_migration() {
    use sha2::{Digest, Sha256};
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let session = create_project(&path, "Historical").unwrap();
    let existing = snapshot(&session);
    drop(session);
    let project_id = &existing["document"]["projectId"];
    let page_id = &existing["document"]["pages"][0]["id"];
    let root_id = &existing["document"]["pages"][0]["rootId"];
    // Exact editable document shape emitted by the original b9a27b4 model.
    let document = json!({
        "schemaVersion": 1, "projectId": project_id, "revision": 1,
        "pages": [{"id": page_id, "name": "Page 1", "rootId": root_id}],
        "nodes": [{"id": root_id, "parentId": null, "kind": "frame", "name": "Historical frame",
            "children": [], "layout": {"x": 0.0, "y": 0.0, "w": 1200.0, "h": 800.0},
            "style": {}, "content": {}, "a11y": {}, "bindings": {}, "events": {},
            "propertyMeta": {"/name": {"origin": "user", "lastRevision": 1, "agentLocked": false}}}],
        "components": [], "tokens": [], "variables": [], "actions": [], "animations": [],
        "assets": [], "codeModules": [], "targets": [], "referenceBaselines": []
    });
    let mut bytes = serde_json::to_vec_pretty(&document).unwrap();
    bytes.push(b'\n');
    std::fs::write(path.join("document.json"), &bytes).unwrap();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let marker_path = path.join("project.json");
    let mut marker: Value = serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
    marker["revision"] = json!(1);
    marker["snapshotHash"] = json!(digest);
    marker["files"]["document.json"] = json!(digest);
    let mut marker_bytes = serde_json::to_vec_pretty(&marker).unwrap();
    marker_bytes.push(b'\n');
    std::fs::write(marker_path, marker_bytes).unwrap();
    let opened = open_project(&path).unwrap();
    assert_eq!(snapshot(&opened)["readOnly"], false);
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
    rename(&opened, "Updated historical frame");
    opened.save().unwrap();
    assert_eq!(
        snapshot(&open_project(&path).unwrap())["document"]["nodes"][0]["name"],
        "Updated historical frame"
    );
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
fn git_index_copies_two_projects_with_real_chats_without_local_cache() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let mut copies = Vec::new();
    let mut project_ids = Vec::new();
    for (folder, title, frame, message_text, source_text) in [
        (
            "first",
            "First",
            "Frame one",
            "First conversation",
            "export const label = 'one';\n",
        ),
        (
            "second",
            "Second",
            "Frame two",
            "Second conversation",
            "export const label = 'two';\n",
        ),
    ] {
        let path = dir.path().join(folder);
        let copy = dir.path().join(format!("{folder}-copy"));
        let session = create_project(&path, title).unwrap();
        rename(&session, frame);
        session.save().unwrap();
        let chat: Value =
            serde_json::from_str(&session.create_chat(title, "discuss").unwrap()).unwrap();
        let chat_id = chat["id"].as_str().unwrap();
        let input =
            json!({"role":"user","content":[{"type":"text","text":message_text}],"attachments":[]});
        session
            .append_chat_message(chat_id, &input.to_string())
            .unwrap();

        let source_path = "src/user/view.tsx";
        std::fs::create_dir_all(path.join("src/user")).unwrap();
        std::fs::write(path.join(source_path), source_text).unwrap();
        let diagnostics: Value =
            serde_json::from_str(&session.diagnostics_json().unwrap()).unwrap();
        let conflict = diagnostics["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["kind"] == "externalChange")
            .unwrap();
        let selections: Vec<Value> = conflict["paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let name = entry["path"].as_str().unwrap();
                json!({"path": name, "take": if name == source_path { "disk" } else { "local" }})
            })
            .collect();
        assert!(selections
            .iter()
            .any(|selection| selection["path"] == source_path));
        let request = json!({
            "schemaVersion": 1, "conflictId": conflict["id"],
            "expectedDiskToken": diagnostics["observedDiskToken"],
            "expectedLocalGeneration": diagnostics["localGeneration"],
            "selections": selections
        });
        session
            .resolve_external_conflict(&request.to_string())
            .unwrap();
        session.save().unwrap();
        project_ids.push(snapshot(&session)["document"]["projectId"].clone());
        drop(session);

        std::fs::write(path.join(".env.local"), "local-only\n").unwrap();
        std::fs::write(path.join(".aic-local/permission-test"), "local-only\n").unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["-c", "core.excludesFile=/dev/null"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        let tracked = git(&["ls-files", "-z"]);
        let tracked = std::str::from_utf8(&tracked).unwrap();
        assert!(tracked.split('\0').any(|name| name == source_path));
        assert!(tracked
            .split('\0')
            .any(|name| name == format!("chats/{chat_id}/chat.json")));
        assert!(tracked
            .split('\0')
            .any(|name| name == format!("chats/{chat_id}/messages.jsonl")));
        assert!(!tracked.contains(".aic-local/"));
        assert!(!tracked.contains(".env.local"));
        assert!(!tracked.split('\0').any(|name| name.ends_with(".db")));

        let unchanged: Vec<_> = tracked
            .split('\0')
            .filter(|name| {
                !name.is_empty()
                    && !["document.json", "history/state.json", "project.json"].contains(name)
            })
            .map(|name| {
                (
                    name,
                    std::fs::metadata(path.join(name))
                        .unwrap()
                        .modified()
                        .unwrap(),
                )
            })
            .collect();
        let session = open_project(&path).unwrap();
        let updated_frame = format!("{frame} updated");
        rename(&session, &updated_frame);
        session.save().unwrap();
        drop(session);
        let changed = git(&["diff", "--name-only"]);
        assert_eq!(
            std::str::from_utf8(&changed).unwrap(),
            "document.json\nhistory/state.json\nproject.json\n"
        );
        let diff = git(&["diff", "--", "document.json", "project.json"]);
        let diff = std::str::from_utf8(&diff).unwrap();
        assert!(diff.contains(frame) && diff.contains(&updated_frame));
        for (name, modified) in unchanged {
            assert_eq!(
                std::fs::metadata(path.join(name))
                    .unwrap()
                    .modified()
                    .unwrap(),
                modified,
                "{name}"
            );
        }
        git(&["add", "-A"]);
        std::fs::create_dir(&copy).unwrap();
        let prefix = format!("{}/", copy.display());
        git(&["checkout-index", "--all", &format!("--prefix={prefix}")]);
        assert!(!copy.join(".aic-local").exists());
        assert!(!copy.join(".env.local").exists());
        std::fs::remove_dir_all(&path).unwrap();
        copies.push((
            copy,
            title,
            updated_frame,
            chat_id.to_owned(),
            message_text,
            source_text,
        ));
    }

    assert_ne!(project_ids[0], project_ids[1]);
    for (copy, title, frame, chat_id, message_text, source_text) in copies {
        let restored = open_project(&copy).unwrap();
        assert_eq!(snapshot(&restored)["readOnly"], false);
        assert_eq!(snapshot(&restored)["document"]["nodes"][0]["name"], frame);
        assert_eq!(
            std::fs::read_to_string(copy.join("src/user/view.tsx")).unwrap(),
            source_text
        );
        let chats: Value = serde_json::from_str(&restored.chats_json().unwrap()).unwrap();
        assert_eq!(chats.as_array().unwrap().len(), 1);
        assert_eq!(chats[0]["id"], chat_id);
        assert_eq!(chats[0]["title"], title);
        let messages: Value =
            serde_json::from_str(&restored.chat_messages_json(&chat_id).unwrap()).unwrap();
        assert_eq!(messages.as_array().unwrap().len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], message_text);
    }
}

#[test]
fn chat_round_trip_and_duplicate_reidentify_without_changing_design() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("source");
    let session = create_project(&path, "Original").unwrap();
    let before = snapshot(&session);
    let generation = |path: &std::path::Path| -> u64 {
        let m: Value =
            serde_json::from_slice(&std::fs::read(path.join("project.json")).unwrap()).unwrap();
        m["storageGeneration"].as_u64().unwrap()
    };
    let old_generation = generation(&path);
    let chat: Value =
        serde_json::from_str(&session.create_chat("Discussion", "discuss").unwrap()).unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let input = json!({"role":"user","content":[{"type":"text","text":"Hello"}],"attachments":[]});
    let message: Value = serde_json::from_str(
        &session
            .append_chat_message(chat_id, &input.to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(snapshot(&session)["fingerprint"], before["fingerprint"]);
    assert_eq!(
        snapshot(&session)["document"]["revision"],
        before["document"]["revision"]
    );
    assert!(generation(&path) >= old_generation + 2);
    let copy_path = dir.path().join("copy");
    let copy = session.duplicate_to(&copy_path, "Copy").unwrap();
    let chats: Value = serde_json::from_str(&copy.chats_json().unwrap()).unwrap();
    let copied_chat = &chats[0];
    assert_ne!(copied_chat["id"], chat["id"]);
    assert_eq!(
        copied_chat["projectId"],
        snapshot(&copy)["document"]["projectId"]
    );
    let messages: Value = serde_json::from_str(
        &copy
            .chat_messages_json(copied_chat["id"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_ne!(messages[0]["id"], message["id"]);
    assert_eq!(messages[0]["chatId"], copied_chat["id"]);
    assert_eq!(messages[0]["content"], message["content"]);
    drop(copy);
    let reopened = open_project(&copy_path).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&reopened.chats_json().unwrap()).unwrap(),
        chats
    );
}

#[test]
fn stale_in_memory_session_rejects_transaction_before_mutation() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let first = create_project(&path, "A").unwrap();
    let second = open_project(&path).unwrap();
    let old = snapshot(&first);
    rename(&second, "Other branch");
    second.save().unwrap();
    let root = &old["document"]["pages"][0]["rootId"];
    let tx = json!({
        "transactionId": Uuid::new_v4(), "baseRevision": old["document"]["revision"],
        "baseFingerprint": old["fingerprint"], "origin": "user", "scopeNodeIds": [root],
        "operations": [{"type":"setProperty","nodeId":root,"path":"/name","value":"Stale"}]
    });
    assert!(first.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&first), old);
    assert_eq!(
        snapshot(&open_project(&path).unwrap())["document"]["nodes"][0]["name"],
        "Other branch"
    );
}

#[test]
fn asset_import_validates_and_relinks_through_history() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let source = dir.path().join("first.svg");
    let replacement = dir.path().join("second.svg");
    std::fs::write(
        &source,
        br#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="1" height="1"/></svg>"#,
    )
    .unwrap();
    std::fs::write(
        &replacement,
        br#"<svg xmlns="http://www.w3.org/2000/svg"><circle r="1"/></svg>"#,
    )
    .unwrap();
    let session = create_project(&path, "Assets").unwrap();
    let initial = snapshot(&session);
    let asset: Value = serde_json::from_str(&session.import_asset(&source).unwrap()).unwrap();
    let asset_path = asset["path"].as_str().unwrap();
    assert!(path.join(asset_path).is_file());
    assert_eq!(
        snapshot(&session)["document"]["revision"].as_u64().unwrap(),
        1
    );
    assert_ne!(snapshot(&session)["fingerprint"], initial["fingerprint"]);
    let updated: Value = serde_json::from_str(
        &session
            .relink_asset(asset["id"].as_str().unwrap(), &replacement)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["id"], asset["id"]);
    assert_ne!(updated["path"], asset["path"]);
    session.undo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["assets"][0]["path"],
        asset["path"]
    );
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(
        snapshot(&reopened)["document"]["assets"][0]["path"],
        asset["path"]
    );
    let invalid = dir.path().join("broken.svg");
    std::fs::write(&invalid, b"<svg><unclosed></svg>").unwrap();
    let before = snapshot(&reopened);
    assert!(reopened.import_asset(&invalid).is_err());
    assert_eq!(snapshot(&reopened), before);
}

#[test]
fn asset_import_limits_are_distinct_from_invalid_assets() {
    let dir = tempdir().unwrap();
    let session = create_project(dir.path().join("project"), "Assets").unwrap();
    let before = snapshot(&session);
    let oversized = dir.path().join("oversized");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(50 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        session.import_asset(&oversized).unwrap_err().code,
        "IMPORT_LIMIT_EXCEEDED"
    );
    let dimensions = dir.path().join("large.png");
    std::fs::write(
        &dimensions,
        [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 39, 16, 0, 0, 39,
            16, 8, 2, 0, 0, 0, 53, 44, 245, 112, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 96, 96,
            96, 0, 0, 0, 4, 0, 1, 246, 23, 56, 85, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ],
    )
    .unwrap();
    let error = session.import_asset(&dimensions).unwrap_err();
    assert_eq!(error.code, "IMPORT_LIMIT_EXCEEDED", "{error}");
    let empty = dir.path().join("empty");
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(
        session.import_asset(&empty).unwrap_err().code,
        "INVALID_DOCUMENT"
    );
    assert_eq!(snapshot(&session), before);
}

#[test]
fn diagnostics_detect_external_edits_in_open_session() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let session = create_project(&path, "A").unwrap();
    let input = dir.path().join("asset.svg");
    std::fs::write(&input, br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&input).unwrap()).unwrap();
    let relative = asset["path"].as_str().unwrap();
    std::fs::remove_file(path.join(relative)).unwrap();
    let diagnostics: Value = serde_json::from_str(&session.diagnostics_json().unwrap()).unwrap();
    assert_eq!(diagnostics["readOnly"], true);
    assert!(diagnostics["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["path"] == relative && v["kind"] == "missing" && v["actualHash"].is_null()));
    session
        .relink_asset(asset["id"].as_str().unwrap(), &input)
        .unwrap();
    assert_eq!(
        std::fs::read(path.join(relative)).unwrap(),
        std::fs::read(input).unwrap()
    );
    assert_eq!(snapshot(&open_project(&path).unwrap())["readOnly"], false);
}

#[test]
fn corrupt_asset_relink_preserves_external_bytes_and_survives_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let old = dir.path().join("old.svg");
    let replacement = dir.path().join("new.svg");
    std::fs::write(
        &old,
        br#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#,
    )
    .unwrap();
    std::fs::write(
        &replacement,
        br#"<svg xmlns="http://www.w3.org/2000/svg"><circle/></svg>"#,
    )
    .unwrap();
    let session = create_project(&path, "A").unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&old).unwrap()).unwrap();
    let old_path = asset["path"].as_str().unwrap();
    std::fs::write(path.join(old_path), b"external edit, not a valid SVG").unwrap();
    drop(session);
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], true);
    let new: Value = serde_json::from_str(
        &session
            .relink_asset(asset["id"].as_str().unwrap(), &replacement)
            .unwrap(),
    )
    .unwrap();
    assert_ne!(new["path"], asset["path"]);
    assert_eq!(snapshot(&session)["readOnly"], false);
    let diagnostics: Value = serde_json::from_str(&session.diagnostics_json().unwrap()).unwrap();
    let id = diagnostics["conflicts"][0]["id"].as_str().unwrap();
    assert_eq!(
        std::fs::read(path.join("conflicts").join(id).join("external")).unwrap(),
        b"external edit, not a valid SVG"
    );
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], false);
    assert_eq!(
        snapshot(&reopened)["document"]["assets"][0]["path"],
        new["path"]
    );
}

#[test]
fn unknown_schema_one_field_is_exposed_read_only_without_writing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let document_path = path.join("document.json");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&document_path).unwrap()).unwrap();
    document["futureExtension"] = json!({"original": [1, 2, 3]});
    let bytes = serde_json::to_vec_pretty(&document).unwrap();
    std::fs::write(&document_path, &bytes).unwrap();
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]["document.json"] = json!(digest);
    manifest["snapshotHash"] = json!(digest);
    std::fs::write(manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let session = open_project(&path).unwrap();
    let state = snapshot(&session);
    assert_eq!(state["readOnly"], true);
    assert_eq!(
        state["document"]["futureExtension"],
        document["futureExtension"]
    );
    assert!(session.save().is_err());
    assert_eq!(std::fs::read(document_path).unwrap(), bytes);
}

#[test]
fn unknown_manifest_fields_remain_exactly_read_only_across_reopen_and_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let marker_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
    manifest["futureData"] = json!({"nested": [1, {"preserve": true}]});
    let original = format!(" \n{}\n  ", serde_json::to_string(&manifest).unwrap()).into_bytes();
    std::fs::write(&marker_path, &original).unwrap();
    for _ in 0..2 {
        let session = open_project(&path).unwrap();
        assert_eq!(snapshot(&session)["readOnly"], true);
        assert!(session.rename("Must not publish").is_err());
        assert!(session.save().is_err());
        assert_eq!(std::fs::read(&marker_path).unwrap(), original);
        drop(session);
    }
}

#[test]
fn known_manifest_fields_with_noncanonical_uuid_still_open_and_save() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    drop(create_project(&path, "Example").unwrap());
    let marker_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
    manifest["projectId"] = json!(manifest["projectId"].as_str().unwrap().to_uppercase());
    std::fs::write(&marker_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], false);
    session.rename("Known fields").unwrap();
    session.save().unwrap();
    drop(session);
    assert_eq!(
        snapshot(&open_project(&path).unwrap())["name"],
        "Known fields"
    );
}

#[test]
fn shared_missing_asset_cannot_silently_rebind_another_id() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let old = dir.path().join("old.svg");
    let new = dir.path().join("new.svg");
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
    let session = create_project(&path, "A").unwrap();
    let first: Value = serde_json::from_str(&session.import_asset(&old).unwrap()).unwrap();
    let second: Value = serde_json::from_str(&session.import_asset(&old).unwrap()).unwrap();
    assert_ne!(first["id"], second["id"]);
    assert_eq!(first["path"], second["path"]);
    std::fs::remove_file(path.join(first["path"].as_str().unwrap())).unwrap();
    let replacement: Value = serde_json::from_str(
        &session
            .relink_asset(first["id"].as_str().unwrap(), &new)
            .unwrap(),
    )
    .unwrap();
    let state = snapshot(&session);
    assert_eq!(state["document"]["assets"][0]["path"], replacement["path"]);
    assert_eq!(state["document"]["assets"][1]["path"], second["path"]);
    assert_eq!(
        std::fs::read(path.join(second["path"].as_str().unwrap())).unwrap(),
        std::fs::read(&old).unwrap()
    );
    session.undo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["assets"][1]["path"],
        second["path"]
    );
}

#[test]
fn chat_search_fork_archive_and_delete_leave_design_untouched() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let session = create_project(&path, "A").unwrap();
    let before = snapshot(&session);
    let chat: Value =
        serde_json::from_str(&session.create_chat("Source", "plan").unwrap()).unwrap();
    let id = chat["id"].as_str().unwrap();
    let first: Value = serde_json::from_str(&session.append_chat_message(id,
        &json!({"role":"user","content":[{"type":"text","text":"First question"}],"attachments":[]}).to_string()).unwrap()).unwrap();
    session.append_chat_message(id,
        &json!({"role":"assistant","content":[{"type":"text","text":"Later answer"}],"attachments":[]}).to_string()).unwrap();
    let matches: Value =
        serde_json::from_str(&session.search_chats_json("FIRST").unwrap()).unwrap();
    assert_eq!(matches.as_array().unwrap().len(), 1);
    assert_eq!(matches[0]["message"]["id"], first["id"]);
    let fork: Value = serde_json::from_str(
        &session
            .fork_chat(id, first["id"].as_str().unwrap(), "Branch")
            .unwrap(),
    )
    .unwrap();
    let fork_id = fork["id"].as_str().unwrap();
    let messages: Value =
        serde_json::from_str(&session.chat_messages_json(fork_id).unwrap()).unwrap();
    assert_eq!(messages.as_array().unwrap().len(), 1);
    assert_ne!(messages[0]["id"], first["id"]);
    assert_eq!(messages[0]["content"], first["content"]);
    session.archive_chat(id, true).unwrap();
    assert!(session
        .append_chat_message(
            id,
            &json!({"role":"user","content":[{"type":"text","text":"blocked"}],"attachments":[]})
                .to_string()
        )
        .is_err());
    session.delete_chat(id).unwrap();
    assert_eq!(snapshot(&session)["fingerprint"], before["fingerprint"]);
    assert_eq!(snapshot(&session)["document"], before["document"]);
    drop(session);
    let reopened = open_project(path).unwrap();
    let chats: Value = serde_json::from_str(&reopened.chats_json().unwrap()).unwrap();
    assert_eq!(chats.as_array().unwrap().len(), 1);
    assert_eq!(chats[0]["id"], fork["id"]);
}

#[test]
fn duplicate_never_overwrites_an_existing_destination() {
    let dir = tempdir().unwrap();
    let source = create_project(dir.path().join("source"), "A").unwrap();
    let destination = dir.path().join("destination");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("keep.txt"), b"external data").unwrap();
    assert!(source.duplicate_to(&destination, "Copy").is_err());
    assert_eq!(
        std::fs::read(destination.join("keep.txt")).unwrap(),
        b"external data"
    );
    assert!(!destination.join("project.json").exists());
}

#[test]
fn failed_save_preserves_local_and_external_versions_in_conflict_archive() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("project");
    let session = create_project(&path, "A").unwrap();
    rename(&session, "Unsaved local");
    std::fs::write(path.join("document.json"), b"external broken JSON").unwrap();
    assert!(session.save().is_err());
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], true);
    let diagnostics: Value = serde_json::from_str(&reopened.diagnostics_json().unwrap()).unwrap();
    let conflicts = diagnostics["conflicts"].as_array().unwrap();
    // Reopening can add a second archive whose local version is the saved base.
    // Archive order is by content hash, not by the time the conflict was captured.
    let locals = conflicts
        .iter()
        .filter_map(|conflict| conflict["paths"].as_array())
        .flatten()
        .filter(|entry| entry["path"] == "document.json" && entry["local"]["state"] == "blob")
        .map(|entry| {
            let local_path = entry["local"]["archiveRelativePath"].as_str().unwrap();
            serde_json::from_slice::<Value>(&std::fs::read(path.join(local_path)).unwrap()).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(locals
        .iter()
        .any(|document| document["nodes"][0]["name"] == "Unsaved local"));
    assert_eq!(
        std::fs::read(path.join("document.json")).unwrap(),
        b"external broken JSON"
    );
}

#[test]
fn duplicate_checkpoint_restores_with_the_new_project_identity() {
    let dir = tempdir().unwrap();
    let original = create_project(dir.path().join("original"), "A").unwrap();
    rename(&original, "Checkpoint frame");
    original.save().unwrap();
    let checkpoint = original.create_checkpoint().unwrap();
    rename(&original, "Later frame");
    original.save().unwrap();
    let duplicate = original
        .duplicate_to(dir.path().join("duplicate"), "B")
        .unwrap();
    let duplicate_id = snapshot(&duplicate)["document"]["projectId"].clone();
    assert_ne!(duplicate_id, snapshot(&original)["document"]["projectId"]);
    duplicate.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(snapshot(&duplicate)["document"]["projectId"], duplicate_id);
    assert_eq!(
        snapshot(&duplicate)["document"]["nodes"][0]["name"],
        "Checkpoint frame"
    );
    assert_eq!(
        snapshot(&original)["document"]["nodes"][0]["name"],
        "Later frame"
    );
}

#[test]
fn malformed_font_collection_headers_are_bounded_and_leave_import_and_relink_atomic() {
    fn header(count: u32) -> Vec<u8> {
        let mut bytes = b"ttcf\0\x01\0\0".to_vec();
        bytes.extend_from_slice(&count.to_be_bytes());
        bytes
    }
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    let session = create_project(&project, "Bounded fonts").unwrap();
    let valid = dir.path().join("valid.ttf");
    std::fs::write(&valid, font_test_data::AHEM).unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&valid).unwrap()).unwrap();
    let before = snapshot(&session);
    let source = dir.path().join("malicious.ttc");
    let mut bad_offset = header(1);
    bad_offset.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut bad_version = header(1);
    bad_version[4..8].copy_from_slice(&3u32.to_be_bytes());
    let mut too_many_tables = b"\0\x01\0\0\0\x81\0\0\0\0\0\0".to_vec();
    too_many_tables.resize(12 + 129 * 16, 0);
    for (bytes, expected) in [
        (header(u32::MAX), "IMPORT_LIMIT_EXCEEDED"),
        (header(65), "IMPORT_LIMIT_EXCEEDED"),
        (header(0), "INVALID_DOCUMENT"),
        (header(1), "INVALID_DOCUMENT"),
        (header(64), "INVALID_DOCUMENT"),
        (bad_offset, "INVALID_DOCUMENT"),
        (bad_version, "INVALID_DOCUMENT"),
        (too_many_tables, "IMPORT_LIMIT_EXCEEDED"),
    ] {
        std::fs::write(&source, bytes).unwrap();
        assert_eq!(session.import_asset(&source).unwrap_err().code, expected);
        assert_eq!(
            session
                .relink_asset(asset["id"].as_str().unwrap(), &source)
                .unwrap_err()
                .code,
            expected
        );
        assert_eq!(snapshot(&session), before);
        assert_eq!(
            std::fs::read(project.join(asset["path"].as_str().unwrap())).unwrap(),
            font_test_data::AHEM
        );
    }
}

#[cfg(unix)]
#[test]
fn fifo_assets_are_rejected_before_any_blocking_open() {
    use std::{
        ffi::CString,
        fs::OpenOptions,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        sync::{mpsc, Arc},
        time::Duration,
    };
    let dir = tempdir().unwrap();
    let session = Arc::new(create_project(dir.path().join("project"), "Nonregular asset").unwrap());
    let source = dir.path().join("font.ttf");
    std::fs::write(&source, font_test_data::AHEM).unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&source).unwrap()).unwrap();
    let before = snapshot(&session);
    let fifo = dir.path().join("fifo.ttf");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a NUL-terminated path in this test's temporary directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (sender, receiver) = mpsc::channel();
    let worker_session = session.clone();
    let worker_path = fifo.clone();
    let worker = std::thread::spawn(move || {
        let import = worker_session.import_asset(&worker_path).unwrap_err().code;
        let relink = worker_session
            .relink_asset(asset["id"].as_str().unwrap(), &worker_path)
            .unwrap_err()
            .code;
        sender.send((import, relink)).unwrap();
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(errors) => assert_eq!(errors, ("INVALID_DOCUMENT", "INVALID_DOCUMENT")),
        Err(error) => {
            // Release a regressed blocking reader so this test fails rather than hangs.
            let _unblock = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
                .unwrap();
            worker.join().unwrap();
            panic!("FIFO import/relink blocked: {error}");
        }
    }
    worker.join().unwrap();
    assert_eq!(snapshot(&session), before);
}
