use aic_core::{create_project, open_project, ProjectSession};
use serde_json::{json, Value};
use tempfile::tempdir;
use uuid::Uuid;

fn snapshot(session: &ProjectSession) -> Value {
    serde_json::from_str(&session.snapshot_json().unwrap()).unwrap()
}

fn node(id: Uuid, parent: Uuid, kind: &str) -> Value {
    json!({
        "id": id, "parentId": parent, "kind": kind, "name": kind, "children": [],
        "layout": {"x": 0, "y": 0, "w": 100, "h": 50},
        "style": {}, "content": {}, "a11y": {}, "bindings": {}, "events": {}, "propertyMeta": {}
    })
}

fn transaction(session: &ProjectSession, scope: &[Uuid], operations: Value) -> Value {
    let before = snapshot(session);
    json!({
        "transactionId": Uuid::new_v4(), "baseRevision": before["document"]["revision"],
        "baseFingerprint": before["fingerprint"], "origin": "user", "scopeNodeIds": scope,
        "operations": operations
    })
}

fn root(session: &ProjectSession) -> Uuid {
    Uuid::parse_str(
        snapshot(session)["document"]["pages"][0]["rootId"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

fn write_document_fixture(path: &std::path::Path, doc: &Value) {
    use sha2::{Digest, Sha256};
    let mut bytes = serde_json::to_vec_pretty(doc).unwrap();
    bytes.push(b'\n');
    std::fs::write(path.join("document.json"), &bytes).unwrap();
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    manifest["files"]["document.json"] = json!(hash);
    manifest["snapshotHash"] = json!(hash);
    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    bytes.push(b'\n');
    std::fs::write(manifest_path, bytes).unwrap();
}

#[test]
fn tree_transactions_survive_save_reopen_and_history() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Tree").unwrap();
    let root = root(&session);
    let group = Uuid::new_v4();
    let text = Uuid::new_v4();
    let tx = transaction(
        &session,
        &[root, group, text],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(group, root, "group")},
            {"type": "insertNode", "parentId": group, "index": 0, "node": node(text, group, "text")},
            {"type": "setProperty", "nodeId": text, "path": "/content/text", "value": "Привет"}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    session.save().unwrap();
    drop(session);
    let session = open_project(&path).unwrap();
    let saved = snapshot(&session);
    assert_eq!(saved["readOnly"], false);
    assert_eq!(saved["document"]["nodes"].as_array().unwrap().len(), 3);
    assert_eq!(saved["document"]["nodes"][2]["content"]["text"], "Привет");
    let tx = transaction(
        &session,
        &[root, group, text],
        json!([
            {"type": "moveNode", "nodeId": text, "parentId": root, "index": 0}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let moved = snapshot(&session);
    assert_eq!(
        moved["document"]["nodes"][0]["children"],
        json!([text, group])
    );
    session.undo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][1]["children"],
        json!([text])
    );
    session.redo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][0]["children"],
        json!([text, group])
    );
    let tx = transaction(
        &session,
        &[root, group],
        json!([
            {"type": "removeNode", "nodeId": group}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn invalid_later_operation_leaves_everything_unchanged() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Atomic").unwrap();
    let root = root(&session);
    let group = Uuid::new_v4();
    let tx = transaction(
        &session,
        &[root, group],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(group, root, "group")}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[root, group],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/name", "value": "Changed"},
            {"type": "moveNode", "nodeId": group, "parentId": group, "index": 0}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
}

#[test]
fn typed_dependencies_validate_scope_and_survive_reopen() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Dependencies").unwrap();
    let root = root(&session);
    let text = Uuid::new_v4();
    let token = Uuid::new_v4();
    let animation = Uuid::new_v4();
    let action = Uuid::new_v4();
    let tx = transaction(
        &session,
        &[root, text],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(text, root, "text")},
            {"type": "setToken", "token": {"id": token, "name": "Message", "value": "Hello"}},
            {"type": "setBinding", "nodeId": text, "path": "/content/text", "binding": {"type": "token", "tokenId": token}},
            {"type": "upsertAnimation", "animation": {"id": animation, "nodeId": text, "property": "/style/opacity", "durationMs": 300, "delayMs": 0, "keyframes": [
                {"offset": 0, "value": 0}, {"offset": 1, "value": 1}
            ]}},
            {"type": "upsertAction", "action": {"id": action, "kind": {"type": "playAnimation", "animationId": animation}}},
            {"type": "setProperty", "nodeId": text, "path": "/events/click", "value": action}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let wrong_scope = transaction(
        &session,
        &[root],
        json!([
            {"type": "setToken", "token": {"id": token, "name": "Message", "value": "Changed"}}
        ]),
    );
    assert!(session.apply_transaction(&wrong_scope.to_string()).is_err());
    let wrong_type = transaction(
        &session,
        &[root, text],
        json!([
            {"type": "setToken", "token": {"id": token, "name": "Message", "value": 42}}
        ]),
    );
    assert!(session.apply_transaction(&wrong_type.to_string()).is_err());
    let remove_referenced = transaction(
        &session,
        &[root, text],
        json!([
            {"type": "removeNode", "nodeId": text}
        ]),
    );
    assert!(session
        .apply_transaction(&remove_referenced.to_string())
        .is_err());
    assert_eq!(snapshot(&session), before);
    session.save().unwrap();
    drop(session);
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["document"], before["document"]);
    let tx = transaction(
        &session,
        &[root, text],
        json!([
            {"type": "setBinding", "nodeId": text, "path": "/content/text", "binding": null},
            {"type": "setProperty", "nodeId": text, "path": "/content/text", "value": "Local"}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][1]["content"]["text"],
        "Local"
    );
    session.undo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][1]["bindings"]["/content/text"]["tokenId"],
        token.to_string()
    );
}

#[test]
fn invalid_resources_layouts_and_properties_do_not_mutate_session() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Validation").unwrap();
    let root = root(&session);
    let image = Uuid::new_v4();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[root, image],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(image, root, "image")},
            {"type": "setProperty", "nodeId": image, "path": "/content/assetId", "value": Uuid::new_v4()}
        ]),
    );
    let error = session.apply_transaction(&tx.to_string()).unwrap_err();
    assert_eq!(error.code, "MISSING_ASSET");
    assert_eq!(snapshot(&session), before);
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "upsertCodeModule", "codeModule": {"id": Uuid::new_v4(), "path": "src/user/missing.tsx", "props": {}, "events": []}}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/responsive", "value": [
                {"breakpoint": 500, "w": 100}, {"breakpoint": 500, "w": 200}
            ]}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/content/text", "value": "Not a text node"}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": -1}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/events/click", "value": Uuid::new_v4()}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(image, root, "image")}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
}

#[test]
fn ancestor_agent_lock_survives_user_edit_and_undo() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Locked").unwrap());
    let doc_path = path.join("document.json");
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&doc_path).unwrap()).unwrap();
    doc["nodes"][0]["propertyMeta"]["/layout"] =
        json!({"origin": "user", "lastRevision": 0, "agentLocked": true});
    let mut bytes = serde_json::to_vec_pretty(&doc).unwrap();
    bytes.push(b'\n');
    std::fs::write(&doc_path, &bytes).unwrap();
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]["document.json"] = json!(format!("{:x}", Sha256::digest(&bytes)));
    manifest["snapshotHash"] = json!(format!("{:x}", Sha256::digest(&bytes)));
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_bytes.push(b'\n');
    std::fs::write(&manifest_path, manifest_bytes).unwrap();
    let session = open_project(&path).unwrap();
    let root = root(&session);
    let before = snapshot(&session);
    let mut tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setViewport", "nodeId": root, "width": 1200, "height": 800}
        ]),
    );
    tx["origin"] = json!("agent");
    assert_eq!(
        session.apply_transaction(&tx.to_string()).unwrap_err().code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
    tx["origin"] = json!("user");
    session.apply_transaction(&tx.to_string()).unwrap();
    session.undo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][0]["propertyMeta"]["/layout"]["agentLocked"],
        true
    );
    let mut tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 700}
        ]),
    );
    tx["origin"] = json!("agent");
    assert_eq!(
        session.apply_transaction(&tx.to_string()).unwrap_err().code,
        "LOCKED_PROPERTY"
    );
}

#[test]
fn flow_vector_and_source_geometry_are_stored_separately() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Geometry").unwrap();
    let root = root(&session);
    let vector = Uuid::new_v4();
    let mut vector_node = node(vector, root, "vector");
    vector_node["content"]["vector"] = json!({"paths": [[
        {"type": "moveTo", "x": 1, "y": 2}, {"type": "lineTo", "x": 8, "y": 9}, {"type": "close"}
    ]]});
    let tx = transaction(
        &session,
        &[root, vector],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/flow", "value": {
                "direction": "row", "gap": 12, "padding": {"top": 1, "right": 2, "bottom": 3, "left": 4}, "align": "center"
            }},
            {"type": "insertNode", "parentId": root, "index": 0, "node": vector_node},
            {"type": "setProperty", "nodeId": vector, "path": "/sourceBounds", "value": {"x": 20, "y": 30, "w": 40, "h": 50}},
            {"type": "setProperty", "nodeId": vector, "path": "/sourceDensity", "value": 2},
            {"type": "setProperty", "nodeId": vector, "path": "/responsive", "value": [{"breakpoint": 600, "w": 80}]}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    session.save().unwrap();
    drop(session);
    let session = open_project(path).unwrap();
    let result = snapshot(&session);
    assert_eq!(
        result["document"]["nodes"][0]["layout"]["flow"]["direction"],
        "row"
    );
    assert_eq!(
        result["document"]["nodes"][1]["layout"]["w"].as_f64(),
        Some(100.0)
    );
    assert_eq!(
        result["document"]["nodes"][1]["sourceBounds"]["w"].as_f64(),
        Some(40.0)
    );
    assert_eq!(
        result["document"]["nodes"][1]["responsive"][0]["w"].as_f64(),
        Some(80.0)
    );
    assert_eq!(
        result["document"]["nodes"][1]["content"]["vector"]["paths"][0][1]["type"],
        "lineTo"
    );
}

#[test]
fn code_module_requires_a_tracked_source_and_valid_prop_contract() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Code").unwrap());
    let source_path = "src/user/card.tsx";
    let source = b"export default function Card() { return null; }\n";
    std::fs::write(path.join(source_path), source).unwrap();
    let doc = std::fs::read(path.join("document.json")).unwrap();
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let source_hash = format!("{:x}", Sha256::digest(source));
    manifest["files"][source_path] = json!(source_hash);
    let mut fingerprint = Sha256::new();
    fingerprint.update(&doc);
    fingerprint.update((source_path.len() as u64).to_be_bytes());
    fingerprint.update(source_path.as_bytes());
    fingerprint.update(source_hash.as_bytes());
    manifest["snapshotHash"] = json!(format!("{:x}", fingerprint.finalize()));
    let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    bytes.push(b'\n');
    std::fs::write(manifest_path, bytes).unwrap();
    let session = open_project(&path).unwrap();
    let root = root(&session);
    let module = Uuid::new_v4();
    let component = Uuid::new_v4();
    let page = snapshot(&session)["document"]["pages"][0]["id"].clone();
    let action = Uuid::new_v4();
    let mut component_node = node(component, root, "codeComponent");
    component_node["content"] =
        json!({"moduleId": module, "props": {"title": "Hello", "count": 2}});
    component_node["events"] = json!({"changed": action});
    let tx = transaction(
        &session,
        &[root, component],
        json!([
            {"type": "upsertCodeModule", "codeModule": {"id": module, "path": source_path, "props": {"title": "string", "count": "number"}, "events": ["changed"]}},
            {"type": "upsertAction", "action": {"id": action, "kind": {"type": "navigate", "pageId": page}}},
            {"type": "insertNode", "parentId": root, "index": 0, "node": component_node}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[root, component],
        json!([
            {"type": "upsertCodeModule", "codeModule": {"id": module, "path": source_path, "props": {"title": "boolean"}, "events": []}}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
    session.save().unwrap();
    assert_eq!(std::fs::read(path.join(source_path)).unwrap(), source);
    drop(session);
    assert_eq!(
        snapshot(&open_project(path).unwrap())["document"],
        before["document"]
    );
}

#[test]
fn existing_typed_variable_can_be_bound_and_referenced_by_action() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Variables").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let variable = Uuid::new_v4();
    doc["variables"] = json!([{"id": variable, "name": "Title", "value": "Initial"}]);
    write_document_fixture(&path, &doc);
    let session = open_project(&path).unwrap();
    let root = root(&session);
    let action = Uuid::new_v4();
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setBinding", "nodeId": root, "path": "/name", "binding": {"type": "variable", "variableId": variable}},
            {"type": "upsertAction", "action": {"id": action, "kind": {"type": "setVariable", "variableId": variable, "value": "Next"}}},
            {"type": "setProperty", "nodeId": root, "path": "/events/click", "value": action}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "upsertAction", "action": {"id": action, "kind": {"type": "toggleVariable", "variableId": variable}}}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
    session.save().unwrap();
    drop(session);
    assert_eq!(
        snapshot(&open_project(path).unwrap())["document"],
        before["document"]
    );
}

#[test]
fn a_self_consistent_file_with_an_orphaned_cycle_is_rejected_without_rewrite() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Corrupt").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let id = Uuid::new_v4();
    let mut cyclic = node(id, id, "group");
    cyclic["children"] = json!([id]);
    doc["nodes"].as_array_mut().unwrap().push(cyclic);
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    let error = match open_project(&path) {
        Ok(_) => panic!("cyclic document was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.code, "INVALID_DOCUMENT");
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
}

#[test]
fn unsupported_legacy_content_stays_read_only_and_unchanged() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "External").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    doc["nodes"][0]["content"]["legacyData"] = json!({"keep": [1, 2, 3]});
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], true);
    let id = root(&session);
    let tx = transaction(
        &session,
        &[id],
        json!([
            {"type": "setProperty", "nodeId": id, "path": "/name", "value": "Should not save"}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert!(session.save().is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
}

#[test]
fn unknown_fields_in_vector_close_open_read_only_without_data_loss() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Future vector").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = Uuid::parse_str(doc["pages"][0]["rootId"].as_str().unwrap()).unwrap();
    let vector = Uuid::new_v4();
    let mut vector_node = node(vector, root, "vector");
    vector_node["content"]["vector"] =
        json!({"paths": [[{"type": "close", "legacyData": {"keep": 1}}]]});
    doc["nodes"][0]["children"] = json!([vector]);
    doc["nodes"].as_array_mut().unwrap().push(vector_node);
    write_document_fixture(&path, &doc);
    let original = std::fs::read(path.join("document.json")).unwrap();

    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    assert_eq!(before["readOnly"], true);
    assert_eq!(
        before["document"]["nodes"][1]["content"]["vector"]["paths"][0][0]["legacyData"],
        json!({"keep": 1})
    );
    let tx = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Changed"}]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert!(session.save().is_err());
    assert_eq!(snapshot(&session), before);
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), original);
}

#[test]
fn transactions_reject_unknown_fields_in_vector_close_without_mutation() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Vector validation").unwrap();
    let root = root(&session);
    let vector = Uuid::new_v4();
    let mut vector_node = node(vector, root, "vector");
    let unknown = json!({"paths": [[{"type": "close", "legacyData": {"keep": 1}}]]});
    vector_node["content"]["vector"] = unknown.clone();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[root, vector],
        json!([{"type": "insertNode", "parentId": root, "index": 0, "node": vector_node}]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);

    vector_node["content"]["vector"] = json!({"paths": [[{"type": "close"}]]});
    let tx = transaction(
        &session,
        &[root, vector],
        json!([{"type": "insertNode", "parentId": root, "index": 0, "node": vector_node}]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[vector],
        json!([{"type": "setProperty", "nodeId": vector, "path": "/content/vector", "value": unknown}]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
}

#[test]
fn tracked_content_addressed_asset_can_be_referenced_and_is_not_overwritten() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Assets").unwrap());
    let asset_bytes = b"synthetic resource bytes";
    let hash = format!("{:x}", Sha256::digest(asset_bytes));
    let asset_path = format!("assets/{hash}.png");
    std::fs::write(path.join(&asset_path), asset_bytes).unwrap();
    let id = Uuid::new_v4();
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    doc["assets"] = json!([{"id": id, "path": asset_path}]);
    let mut doc_bytes = serde_json::to_vec_pretty(&doc).unwrap();
    doc_bytes.push(b'\n');
    std::fs::write(path.join("document.json"), &doc_bytes).unwrap();
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]["document.json"] = json!(format!("{:x}", Sha256::digest(&doc_bytes)));
    manifest["files"][asset_path.as_str()] = json!(hash);
    let mut fingerprint = Sha256::new();
    fingerprint.update(&doc_bytes);
    fingerprint.update((asset_path.len() as u64).to_be_bytes());
    fingerprint.update(asset_path.as_bytes());
    fingerprint.update(hash.as_bytes());
    manifest["snapshotHash"] = json!(format!("{:x}", fingerprint.finalize()));
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest_bytes.push(b'\n');
    std::fs::write(manifest_path, manifest_bytes).unwrap();
    let session = open_project(&path).unwrap();
    let root = root(&session);
    let image = Uuid::new_v4();
    let mut image_node = node(image, root, "image");
    image_node["content"]["assetId"] = json!(id);
    let tx = transaction(
        &session,
        &[root, image],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": image_node}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    session.save().unwrap();
    assert_eq!(std::fs::read(path.join(&asset_path)).unwrap(), asset_bytes);
    drop(session);
    let session = open_project(&path).unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][1]["content"]["assetId"],
        id.to_string()
    );
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/name", "value": "Pending"}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    std::fs::write(path.join(&asset_path), b"external change").unwrap();
    assert!(session.save().is_err());
    assert_eq!(
        std::fs::read(path.join(&asset_path)).unwrap(),
        b"external change"
    );
}

#[test]
fn user_can_lock_a_property_and_undo_redo_the_lock_but_agent_cannot_unlock() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Locks").unwrap();
    let root = root(&session);
    let lock_path = "/propertyMeta/~1layout/agentLocked";
    let tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": lock_path, "value": true}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][0]["propertyMeta"]["/layout"]["agentLocked"],
        true
    );
    let mut tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": lock_path, "value": false}
        ]),
    );
    tx["origin"] = json!("agent");
    assert_eq!(
        session.apply_transaction(&tx.to_string()).unwrap_err().code,
        "LOCKED_PROPERTY"
    );
    session.undo().unwrap();
    assert_ne!(
        snapshot(&session)["document"]["nodes"][0]["propertyMeta"]["/layout"]["agentLocked"],
        true
    );
    session.redo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][0]["propertyMeta"]["/layout"]["agentLocked"],
        true
    );
    let mut tx = transaction(
        &session,
        &[root],
        json!([
            {"type": "setViewport", "nodeId": root, "width": 800, "height": 600}
        ]),
    );
    tx["origin"] = json!("agent");
    assert_eq!(
        session.apply_transaction(&tx.to_string()).unwrap_err().code,
        "LOCKED_PROPERTY"
    );
}

#[test]
fn agent_cannot_change_a_shared_token_behind_a_locked_binding() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Shared locks").unwrap();
    let root = root(&session);
    let text = Uuid::new_v4();
    let token = Uuid::new_v4();
    let tx = transaction(
        &session,
        &[root, text],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(text, root, "text")},
            {"type": "setToken", "token": {"id": token, "name": "Label", "value": "First"}},
            {"type": "setBinding", "nodeId": text, "path": "/content/text", "binding": {"type": "token", "tokenId": token}}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let tx = transaction(
        &session,
        &[text],
        json!([
            {"type": "setProperty", "nodeId": text, "path": "/propertyMeta/~1content~1text/agentLocked", "value": true}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let mut tx = transaction(
        &session,
        &[text],
        json!([
            {"type": "setToken", "token": {"id": token, "name": "Label", "value": "Changed"}}
        ]),
    );
    tx["origin"] = json!("agent");
    assert_eq!(
        session.apply_transaction(&tx.to_string()).unwrap_err().code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
}
