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
    let mut fingerprint = Sha256::new();
    fingerprint.update(&bytes);
    for (path, hash) in manifest["files"].as_object().unwrap() {
        if path.starts_with("assets/") || path.starts_with("src/user/") {
            fingerprint.update((path.len() as u64).to_be_bytes());
            fingerprint.update(path.as_bytes());
            fingerprint.update(hash.as_str().unwrap().as_bytes());
        }
    }
    manifest["snapshotHash"] = json!(format!("{:x}", fingerprint.finalize()));
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
fn changing_single_frame_properties_preserves_other_metadata_and_locks_through_history() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Frame metadata").unwrap();
    let root = root(&session);
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 1200},
            {"type": "setProperty", "nodeId": root, "path": "/layout/h", "value": 700}
        ]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    let lock = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/propertyMeta/~1layout~1w/agentLocked", "value": true}
        ]),
    );
    session.apply_transaction(&lock.to_string()).unwrap();
    let before = snapshot(&session)["document"]["nodes"][0]["propertyMeta"].clone();
    assert_eq!(before["/layout/w"]["agentLocked"], true);
    assert_eq!(before["/layout/h"]["origin"], "agent");
    let rename = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/name", "value": "Renamed"}
        ]),
    );
    session.apply_transaction(&rename.to_string()).unwrap();
    for action in [0, 1, 2] {
        if action == 1 {
            session.undo().unwrap();
        }
        if action == 2 {
            session.redo().unwrap();
        }
        let meta = &snapshot(&session)["document"]["nodes"][0]["propertyMeta"];
        assert_eq!(meta["/layout/w"], before["/layout/w"]);
        assert_eq!(meta["/layout/h"], before["/layout/h"]);
    }
    let resize = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 1300}
        ]),
    );
    session.apply_transaction(&resize.to_string()).unwrap();
    let after = snapshot(&session);
    assert_eq!(
        after["document"]["nodes"][0]["propertyMeta"]["/layout/w"]["agentLocked"],
        true
    );
    assert_eq!(
        after["document"]["nodes"][0]["propertyMeta"]["/layout/h"],
        before["/layout/h"]
    );
    let mut agent_resize = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 1400}
        ]),
    );
    agent_resize["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent_resize.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), after);
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
    assert_eq!(
        before["document"]["nodes"][1]["propertyMeta"]["/content/props/title"]["agentLocked"],
        true
    );
    assert_eq!(
        before["document"]["nodes"][1]["propertyMeta"]["/content/props/count"]["agentLocked"],
        true
    );
    let unlock = transaction(
        &session,
        &[component],
        json!([
            {"type":"setProperty","nodeId":component,
             "path":"/propertyMeta/~1content~1props~1count/agentLocked","value":false}
        ]),
    );
    session.apply_transaction(&unlock.to_string()).unwrap();
    let before_agent = snapshot(&session);
    let mut locked = transaction(
        &session,
        &[component],
        json!([
            {"type":"setProperty","nodeId":component,"path":"/content/props/count","value":3},
            {"type":"setProperty","nodeId":component,"path":"/content/props/title","value":"No"}
        ]),
    );
    locked["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&locked.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before_agent);
    let mut valid = transaction(
        &session,
        &[component],
        json!([
            {"type":"setProperty","nodeId":component,"path":"/content/props/count","value":3}
        ]),
    );
    valid["origin"] = json!("agent");
    session.apply_transaction(&valid.to_string()).unwrap();
    let changed = snapshot(&session);
    assert_eq!(
        changed["document"]["nodes"][1]["content"]["props"]["count"].as_f64(),
        Some(3.0)
    );
    assert_eq!(
        changed["document"]["nodes"][1]["content"]["props"]["title"],
        "Hello"
    );
    let invalid = transaction(
        &session,
        &[component],
        json!([
            {"type":"setProperty","nodeId":component,"path":"/content/props/title","value":false}
        ]),
    );
    assert!(session.apply_transaction(&invalid.to_string()).is_err());
    assert_eq!(snapshot(&session), changed);
    session.undo().unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][1]["content"]["props"]["count"].as_f64(),
        Some(2.0)
    );
    session.redo().unwrap();
    let before = snapshot(&session);
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

#[test]
fn component_instances_and_targets_round_trip_and_references_are_atomic() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Components").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = Uuid::parse_str(doc["pages"][0]["rootId"].as_str().unwrap()).unwrap();
    let template = Uuid::new_v4();
    let component = Uuid::new_v4();
    let target = Uuid::new_v4();
    doc["nodes"][0]["children"] = json!([template]);
    doc["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node(template, root, "group"));
    doc["components"] = json!([{"id": component, "rootId": template, "name": "Card"}]);
    doc["targets"] = json!([{"id": target, "adapter": "web-react"}]);
    write_document_fixture(&path, &doc);

    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], false);
    let instance = Uuid::new_v4();
    let mut instance_node = node(instance, root, "componentInstance");
    instance_node["content"]["componentId"] = json!(component);
    let tx = transaction(
        &session,
        &[root, instance],
        json!([{"type": "insertNode", "parentId": root, "index": 1, "node": instance_node}]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    assert_eq!(
        before["document"]["nodes"][2]["propertyMeta"]["/content/componentId"]["origin"],
        "user"
    );
    let tx = transaction(
        &session,
        &[instance],
        json!([{"type": "setProperty", "nodeId": instance, "path": "/content/componentId", "value": Uuid::new_v4()}]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
    let tx = transaction(
        &session,
        &[instance],
        json!([{
            "type": "setProperty", "nodeId": instance,
            "path": "/propertyMeta/~1content~1componentId/agentLocked", "value": true
        }]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let mut agent_tx = transaction(
        &session,
        &[instance],
        json!([{
            "type": "setProperty", "nodeId": instance, "path": "/content/componentId", "value": component
        }]),
    );
    agent_tx["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent_tx.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
    let tx = transaction(
        &session,
        &[root, template],
        json!([{"type": "removeNode", "nodeId": template}]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
    let tx = transaction(
        &session,
        &[root, template, instance],
        json!([{"type": "moveNode", "nodeId": instance, "parentId": template, "index": 0}]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["document"], before["document"]);
}

#[test]
fn malformed_known_records_are_rejected_but_unknown_legacy_records_are_preserved() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Unknown").unwrap());
    let original: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let mut invalid = original.clone();
    invalid["components"] = json!([{"id": Uuid::new_v4(), "rootId": "not-a-uuid"}]);
    write_document_fixture(&path, &invalid);
    assert!(open_project(&path).is_err());

    let mut legacy = original;
    legacy["components"] = json!([{"oldFormat": {"keep": [1, 2, 3]}}]);
    legacy["targets"] = json!(["legacy-target"]);
    legacy["referenceBaselines"] = json!([{"future": true}]);
    let root = Uuid::parse_str(legacy["pages"][0]["rootId"].as_str().unwrap()).unwrap();
    let instance = Uuid::new_v4();
    legacy["nodes"][0]["children"] = json!([instance]);
    legacy["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node(instance, root, "componentInstance"));
    write_document_fixture(&path, &legacy);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], true);
    assert_eq!(snapshot(&session)["document"], legacy);
    assert!(session.save().is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
}

#[test]
fn component_extensions_remain_read_only_but_known_references_are_validated() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Future component").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = doc["pages"][0]["rootId"].clone();
    doc["components"] = json!([{"id": Uuid::new_v4(), "rootId": root, "future": {"keep": 1}}]);
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], true);
    assert_eq!(snapshot(&session)["document"], doc);
    assert!(session.save().is_err());
    drop(session);
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
    doc["components"][0]["rootId"] = json!(Uuid::new_v4());
    write_document_fixture(&path, &doc);
    assert!(open_project(&path).is_err());
}

#[test]
fn known_properties_are_checked_even_when_other_fields_are_unknown() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Invalid known field").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    doc["nodes"][0]["style"] = json!({"future": true, "opacity": "not-a-number"});
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    assert!(open_project(&path).is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
    doc["nodes"][0]["style"] = json!({"future": true, "opacity": 2});
    write_document_fixture(&path, &doc);
    assert!(open_project(&path).is_err());
}

#[test]
fn unknown_vector_command_fields_do_not_hide_invalid_known_commands() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Mixed vector").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = Uuid::parse_str(doc["pages"][0]["rootId"].as_str().unwrap()).unwrap();
    let id = Uuid::new_v4();
    let mut vector = node(id, root, "vector");
    vector["content"]["vector"] = json!({"paths": [[
        {"type": "close", "future": true}, {"type": "lineTo", "x": "invalid", "y": 2}
    ]]});
    doc["nodes"][0]["children"] = json!([id]);
    doc["nodes"].as_array_mut().unwrap().push(vector);
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    assert!(open_project(&path).is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
}

#[test]
fn unknown_nested_fields_are_opened_read_only_without_rewriting() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Future layout").unwrap());
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    doc["nodes"][0]["layout"]["futureTransform"] = json!({"preserve": [1, 2]});
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], true);
    assert_eq!(snapshot(&session)["document"], doc);
    assert!(session.save().is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
}

#[test]
fn viewport_coordinates_follow_flow_reordering_and_source_density() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Flow coordinates").unwrap();
    let root = root(&session);
    let fixed = Uuid::new_v4();
    let fill = Uuid::new_v4();
    let mut fixed_node = node(fixed, root, "group");
    fixed_node["layout"]["h"] = json!(40);
    fixed_node["responsive"] = json!([{"breakpoint": 600, "w": 150}]);
    let mut fill_node = node(fill, root, "group");
    fill_node["layout"] = json!({"x": 500, "y": 600, "w": 100, "h": 20, "widthMode": "fill"});
    fill_node["sourceDensity"] = json!(2);
    fill_node["sourceBounds"] = json!({"x": 800, "y": 900, "w": 200, "h": 40});
    let tx = transaction(
        &session,
        &[root, fixed, fill],
        json!([
            {"type": "setViewport", "nodeId": root, "width": 300, "height": 120},
            {"type": "setProperty", "nodeId": root, "path": "/layout/flow", "value": {
                "direction": "row", "gap": 10,
                "padding": {"top": 10, "right": 10, "bottom": 10, "left": 10}, "align": "center"
            }},
            {"type": "insertNode", "parentId": root, "index": 0, "node": fixed_node},
            {"type": "insertNode", "parentId": root, "index": 1, "node": fill_node}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let transform = |x, y, inverse| -> Value {
        serde_json::from_str(
            &session
                .transform_point_json(&fill.to_string(), x, y, 2.0, 7.0, 11.0, inverse, true)
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(transform(6.0, 8.0, false), json!({"x": 253.0, "y": 119.0}));
    assert_eq!(transform(253.0, 119.0, true), json!({"x": 6.0, "y": 8.0}));
    let before_override = snapshot(&session);
    let at_breakpoint: Value = serde_json::from_str(
        &session
            .transform_point_at_breakpoint_json(
                &fill.to_string(),
                6.0,
                8.0,
                2.0,
                7.0,
                11.0,
                false,
                true,
                600.0,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(at_breakpoint, json!({"x": 353.0, "y": 119.0}));
    assert_eq!(snapshot(&session), before_override);
    let tx = transaction(
        &session,
        &[root, fill],
        json!([{"type": "moveNode", "nodeId": fill, "parentId": root, "index": 0}]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    assert_eq!(transform(6.0, 8.0, false), json!({"x": 33.0, "y": 119.0}));
    let after = snapshot(&session);
    assert_eq!(
        after["document"]["nodes"][2]["layout"]["x"].as_f64(),
        Some(500.0)
    );
    assert_eq!(
        after["document"]["nodes"][2]["sourceBounds"]["x"].as_f64(),
        Some(800.0)
    );
    assert!(session
        .transform_point_json(&fill.to_string(), 0.0, 0.0, 0.0, 0.0, 0.0, false, false)
        .is_err());
}

#[test]
fn explicit_breakpoint_transforms_nested_free_layout_without_mutating_it() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Responsive").unwrap();
    let root = root(&session);
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let mut parent_node = node(parent, root, "group");
    parent_node["layout"]["x"] = json!(10);
    parent_node["layout"]["y"] = json!(20);
    parent_node["responsive"] = json!([{"breakpoint": 600, "x": 40}]);
    let mut child_node = node(child, parent, "text");
    child_node["layout"]["x"] = json!(3);
    child_node["layout"]["y"] = json!(4);
    child_node["responsive"] = json!([{"breakpoint": 600, "x": 8}]);
    let tx = transaction(
        &session,
        &[root, parent, child],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": parent_node},
            {"type": "insertNode", "parentId": parent, "index": 0, "node": child_node}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let at = |width| -> Value {
        serde_json::from_str(
            &session
                .transform_point_at_breakpoint_json(
                    &child.to_string(),
                    1.0,
                    2.0,
                    2.0,
                    5.0,
                    7.0,
                    false,
                    false,
                    width,
                )
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(at(600.0), json!({"x": 103.0, "y": 59.0}));
    assert_eq!(at(601.0), json!({"x": 33.0, "y": 59.0}));
    assert_eq!(snapshot(&session), before);
}

#[test]
fn hug_flow_uses_measured_text_dimensions() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Hug").unwrap();
    let font_path = temp.path().join("ahem.ttf");
    std::fs::write(&font_path, font_test_data::AHEM).unwrap();
    let font: Value = serde_json::from_str(&session.import_asset(&font_path).unwrap()).unwrap();
    let root = root(&session);
    let group = Uuid::new_v4();
    let text = Uuid::new_v4();
    let second = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut group_node = node(group, root, "group");
    group_node["layout"] = json!({
        "x": 0, "y": 0, "w": 100, "h": 100, "widthMode": "hug", "heightMode": "hug",
        "flow": {"direction": "column", "gap": 5,
            "padding": {"top": 5, "right": 5, "bottom": 5, "left": 5}, "align": "start"}
    });
    let mut text_node = node(text, group, "text");
    text_node["layout"]["w"] = json!(70);
    text_node["layout"]["h"] = json!(20);
    text_node["content"]["text"] = json!("Hello");
    text_node["style"] = json!({"fontAssetId": font["id"], "fontSize": 16});
    let mut second_node = node(second, group, "group");
    second_node["layout"]["w"] = json!(40);
    second_node["layout"]["h"] = json!(30);
    let tx = transaction(
        &session,
        &[root, group, text, second, marker],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/flow", "value": {
                "direction": "row", "gap": 0,
                "padding": {"top": 10, "right": 10, "bottom": 10, "left": 10}, "align": "start"
            }},
            {"type": "insertNode", "parentId": root, "index": 0, "node": group_node},
            {"type": "insertNode", "parentId": group, "index": 0, "node": text_node},
            {"type": "insertNode", "parentId": group, "index": 1, "node": second_node},
            {"type": "insertNode", "parentId": root, "index": 1, "node": node(marker, root, "group")}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let mapped: Value = serde_json::from_str(
        &session
            .transform_point_json(&second.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(mapped, json!({"x": 15.0, "y": 40.0}));
    let tx = transaction(
        &session,
        &[text],
        json!([
            {"type": "setProperty", "nodeId": text, "path": "/layout/widthMode", "value": "hug"}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let marker: Value = serde_json::from_str(
        &session
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(marker, json!({"x": 100.0, "y": 10.0}));
}

#[test]
fn visual_baseline_tracks_page_and_asset_and_rejects_missing_references() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Baseline").unwrap());
    let image = b"synthetic baseline image";
    let hash = format!("{:x}", Sha256::digest(image));
    let asset_path = format!("assets/{hash}.png");
    std::fs::write(path.join(&asset_path), image).unwrap();
    let manifest_path = path.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"][asset_path.as_str()] = json!(hash);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let asset = Uuid::new_v4();
    let baseline = Uuid::new_v4();
    let page = doc["pages"][0]["id"].clone();
    doc["assets"] = json!([{"id": asset, "path": asset_path}]);
    doc["referenceBaselines"] = json!([{"id": baseline, "pageId": page, "assetId": asset}]);
    write_document_fixture(&path, &doc);
    let session = open_project(&path).unwrap();
    assert_eq!(snapshot(&session)["readOnly"], false);
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(
        snapshot(&reopened)["document"]["referenceBaselines"],
        doc["referenceBaselines"]
    );
    drop(reopened);

    doc["referenceBaselines"][0]["assetId"] = json!(Uuid::new_v4());
    write_document_fixture(&path, &doc);
    let bytes = std::fs::read(path.join("document.json")).unwrap();
    let error = open_project(&path).err().unwrap();
    assert_eq!(error.code, "MISSING_ASSET");
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), bytes);
}

#[test]
fn ordinary_node_types_accept_only_applicable_content_and_remain_atomic() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Node types").unwrap();
    let root = root(&session);
    let kinds = [
        "frame", "group", "text", "image", "vector", "button", "input", "textarea", "checkbox",
        "select",
    ];
    let ids: Vec<_> = kinds.iter().map(|_| Uuid::new_v4()).collect();
    let mut scope = vec![root];
    scope.extend(ids.iter().copied());
    let operations: Vec<_> = kinds.iter().zip(&ids).enumerate().map(|(index, (kind, id))| {
        json!({"type": "insertNode", "parentId": root, "index": index, "node": node(*id, root, kind)})
    }).collect();
    let tx = transaction(&session, &scope, json!(operations));
    session.apply_transaction(&tx.to_string()).unwrap();
    let before = snapshot(&session);
    let tx = transaction(
        &session,
        &[ids[9]],
        json!([
            {"type": "setProperty", "nodeId": ids[9], "path": "/content/text", "value": "invalid"}
        ]),
    );
    assert!(session.apply_transaction(&tx.to_string()).is_err());
    assert_eq!(snapshot(&session), before);
    let tx = transaction(
        &session,
        &[ids[2], ids[6]],
        json!([
            {"type": "setProperty", "nodeId": ids[2], "path": "/content/text", "value": "Title"},
            {"type": "setProperty", "nodeId": ids[6], "path": "/content/placeholder", "value": "Type here"}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let after = snapshot(&session);
    assert_eq!(after["document"]["nodes"][3]["content"]["text"], "Title");
    assert_eq!(
        after["document"]["nodes"][7]["content"]["placeholder"],
        "Type here"
    );
    assert_eq!(
        after["document"]["nodes"][3]["propertyMeta"]["/content/text"]["origin"],
        "user"
    );
}

#[test]
fn column_flow_supports_end_and_stretch_alignment() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Column").unwrap();
    let root = root(&session);
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut first_node = node(first, root, "group");
    first_node["layout"] = json!({"x": 0, "y": 0, "w": 30, "h": 40});
    let mut second_node = node(second, root, "group");
    second_node["layout"] = json!({"x": 0, "y": 0, "w": 50, "h": 20, "heightMode": "fill"});
    let end_flow = json!({"direction": "column", "gap": 10,
        "padding": {"top": 10, "right": 10, "bottom": 10, "left": 10}, "align": "end"});
    let tx = transaction(
        &session,
        &[root, first, second],
        json!([
            {"type": "setViewport", "nodeId": root, "width": 200, "height": 200},
            {"type": "setProperty", "nodeId": root, "path": "/layout/flow", "value": end_flow},
            {"type": "insertNode", "parentId": root, "index": 0, "node": first_node},
            {"type": "insertNode", "parentId": root, "index": 1, "node": second_node}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let point = || -> Value {
        serde_json::from_str(
            &session
                .transform_point_json(&second.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(point(), json!({"x": 140.0, "y": 60.0}));
    let tx = transaction(
        &session,
        &[root],
        json!([{
            "type": "setProperty", "nodeId": root, "path": "/layout/flow",
            "value": {"direction": "column", "gap": 10,
                "padding": {"top": 10, "right": 10, "bottom": 10, "left": 10}, "align": "stretch"}
        }]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    assert_eq!(point(), json!({"x": 10.0, "y": 60.0}));
}

struct TextFlowFixture {
    _temp: tempfile::TempDir,
    session: ProjectSession,
    root: Uuid,
    text: Uuid,
    marker: Uuid,
    font_path: std::path::PathBuf,
}

impl TextFlowFixture {
    fn new(direction: &str, width: f64, text_layout: Value, text_value: &str) -> Self {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let session = create_project(&project, "Text flow").unwrap();
        let source = temp.path().join("ahem.ttf");
        std::fs::write(&source, font_test_data::AHEM).unwrap();
        let font: Value = serde_json::from_str(&session.import_asset(&source).unwrap()).unwrap();
        let font_path = project.join(font["path"].as_str().unwrap());
        let root = root(&session);
        let text = Uuid::new_v4();
        let marker = Uuid::new_v4();
        let mut text_node = node(text, root, "text");
        text_node["layout"] = text_layout;
        text_node["content"]["text"] = json!(text_value);
        text_node["style"] = json!({"fontAssetId": font["id"], "fontSize": 16, "lineHeight": 20});
        let mut marker_node = node(marker, root, "group");
        marker_node["layout"]["w"] = json!(16);
        marker_node["layout"]["h"] = json!(10);
        marker_node["sourceDensity"] = json!(2);
        let tx = transaction(
            &session,
            &[root, text, marker],
            json!([
                {"type": "setViewport", "nodeId": root, "width": width, "height": 100},
                {"type": "setProperty", "nodeId": root, "path": "/layout/flow", "value": {
                    "direction": direction, "gap": 0,
                    "padding": {"top": 0, "right": 0, "bottom": 0, "left": 0}, "align": "start"
                }},
                {"type": "insertNode", "parentId": root, "index": 0, "node": text_node},
                {"type": "insertNode", "parentId": root, "index": 1, "node": marker_node}
            ]),
        );
        session.apply_transaction(&tx.to_string()).unwrap();
        Self {
            _temp: temp,
            session,
            root,
            text,
            marker,
            font_path,
        }
    }

    fn set(&self, id: Uuid, path: &str, value: Value) {
        let tx = transaction(
            &self.session,
            &[id],
            json!([
                {"type": "setProperty", "nodeId": id, "path": path, "value": value}
            ]),
        );
        self.session.apply_transaction(&tx.to_string()).unwrap();
    }

    fn point(&self, id: Uuid) -> Value {
        serde_json::from_str(
            &self
                .session
                .transform_point_json(&id.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
                .unwrap(),
        )
        .unwrap()
    }
}

#[test]
fn text_hug_shapes_unicode_multiline_styles_and_viewport_scale() {
    let f = TextFlowFixture::new(
        "row",
        400.0,
        json!({
            "x": 0, "y": 0, "w": 100, "h": 20, "widthMode": "hug"
        }),
        "一二三",
    );
    assert_eq!(f.point(f.marker)["x"], 48.0);
    f.set(f.text, "/content/text", json!("AB\nC"));
    assert_eq!(f.point(f.marker)["x"], 32.0);
    for separator in ["\r\n", "\u{85}", "\u{2028}", "\u{2029}"] {
        f.set(f.text, "/content/text", json!(format!("AB{separator}C")));
        assert_eq!(f.point(f.marker)["x"], 32.0, "separator {separator:?}");
    }
    f.set(f.text, "/content/text", json!("AB\nC"));
    let scaled: Value = serde_json::from_str(
        &f.session
            .transform_point_json(&f.marker.to_string(), 4.0, 8.0, 2.0, 7.0, 11.0, false, true)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(scaled, json!({"x": 75.0, "y": 19.0}));
    f.set(f.text, "/style/fontSize", json!(24));
    assert_eq!(f.point(f.marker)["x"], 48.0);
    f.set(f.text, "/style/letterSpacing", json!(2));
    assert!(f.point(f.marker)["x"].as_f64().unwrap() > 48.0);
}

#[test]
fn text_hug_height_wraps_to_fixed_fill_and_breakpoint_width() {
    let f = TextFlowFixture::new(
        "column",
        32.0,
        json!({
            "x": 0, "y": 0, "w": 32, "h": 20, "widthMode": "fixed", "heightMode": "hug"
        }),
        "AAAAAA",
    );
    assert_eq!(f.point(f.marker)["y"], 60.0);
    f.set(f.text, "/content/text", json!("AA\n"));
    assert_eq!(f.point(f.marker)["y"], 40.0);
    f.set(f.text, "/content/text", json!("AA\r\n"));
    assert_eq!(f.point(f.marker)["y"], 40.0);
    f.set(f.text, "/content/text", json!("\n\r"));
    assert_eq!(f.point(f.marker)["y"], 60.0);
    f.set(f.text, "/content/text", json!(""));
    assert_eq!(f.point(f.marker)["y"], 20.0);
    f.set(f.text, "/content/text", json!("A\u{a0}A"));
    f.set(f.text, "/style/textWrap", json!("word"));
    assert_eq!(f.point(f.marker)["y"], 20.0);
    f.set(f.text, "/content/text", json!("A A"));
    assert_eq!(f.point(f.marker)["y"], 40.0);
    f.set(f.text, "/style/textWrap", json!("wordOrGlyph"));
    f.set(f.text, "/content/text", json!("AAAAAA"));
    f.set(f.text, "/style/textWrap", json!("none"));
    assert_eq!(f.point(f.marker)["y"], 20.0);
    f.set(f.text, "/style/textWrap", json!("wordOrGlyph"));
    f.set(f.text, "/responsive", json!([{"breakpoint": 600, "w": 16}]));
    let point: Value = serde_json::from_str(
        &f.session
            .transform_point_at_breakpoint_json(
                &f.marker.to_string(),
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                false,
                false,
                600.0,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(point["y"], 120.0);
    assert_eq!(f.point(f.marker)["y"], 60.0);
    f.set(f.text, "/layout/widthMode", json!("fill"));
    assert_eq!(f.point(f.marker)["y"], 60.0);
    let tx = transaction(
        &f.session,
        &[f.root],
        json!([
            {"type": "setViewport", "nodeId": f.root, "width": 48, "height": 100}
        ]),
    );
    f.session.apply_transaction(&tx.to_string()).unwrap();
    assert_eq!(f.point(f.marker)["y"], 40.0);
}

#[test]
fn text_hug_height_uses_remaining_row_width_before_alignment() {
    let f = TextFlowFixture::new(
        "row",
        80.0,
        json!({
            "x": 0, "y": 0, "w": 1, "h": 20, "widthMode": "fill", "heightMode": "hug"
        }),
        "AAAAAA",
    );
    f.set(
        f.root,
        "/layout/flow",
        json!({
            "direction": "row", "gap": 0,
            "padding": {"top": 0, "right": 0, "bottom": 0, "left": 0}, "align": "end"
        }),
    );
    assert_eq!(f.point(f.text), json!({"x": 0.0, "y": 60.0}));
    assert_eq!(f.point(f.marker), json!({"x": 64.0, "y": 90.0}));
}

#[test]
fn text_hug_reports_missing_fonts_glyphs_and_changed_assets() {
    let f = TextFlowFixture::new(
        "row",
        400.0,
        json!({
            "x": 0, "y": 0, "w": 100, "h": 20, "widthMode": "hug"
        }),
        "A",
    );
    let error = || {
        f.session
            .transform_point_json(&f.marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap_err()
            .code
    };
    f.set(
        f.text,
        "/style/fontFamily",
        json!("Font that does not exist"),
    );
    assert_eq!(error(), "MISSING_FONT");
    f.set(
        f.text,
        "/style/fontFamily",
        json!("/System/Library/Fonts/Supplemental/Arial.ttf"),
    );
    assert_eq!(error(), "MISSING_FONT");
    f.set(f.text, "/style/fontFamily", Value::Null);
    f.set(
        f.text,
        "/style/fontFallback",
        json!("Font that does not exist"),
    );
    assert_eq!(error(), "MISSING_FONT");
    f.set(f.text, "/style/fontFallback", Value::Null);
    f.set(f.text, "/content/text", json!("Ж"));
    assert_eq!(error(), "MISSING_GLYPH");
    f.set(f.text, "/content/text", json!("A"));
    f.set(f.text, "/style/fontWeight", json!(700));
    assert_eq!(error(), "MISSING_FONT");
    f.set(f.text, "/style/fontWeight", Value::Null);
    let before = snapshot(&f.session);
    let bad = transaction(
        &f.session,
        &[f.text],
        json!([
            {"type": "setProperty", "nodeId": f.text, "path": "/style/fontSize", "value": -1}
        ]),
    );
    assert!(f.session.apply_transaction(&bad.to_string()).is_err());
    assert_eq!(snapshot(&f.session), before);
    let svg = f._temp.path().join("image.svg");
    std::fs::write(&svg, br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).unwrap();
    let image: Value = serde_json::from_str(&f.session.import_asset(&svg).unwrap()).unwrap();
    let before = snapshot(&f.session);
    let font_id = before["document"]["assets"][0]["id"].as_str().unwrap();
    let bad = transaction(
        &f.session,
        &[f.text],
        json!([
            {"type": "setProperty", "nodeId": f.text, "path": "/style/fontAssetId", "value": image["id"]}
        ]),
    );
    assert!(f.session.apply_transaction(&bad.to_string()).is_err());
    assert!(f.session.relink_asset(font_id, &svg).is_err());
    assert_eq!(snapshot(&f.session), before);
    std::fs::write(&f.font_path, b"external change").unwrap();
    assert_eq!(error(), "REVISION_CONFLICT");
    std::fs::remove_file(&f.font_path).unwrap();
    assert_eq!(error(), "MISSING_ASSET");
}

#[test]
fn text_hug_shapes_rtl_and_emoji_clusters_from_project_fonts() {
    let f = TextFlowFixture::new(
        "row",
        400.0,
        json!({
            "x": 0, "y": 0, "w": 100, "h": 20, "widthMode": "hug"
        }),
        "A",
    );
    let hebrew = f._temp.path().join("hebrew.ttf");
    std::fs::write(&hebrew, font_test_data::NOTOSERIFHEBREW_AUTOHINT_METRICS).unwrap();
    let font: Value = serde_json::from_str(&f.session.import_asset(&hebrew).unwrap()).unwrap();
    f.set(f.text, "/style/fontAssetId", font["id"].clone());
    f.set(f.text, "/content/text", json!("CHEהבד"));
    let width = f.point(f.marker)["x"].as_f64().unwrap();
    assert!(width > 0.0);
    assert_eq!(width, f.point(f.marker)["x"].as_f64().unwrap());
    let emoji = f._temp.path().join("emoji.ttf");
    std::fs::write(&emoji, font_test_data::NOTO_COLOR_EMOJI_FLAGS).unwrap();
    let font: Value = serde_json::from_str(&f.session.import_asset(&emoji).unwrap()).unwrap();
    f.set(f.text, "/style/fontAssetId", font["id"].clone());
    f.set(f.text, "/content/text", json!("🇪🇨"));
    let width = f.point(f.marker)["x"].as_f64().unwrap();
    assert!(width > 0.0);
    f.set(f.text, "/content/text", json!("🇪🇨🇪🇨"));
    let doubled = f.point(f.marker)["x"].as_f64().unwrap();
    assert!((doubled - 2.0 * width).abs() < 0.01);
}

#[cfg(target_os = "macos")]
#[test]
fn text_hug_uses_real_installed_cyrillic_and_bold_italic_fonts() {
    let f = TextFlowFixture::new(
        "row",
        400.0,
        json!({
            "x": 0, "y": 0, "w": 100, "h": 20, "widthMode": "hug"
        }),
        "A",
    );
    f.set(f.text, "/style/fontAssetId", Value::Null);
    f.set(f.text, "/style/fontFamily", json!("Arial"));
    f.set(f.text, "/content/text", json!("Привет"));
    assert!(f.point(f.marker)["x"].as_f64().unwrap() > 0.0);
    f.set(f.text, "/content/text", json!("Привет🌍"));
    assert_eq!(
        f.session
            .transform_point_json(&f.marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap_err()
            .code,
        "MISSING_GLYPH"
    );
    f.set(f.text, "/style/fontFallback", json!("Apple Color Emoji"));
    assert!(f.point(f.marker)["x"].as_f64().unwrap() > 0.0);
    f.set(f.text, "/style/fontFallback", Value::Null);
    f.set(f.text, "/content/text", json!("é"));
    let composed = f.point(f.marker)["x"].as_f64().unwrap();
    f.set(f.text, "/content/text", json!("e\u{301}"));
    assert!((f.point(f.marker)["x"].as_f64().unwrap() - composed).abs() < 0.01);
    f.set(f.text, "/content/text", json!("Hello"));
    let regular = f.point(f.marker)["x"].as_f64().unwrap();
    f.set(f.text, "/style/fontWeight", json!(700));
    f.set(f.text, "/style/fontStyle", json!("italic"));
    let styled = f.point(f.marker)["x"].as_f64().unwrap();
    assert!(styled > regular);
}

#[test]
fn text_hug_variable_font_weight_is_measured_and_survives_reopen() {
    let f = TextFlowFixture::new(
        "row",
        400.0,
        json!({"x": 0, "y": 0, "w": 100, "h": 20, "widthMode": "hug"}),
        "A",
    );
    let source = f._temp.path().join("cantarell.ttf");
    std::fs::write(&source, font_test_data::CANTARELL_VF_TRIMMED).unwrap();
    let font: Value = serde_json::from_str(&f.session.import_asset(&source).unwrap()).unwrap();
    f.set(f.text, "/style/fontAssetId", font["id"].clone());
    f.set(f.text, "/content/text", json!("iiii"));
    let regular = f.point(f.marker)["x"].as_f64().unwrap();
    f.set(f.text, "/style/fontWeight", json!(700));
    let bold = f.point(f.marker)["x"].as_f64().unwrap();
    assert!(regular > 0.0 && bold > regular);
    f.session.save().unwrap();
    let reopened = open_project(f._temp.path().join("project")).unwrap();
    let point: Value = serde_json::from_str(
        &reopened
            .transform_point_json(&f.marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(point["x"].as_f64().unwrap(), bold);
}

#[cfg(target_os = "macos")]
#[test]
fn text_hug_collection_requires_a_family_and_never_uses_unselected_faces() {
    // Package two real fixture fonts as a TTC, adjusting SFNT table offsets.
    let mut collection = b"ttcf\0\x01\0\0\0\0\0\x02".to_vec();
    collection.resize(20, 0);
    for (index, data) in [font_test_data::AHEM, font_test_data::NOTO_COLOR_EMOJI_FLAGS]
        .into_iter()
        .enumerate()
    {
        let offset = collection.len().next_multiple_of(4);
        collection.resize(offset, 0);
        collection[12 + index * 4..16 + index * 4].copy_from_slice(&(offset as u32).to_be_bytes());
        collection.extend_from_slice(data);
        let tables = usize::from(u16::from_be_bytes([data[4], data[5]]));
        for table in 0..tables {
            let pos = offset + 12 + table * 16 + 8;
            let old = u32::from_be_bytes(collection[pos..pos + 4].try_into().unwrap());
            collection[pos..pos + 4].copy_from_slice(&(old + offset as u32).to_be_bytes());
        }
    }
    let f = TextFlowFixture::new(
        "row",
        400.0,
        json!({"x": 0, "y": 0, "w": 100, "h": 20, "widthMode": "hug"}),
        "A",
    );
    let source = f._temp.path().join("collection.ttc");
    std::fs::write(&source, collection).unwrap();
    let font: Value = serde_json::from_str(&f.session.import_asset(&source).unwrap()).unwrap();
    f.set(f.text, "/style/fontAssetId", font["id"].clone());
    let error = || {
        f.session
            .transform_point_json(&f.marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap_err()
            .code
    };
    assert_eq!(error(), "INVALID_DOCUMENT");
    f.set(f.text, "/style/fontFamily", json!("Ahem"));
    assert_eq!(f.point(f.marker)["x"], 16.0);
    f.set(f.text, "/style/fontFallback", json!("Arial"));
    f.set(f.text, "/content/text", json!("🇪🇨"));
    assert_eq!(error(), "MISSING_GLYPH");
}

#[test]
fn user_edits_are_protected_until_explicitly_unlocked() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "User protection").unwrap();
    let root = root(&session);
    let edit = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "User"}]),
    );
    session.apply_transaction(&edit.to_string()).unwrap();
    session.save().unwrap();
    drop(session);

    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    assert_eq!(
        before["document"]["nodes"][0]["propertyMeta"]["/name"]["agentLocked"],
        true
    );
    let mut agent = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Agent"}]),
    );
    agent["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);

    let unlock = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/propertyMeta/~1name/agentLocked", "value": false}]),
    );
    session.apply_transaction(&unlock.to_string()).unwrap();
    let mut agent = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Agent"}]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    assert_eq!(snapshot(&session)["document"]["nodes"][0]["name"], "Agent");
}

#[test]
fn undo_and_redo_of_user_edits_keep_restored_values_protected() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Undo protection").unwrap();
    let root = root(&session);
    let edit = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "User"}]),
    );
    session.apply_transaction(&edit.to_string()).unwrap();
    for redo in [false, true] {
        if redo {
            session.redo().unwrap();
        } else {
            session.undo().unwrap();
        }
        let before = snapshot(&session);
        let mut agent = transaction(
            &session,
            &[root],
            json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Agent"}]),
        );
        agent["origin"] = json!("agent");
        assert_eq!(
            session
                .apply_transaction(&agent.to_string())
                .unwrap_err()
                .code,
            "LOCKED_PROPERTY"
        );
        assert_eq!(snapshot(&session), before);
    }
}

#[test]
fn vector_primitives_round_trip_and_invalid_geometry_is_atomic() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Vector primitives").unwrap();
    let root = root(&session);
    let vector_id = Uuid::new_v4();
    let shapes = json!([
        {"type": "rect", "x": 1.0, "y": 2.0, "w": 30.0, "h": 40.0, "rx": 2.0, "ry": 3.0},
        {"type": "circle", "cx": 5.0, "cy": 6.0, "r": 4.0},
        {"type": "ellipse", "cx": 7.0, "cy": 8.0, "rx": 4.0, "ry": 2.0},
        {"type": "line", "x1": 0.0, "y1": 1.0, "x2": 3.0, "y2": 4.0},
        {"type": "polyline", "points": [{"x": 0.0, "y": 1.0}, {"x": 2.0, "y": 3.0}]},
        {"type": "polygon", "points": [{"x": 0.0, "y": 0.0}, {"x": 3.0, "y": 0.0}, {"x": 1.0, "y": 2.0}]}
    ]);
    let mut vector = node(vector_id, root, "vector");
    vector["content"]["vector"] = json!({"paths": [[{"type": "moveTo", "x": 0, "y": 0},
        {"type": "lineTo", "x": 1, "y": 1}]], "shapes": shapes.clone()});
    let insert = transaction(
        &session,
        &[root, vector_id],
        json!([{"type": "insertNode", "parentId": root, "index": 0, "node": vector}]),
    );
    session.apply_transaction(&insert.to_string()).unwrap();
    session.save().unwrap();
    drop(session);

    let session = open_project(&path).unwrap();
    let before = snapshot(&session);
    assert_eq!(
        before["document"]["nodes"][1]["content"]["vector"]["shapes"],
        shapes
    );
    let invalid = transaction(
        &session,
        &[vector_id],
        json!([{"type": "setProperty", "nodeId": vector_id, "path": "/content/vector",
            "value": {"paths": [], "shapes": [{"type": "rect", "x": 0, "y": 0, "w": -1, "h": 1}]}}]),
    );
    assert_eq!(
        session
            .apply_transaction(&invalid.to_string())
            .unwrap_err()
            .code,
        "INVALID_DOCUMENT"
    );
    assert_eq!(snapshot(&session), before);
}

#[test]
fn unknown_vector_shapes_remain_read_only_and_are_not_rewritten() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Unknown vector").unwrap();
    let mut document = snapshot(&session)["document"].clone();
    let root = root(&session);
    drop(session);
    let vector_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut vector = node(vector_id, root, "vector");
    vector["layout"]["widthMode"] = json!("hug");
    vector["content"]["vector"] = json!({
        "paths": [],
        "shapes": [{"type": "futureStar", "points": [1, 2, 3], "extra": {"keep": true}}]
    });
    document["nodes"][0]["layout"]["flow"] = json!({
        "direction":"row","gap":0,
        "padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"
    });
    document["nodes"][0]["children"] = json!([vector_id, marker]);
    document["nodes"].as_array_mut().unwrap().push(vector);
    document["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node(marker, root, "group"));
    write_document_fixture(&path, &document);
    let original = std::fs::read(path.join("document.json")).unwrap();
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], true);
    assert_eq!(snapshot(&reopened)["document"], document);
    assert_eq!(
        reopened
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false,)
            .unwrap_err()
            .code,
        "UNSUPPORTED_FEATURE"
    );
    assert!(reopened.save().is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), original);
}

#[test]
fn unknown_polygon_point_fields_remain_read_only_and_are_not_rewritten() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Extended vector").unwrap();
    let mut document = snapshot(&session)["document"].clone();
    let root = root(&session);
    drop(session);
    let vector_id = Uuid::new_v4();
    let mut vector = node(vector_id, root, "vector");
    vector["content"]["vector"] = json!({
        "paths": [],
        "shapes": [{"type": "polygon", "points": [
            {"x": 0, "y": 0, "futurePressure": 0.5}, {"x": 1, "y": 0}, {"x": 0, "y": 1}
        ]}]
    });
    document["nodes"][0]["children"] = json!([vector_id]);
    document["nodes"].as_array_mut().unwrap().push(vector);
    write_document_fixture(&path, &document);
    let original = std::fs::read(path.join("document.json")).unwrap();
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], true);
    assert_eq!(snapshot(&reopened)["document"], document);
    assert!(reopened.save().is_err());
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), original);
}

#[test]
fn undo_restores_an_explicit_ancestor_lock_alongside_child_edits() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Ancestor lock").unwrap();
    let root = root(&session);
    let lock = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root,
            "path": "/propertyMeta/~1layout/agentLocked", "value": true}]),
    );
    session.apply_transaction(&lock.to_string()).unwrap();
    let unlock_and_edit = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root,
                "path": "/propertyMeta/~1layout/agentLocked", "value": false},
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 700}
        ]),
    );
    session
        .apply_transaction(&unlock_and_edit.to_string())
        .unwrap();
    session.undo().unwrap();
    let before = snapshot(&session);
    assert_eq!(
        before["document"]["nodes"][0]["propertyMeta"]["/layout"]["agentLocked"],
        true
    );
    let mut agent = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/layout/h", "value": 700}]),
    );
    agent["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
}

#[test]
fn redo_restores_an_explicit_unlock_in_the_same_batch_as_a_value_edit() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Explicit unlock").unwrap();
    let root = root(&session);
    let edit_and_unlock = transaction(
        &session,
        &[root],
        json!([
            {"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 700},
            {"type": "setProperty", "nodeId": root,
                "path": "/propertyMeta/~1layout~1w/agentLocked", "value": false}
        ]),
    );
    session
        .apply_transaction(&edit_and_unlock.to_string())
        .unwrap();
    session.undo().unwrap();
    session.redo().unwrap();
    let after = snapshot(&session);
    assert_eq!(
        after["document"]["nodes"][0]["propertyMeta"]["/layout/w"]["agentLocked"],
        false
    );
    let mut agent = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/layout/w", "value": 800}]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    assert_eq!(
        snapshot(&session)["document"]["nodes"][0]["layout"]["w"],
        800.0
    );
}

#[test]
fn undo_of_user_edit_protects_an_earlier_agent_value() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Undo agent value").unwrap();
    let root = root(&session);
    let mut agent = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Agent"}]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    let user = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "User"}]),
    );
    session.apply_transaction(&user.to_string()).unwrap();
    session.undo().unwrap();
    let before = snapshot(&session);
    assert_eq!(before["document"]["nodes"][0]["name"], "Agent");
    assert_eq!(
        before["document"]["nodes"][0]["propertyMeta"]["/name"]["agentLocked"],
        true
    );
    let mut agent = transaction(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/name", "value": "Overwrite"}]),
    );
    agent["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
}

#[test]
fn raster_hug_uses_intrinsic_ratio_and_explicit_breakpoints() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let source = temp.path().join("image.png");
    image::RgbaImage::new(40, 20).save(&source).unwrap();
    let session = create_project(&path, "Image hug").unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&source).unwrap()).unwrap();
    let root = root(&session);
    let image_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut image = node(image_id, root, "image");
    image["content"]["assetId"] = asset["id"].clone();
    image["layout"] = json!({
        "x": 0, "y": 0, "w": 100, "h": 50, "widthMode": "hug", "heightMode": "hug"
    });
    image["responsive"] = json!([{"breakpoint": 500, "w": 80}]);
    let row = json!({"direction":"row","gap":0,
        "padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"});
    let insert = transaction(
        &session,
        &[root, image_id, marker],
        json!([
            {"type":"setViewport","nodeId":root,"width":300,"height":100},
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":row},
            {"type":"insertNode","parentId":root,"index":0,"node":image},
            {"type":"insertNode","parentId":root,"index":1,"node":node(marker, root, "group")}
        ]),
    );
    session.apply_transaction(&insert.to_string()).unwrap();
    let point = |breakpoint: Option<f64>| -> Value {
        let value = if let Some(width) = breakpoint {
            session.transform_point_at_breakpoint_json(
                &marker.to_string(),
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                false,
                false,
                width,
            )
        } else {
            session.transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
        };
        serde_json::from_str(&value.unwrap()).unwrap()
    };
    assert_eq!(point(None)["x"], 40.0);
    assert_eq!(point(Some(500.0))["x"], 80.0);
    assert_eq!(point(None)["x"], 40.0);
    let fixed = transaction(
        &session,
        &[image_id],
        json!([
            {"type":"setProperty","nodeId":image_id,"path":"/layout/heightMode","value":"fixed"}
        ]),
    );
    session.apply_transaction(&fixed.to_string()).unwrap();
    assert_eq!(point(None)["x"], 100.0);
    let height = Uuid::new_v4();
    let bound = transaction(
        &session,
        &[image_id],
        json!([
            {"type":"setToken","token":{"id":height,"name":"Height","value":100}},
            {"type":"setBinding","nodeId":image_id,"path":"/layout/h","binding":{"type":"token","tokenId":height}}
        ]),
    );
    session.apply_transaction(&bound.to_string()).unwrap();
    assert_eq!(point(None)["x"], 200.0);
    let ambiguous = transaction(
        &session,
        &[image_id],
        json!([
            {"type":"setBinding","nodeId":image_id,"path":"/layout/w","binding":{"type":"token","tokenId":height}}
        ]),
    );
    session.apply_transaction(&ambiguous.to_string()).unwrap();
    assert_eq!(
        session
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap_err()
            .code,
        "UNSUPPORTED_FEATURE"
    );
    let unbind = transaction(
        &session,
        &[image_id],
        json!([
            {"type":"setBinding","nodeId":image_id,"path":"/layout/w","binding":null},
            {"type":"setBinding","nodeId":image_id,"path":"/layout/h","binding":null}
        ]),
    );
    session.apply_transaction(&unbind.to_string()).unwrap();
    let fill = transaction(
        &session,
        &[image_id],
        json!([
            {"type":"setProperty","nodeId":image_id,"path":"/layout/heightMode","value":"fill"}
        ]),
    );
    session.apply_transaction(&fill.to_string()).unwrap();
    assert_eq!(point(None)["x"], 200.0);
    let column = transaction(
        &session,
        &[root, image_id],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":{
                "direction":"column","gap":0,
                "padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"}},
            {"type":"setProperty","nodeId":image_id,"path":"/layout/widthMode","value":"fill"},
            {"type":"setProperty","nodeId":image_id,"path":"/layout/heightMode","value":"hug"}
        ]),
    );
    session.apply_transaction(&column.to_string()).unwrap();
    assert_eq!(point(None)["y"], 150.0);
    std::fs::write(
        path.join(asset["path"].as_str().unwrap()),
        b"external change",
    )
    .unwrap();
    assert_eq!(
        session
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap_err()
            .code,
        "REVISION_CONFLICT"
    );
}

#[test]
fn raster_hug_respects_exif_orientation() {
    use image::ImageEncoder;
    let temp = tempdir().unwrap();
    let source = temp.path().join("rotated.jpg");
    let mut jpeg = Vec::new();
    {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(&mut jpeg);
        encoder
            .set_exif_metadata(vec![
                b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0,
                0, 0, 0,
            ])
            .unwrap();
        encoder.encode_image(&image::RgbImage::new(40, 20)).unwrap();
    }
    std::fs::write(&source, jpeg).unwrap();
    let session = create_project(temp.path().join("project"), "Rotated image").unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&source).unwrap()).unwrap();
    let root = root(&session);
    let image_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut image = node(image_id, root, "image");
    image["content"]["assetId"] = asset["id"].clone();
    image["layout"]["widthMode"] = json!("hug");
    image["layout"]["heightMode"] = json!("hug");
    let insert = transaction(
        &session,
        &[root, image_id, marker],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":{
                "direction":"row","gap":0,"padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"}},
            {"type":"insertNode","parentId":root,"index":0,"node":image},
            {"type":"insertNode","parentId":root,"index":1,"node":node(marker, root, "group")}
        ]),
    );
    session.apply_transaction(&insert.to_string()).unwrap();
    let point: Value = serde_json::from_str(
        &session
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(point["x"], 20.0);
}

#[test]
fn cross_axis_hug_fill_cycle_is_reported_without_recursing_forever() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("image.png");
    image::RgbaImage::new(40, 20).save(&source).unwrap();
    let session = create_project(temp.path().join("project"), "Cyclic sizing").unwrap();
    let asset: Value = serde_json::from_str(&session.import_asset(&source).unwrap()).unwrap();
    let root = root(&session);
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut first_image = node(first, root, "image");
    first_image["content"]["assetId"] = asset["id"].clone();
    first_image["layout"]["widthMode"] = json!("hug");
    first_image["layout"]["heightMode"] = json!("fill");
    let mut second_image = node(second, root, "image");
    second_image["content"]["assetId"] = asset["id"].clone();
    second_image["layout"]["widthMode"] = json!("fill");
    second_image["layout"]["heightMode"] = json!("hug");
    let tx = transaction(
        &session,
        &[root, first, second],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/heightMode","value":"hug"},
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":{
                "direction":"row","gap":0,"padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"}},
            {"type":"insertNode","parentId":root,"index":0,"node":first_image},
            {"type":"insertNode","parentId":root,"index":1,"node":second_image}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let error = session
        .transform_point_json(&second.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
        .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_FEATURE");
}

#[test]
fn bound_layout_values_are_used_for_flow_and_breakpoint_geometry() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Bound geometry").unwrap();
    let root = root(&session);
    let first = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let token = Uuid::new_v4();
    let mut first_node = node(first, root, "group");
    first_node["responsive"] = json!([{"breakpoint": 500, "w": 50}]);
    let tx = transaction(
        &session,
        &[root, first, marker],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":{
                "direction":"row","gap":0,"padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"}},
            {"type":"insertNode","parentId":root,"index":0,"node":first_node},
            {"type":"insertNode","parentId":root,"index":1,"node":node(marker, root, "group")},
            {"type":"setToken","token":{"id":token,"name":"Width","value":123}},
            {"type":"setBinding","nodeId":first,"path":"/layout/w","binding":{"type":"token","tokenId":token}}
        ]),
    );
    session.apply_transaction(&tx.to_string()).unwrap();
    let point = |breakpoint: Option<f64>| -> Value {
        let result = if let Some(width) = breakpoint {
            session.transform_point_at_breakpoint_json(
                &marker.to_string(),
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                false,
                false,
                width,
            )
        } else {
            session.transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
        };
        serde_json::from_str(&result.unwrap()).unwrap()
    };
    assert_eq!(point(None)["x"], 123.0);
    assert_eq!(point(Some(500.0))["x"], 50.0);
    let update = transaction(
        &session,
        &[first],
        json!([
            {"type":"setToken","token":{"id":token,"name":"Width","value":80}}
        ]),
    );
    session.apply_transaction(&update.to_string()).unwrap();
    assert_eq!(point(None)["x"], 80.0);
    let invalid = transaction(
        &session,
        &[first],
        json!([
            {"type":"setToken","token":{"id":token,"name":"Width","value":-1}}
        ]),
    );
    let before_invalid = snapshot(&session);
    assert_eq!(
        session
            .apply_transaction(&invalid.to_string())
            .unwrap_err()
            .code,
        "INVALID_DOCUMENT"
    );
    assert_eq!(snapshot(&session), before_invalid);
    assert_eq!(point(None)["x"], 80.0);
    assert_eq!(point(Some(500.0))["x"], 50.0);
    let absolute = transaction(
        &session,
        &[root, first, marker],
        json!([
            {"type":"setToken","token":{"id":token,"name":"Width","value":80}},
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":null},
            {"type":"setBinding","nodeId":marker,"path":"/layout/x","binding":{"type":"token","tokenId":token}}
        ]),
    );
    session.apply_transaction(&absolute.to_string()).unwrap();
    assert_eq!(point(None)["x"], 80.0);
}

#[test]
fn undo_restores_explicit_unlock_when_both_history_states_were_unlocked() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Historical unlock").unwrap();
    let root = root(&session);
    for width in [700, 800] {
        let edit = transaction(
            &session,
            &[root],
            json!([
                {"type":"setProperty","nodeId":root,"path":"/layout/w","value":width},
                {"type":"setProperty","nodeId":root,"path":"/propertyMeta/~1layout~1w/agentLocked","value":false}
            ]),
        );
        session.apply_transaction(&edit.to_string()).unwrap();
    }
    session.undo().unwrap();
    let before = snapshot(&session);
    assert_eq!(before["document"]["nodes"][0]["layout"]["w"], 700.0);
    assert_eq!(
        before["document"]["nodes"][0]["propertyMeta"]["/layout/w"]["agentLocked"],
        false
    );
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/w","value":701}
        ]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
}

#[test]
fn undoing_user_removal_protects_restored_agent_node_unless_explicitly_unlocked() {
    for explicit_unlock in [false, true] {
        let temp = tempdir().unwrap();
        let session = create_project(temp.path().join("project"), "Restored node").unwrap();
        let root = root(&session);
        let child = Uuid::new_v4();
        let mut insert = transaction(
            &session,
            &[root, child],
            json!([
                {"type":"insertNode","parentId":root,"index":0,"node":node(child, root, "group")}
            ]),
        );
        insert["origin"] = json!("agent");
        session.apply_transaction(&insert.to_string()).unwrap();
        if explicit_unlock {
            let unlock = transaction(
                &session,
                &[child],
                json!([
                    {"type":"setProperty","nodeId":child,"path":"/propertyMeta/~1name/agentLocked","value":false}
                ]),
            );
            session.apply_transaction(&unlock.to_string()).unwrap();
        }
        let remove = transaction(
            &session,
            &[root, child],
            json!([
                {"type":"removeNode","nodeId":child}
            ]),
        );
        session.apply_transaction(&remove.to_string()).unwrap();
        session.undo().unwrap();
        let before = snapshot(&session);
        let mut agent = transaction(
            &session,
            &[child],
            json!([
                {"type":"setProperty","nodeId":child,"path":"/name","value":"Agent override"}
            ]),
        );
        agent["origin"] = json!("agent");
        let result = session.apply_transaction(&agent.to_string());
        if explicit_unlock {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().code, "LOCKED_PROPERTY");
            assert_eq!(snapshot(&session), before);
        }
    }
}

#[test]
fn static_variable_binding_sets_absolute_position() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Variable geometry").unwrap());
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let variable = Uuid::new_v4();
    document["variables"] = json!([{"id":variable,"name":"Offset","value":-20}]);
    document["nodes"][0]["bindings"]["/layout/x"] =
        json!({"type":"variable","variableId":variable});
    write_document_fixture(&path, &document);
    let session = open_project(&path).unwrap();
    let root = root(&session);
    let point: Value = serde_json::from_str(
        &session
            .transform_point_json(&root.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(point["x"], -20.0);
}

#[test]
fn checkpoint_restore_does_not_mistake_missing_ancestor_lock_for_explicit_unlock() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Checkpoint protection").unwrap();
    let root = root(&session);
    let checkpoint = session.create_checkpoint().unwrap();
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/w","value":700}
        ]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    session.save().unwrap();
    let lock = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/propertyMeta/~1layout/agentLocked","value":true}
        ]),
    );
    session.apply_transaction(&lock.to_string()).unwrap();
    session.save().unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    let before = snapshot(&session);
    assert_eq!(before["document"]["nodes"][0]["layout"]["w"], 1440.0);
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/w","value":800}
        ]),
    );
    agent["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
    assert!(before["document"]["nodes"][0]["propertyMeta"]["/layout"].is_null());
    let second = session.create_checkpoint().unwrap();
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/h","value":500}
        ]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    session.save().unwrap();
    session.restore_checkpoint(&second).unwrap();
    let before = snapshot(&session);
    assert_eq!(before["document"]["nodes"][0]["layout"]["h"], 900.0);
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/h","value":600}
        ]),
    );
    agent["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
}

#[test]
fn checkpoint_restore_removes_later_explicit_unlock_absent_from_target() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Rewind unlock").unwrap();
    let root = root(&session);
    let checkpoint = session.create_checkpoint().unwrap();
    let unlock = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/propertyMeta/~1layout/agentLocked","value":false}
        ]),
    );
    session.apply_transaction(&unlock.to_string()).unwrap();
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/w","value":700}
        ]),
    );
    agent["origin"] = json!("agent");
    session.apply_transaction(&agent.to_string()).unwrap();
    session.save().unwrap();
    session.restore_checkpoint(&checkpoint).unwrap();
    let before = snapshot(&session);
    assert!(before["document"]["nodes"][0]["propertyMeta"]["/layout"].is_null());
    let mut agent = transaction(
        &session,
        &[root],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/w","value":800}
        ]),
    );
    agent["origin"] = json!("agent");
    assert_eq!(
        session
            .apply_transaction(&agent.to_string())
            .unwrap_err()
            .code,
        "LOCKED_PROPERTY"
    );
    assert_eq!(snapshot(&session), before);
}

#[test]
fn historical_invalid_bound_size_opens_without_rewriting_and_can_be_repaired() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Legacy binding").unwrap());
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = Uuid::parse_str(document["nodes"][0]["id"].as_str().unwrap()).unwrap();
    let child = Uuid::new_v4();
    let token = Uuid::new_v4();
    document["tokens"] = json!([{"id":token,"name":"Width","value":-1}]);
    document["nodes"][0]["bindings"]["/layout/w"] = json!({"type":"token","tokenId":token});
    document["nodes"][0]["layout"]["flow"] = json!({
        "direction":"row","gap":0,"padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"
    });
    document["nodes"][0]["responsive"] = json!([{"breakpoint":500,"w":300}]);
    document["nodes"][0]["children"] = json!([child]);
    document["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node(child, root, "group"));
    write_document_fixture(&path, &document);
    let original = std::fs::read(path.join("document.json")).unwrap();
    let session = open_project(&path).unwrap();
    assert_eq!(std::fs::read(path.join("document.json")).unwrap(), original);
    assert_eq!(
        session
            .transform_point_json(&child.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
            .unwrap_err()
            .code,
        "INVALID_DOCUMENT"
    );
    session
        .transform_point_at_breakpoint_json(
            &child.to_string(),
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            false,
            false,
            500.0,
        )
        .unwrap();
    let repair = transaction(
        &session,
        &[root],
        json!([
            {"type":"setToken","token":{"id":token,"name":"Width","value":300}}
        ]),
    );
    session.apply_transaction(&repair.to_string()).unwrap();
    session
        .transform_point_json(&child.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
        .unwrap();
}

#[test]
fn component_instance_hug_uses_the_template_and_breakpoint() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Component hug").unwrap());
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = Uuid::parse_str(document["pages"][0]["rootId"].as_str().unwrap()).unwrap();
    let template = Uuid::new_v4();
    let child = Uuid::new_v4();
    let row = Uuid::new_v4();
    let component = Uuid::new_v4();
    let mut template_node = node(template, root, "group");
    template_node["layout"]["w"] = json!(80);
    template_node["children"] = json!([child]);
    template_node["responsive"] = json!([{"breakpoint":500,"w":50}]);
    let mut row_node = node(row, root, "group");
    row_node["layout"]["flow"] = json!({
        "direction":"row","gap":0,"padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"
    });
    document["nodes"][0]["children"] = json!([template, row]);
    document["nodes"].as_array_mut().unwrap().extend([
        template_node,
        node(child, template, "group"),
        row_node,
    ]);
    document["components"] = json!([{"id":component,"rootId":template}]);
    write_document_fixture(&path, &document);
    let session = open_project(&path).unwrap();
    let instance = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut instance_node = node(instance, row, "componentInstance");
    instance_node["content"]["componentId"] = json!(component);
    instance_node["layout"]["widthMode"] = json!("hug");
    let insert = transaction(
        &session,
        &[row, instance, marker],
        json!([
            {"type":"insertNode","parentId":row,"index":0,"node":instance_node},
            {"type":"insertNode","parentId":row,"index":1,"node":node(marker, row, "group")}
        ]),
    );
    session.apply_transaction(&insert.to_string()).unwrap();
    let position = |breakpoint: Option<f64>| -> f64 {
        let value = match breakpoint {
            Some(width) => session.transform_point_at_breakpoint_json(
                &marker.to_string(),
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                false,
                false,
                width,
            ),
            None => session.transform_point_json(
                &marker.to_string(),
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                false,
                false,
            ),
        }
        .unwrap();
        serde_json::from_str::<Value>(&value).unwrap()["x"]
            .as_f64()
            .unwrap()
    };
    assert_eq!(position(None), 80.0);
    assert_eq!(position(Some(500.0)), 50.0);
    let change = transaction(
        &session,
        &[template],
        json!([
            {"type":"setProperty","nodeId":template,"path":"/layout/flow","value":{
                "direction":"row","gap":0,"padding":{"top":0,"right":3,"bottom":0,"left":2},"align":"start"
            }},
            {"type":"setProperty","nodeId":template,"path":"/layout/widthMode","value":"hug"}
        ]),
    );
    session.apply_transaction(&change.to_string()).unwrap();
    assert_eq!(position(None), 105.0);
    assert_eq!(position(Some(500.0)), 50.0);
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    let result = reopened
        .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false)
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["x"], 105.0);
}

#[test]
fn vector_hug_measures_shapes_paths_and_viewport_coordinates() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Vector hug").unwrap();
    let root = root(&session);
    let vector_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut vector = node(vector_id, root, "vector");
    vector["layout"]["widthMode"] = json!("hug");
    vector["layout"]["heightMode"] = json!("hug");
    vector["sourceBounds"] = json!({"x": 900, "y": 800, "w": 500, "h": 400});
    vector["sourceDensity"] = json!(2);
    vector["responsive"] = json!([{"breakpoint": 500, "w": 200}]);
    vector["content"]["vector"] = json!({"paths": [], "shapes": [
        {"type":"rect","x":0,"y":0,"w":1,"h":1}
    ]});
    let flow = |direction| {
        json!({
            "direction":direction,"gap":0,
            "padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"
        })
    };
    let insert = transaction(
        &session,
        &[root, vector_id, marker],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":flow("row")},
            {"type":"insertNode","parentId":root,"index":0,"node":vector},
            {"type":"insertNode","parentId":root,"index":1,"node":node(marker, root, "group")}
        ]),
    );
    session.apply_transaction(&insert.to_string()).unwrap();
    let point = |breakpoint: Option<f64>| -> Value {
        let result = if let Some(width) = breakpoint {
            session.transform_point_at_breakpoint_json(
                &marker.to_string(),
                0.0,
                0.0,
                2.0,
                3.0,
                5.0,
                false,
                false,
                width,
            )
        } else {
            session.transform_point_json(&marker.to_string(), 0.0, 0.0, 2.0, 3.0, 5.0, false, false)
        };
        serde_json::from_str(&result.unwrap()).unwrap()
    };
    let shapes = [
        (
            json!({"type":"rect","x":-5,"y":7,"w":40,"h":20,"rx":3,"ry":4}),
            40.0,
            20.0,
        ),
        (json!({"type":"circle","cx":-5,"cy":7,"r":3}), 6.0, 6.0),
        (
            json!({"type":"ellipse","cx":-5,"cy":7,"rx":2,"ry":4}),
            4.0,
            8.0,
        ),
        (
            json!({"type":"line","x1":3,"y1":-4,"x2":-7,"y2":9}),
            10.0,
            13.0,
        ),
        (
            json!({"type":"polyline","points":[{"x":0,"y":1},{"x":-2,"y":7},{"x":3,"y":-5}]}),
            5.0,
            12.0,
        ),
        (
            json!({"type":"polygon","points":[{"x":0,"y":0},{"x":8,"y":4},{"x":-2,"y":9}]}),
            10.0,
            9.0,
        ),
        (
            json!({"type":"rect","x":9007199254740992.0,"y":0,"w":1,"h":1}),
            1.0,
            1.0,
        ),
        (
            json!({"type":"circle","cx":9007199254740992.0,"cy":0,"r":0.25}),
            0.5,
            0.5,
        ),
    ];
    for (shape, width, height) in shapes {
        let change = transaction(
            &session,
            &[root, vector_id],
            json!([
                {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":flow("row")},
                {"type":"setProperty","nodeId":vector_id,"path":"/content/vector",
                    "value":{"paths":[],"shapes":[shape]}}
            ]),
        );
        session.apply_transaction(&change.to_string()).unwrap();
        assert_eq!(point(None)["x"].as_f64().unwrap(), 2.0 * width + 3.0);
        assert_eq!(point(Some(500.0))["x"].as_f64().unwrap(), 403.0);
        let column = transaction(
            &session,
            &[root],
            json!([{"type":"setProperty","nodeId":root,"path":"/layout/flow","value":flow("column")}]),
        );
        session.apply_transaction(&column.to_string()).unwrap();
        assert_eq!(point(None)["y"].as_f64().unwrap(), 2.0 * height + 5.0);
    }
    let curve = json!({"paths":[[
        {"type":"moveTo","x":0,"y":0},
        {"type":"curveTo","x1":100,"y1":200,"x2":100,"y2":-200,"x":0,"y":0},
        {"type":"moveTo","x":-30,"y":-4},
        {"type":"lineTo","x":-10,"y":-12},
        {"type":"close"},
        {"type":"lineTo","x":-20,"y":-8},
        {"type":"moveTo","x":999,"y":999}
    ]]});
    let change = transaction(
        &session,
        &[root, vector_id],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":flow("row")},
            {"type":"setProperty","nodeId":vector_id,"path":"/content/vector","value":curve},
            {"type":"setProperty","nodeId":vector_id,"path":"/layout/heightMode","value":"fixed"}
        ]),
    );
    session.apply_transaction(&change.to_string()).unwrap();
    assert!((point(None)["x"].as_f64().unwrap() - 213.0).abs() < 1e-10);
    let change = transaction(
        &session,
        &[root, vector_id],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":flow("column")},
            {"type":"setProperty","nodeId":vector_id,"path":"/layout/heightMode","value":"hug"},
            {"type":"setProperty","nodeId":vector_id,"path":"/layout/widthMode","value":"fixed"}
        ]),
    );
    session.apply_transaction(&change.to_string()).unwrap();
    let expected_y = 2.0 * (200.0 / 3.0_f64.sqrt()) + 5.0;
    assert!((point(None)["y"].as_f64().unwrap() - expected_y).abs() < 1e-10);
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    let point: Value = serde_json::from_str(
        &reopened
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 2.0, 3.0, 5.0, false, false)
            .unwrap(),
    )
    .unwrap();
    assert!((point["y"].as_f64().unwrap() - expected_y).abs() < 1e-10);
}

#[test]
fn vector_hug_rejects_ambiguous_empty_and_overflowing_geometry() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Vector errors").unwrap();
    let root = root(&session);
    let vector_id = Uuid::new_v4();
    let marker = Uuid::new_v4();
    let mut vector = node(vector_id, root, "vector");
    vector["layout"]["widthMode"] = json!("hug");
    vector["content"]["vector"] = json!({"paths":[]});
    let insert = transaction(
        &session,
        &[root, vector_id, marker],
        json!([
            {"type":"setProperty","nodeId":root,"path":"/layout/flow","value":{
                "direction":"row","gap":0,
                "padding":{"top":0,"right":0,"bottom":0,"left":0},"align":"start"}},
            {"type":"insertNode","parentId":root,"index":0,"node":vector},
            {"type":"insertNode","parentId":root,"index":1,"node":node(marker, root, "group")}
        ]),
    );
    session.apply_transaction(&insert.to_string()).unwrap();
    for value in [
        json!({"paths":[]}),
        json!({"paths":[[{"type":"moveTo","x":10,"y":20}]]}),
        json!({"paths":[[{"type":"lineTo","x":10,"y":20}]]}),
        json!({"paths":[[{"type":"close"}]]}),
    ] {
        let change = transaction(
            &session,
            &[vector_id],
            json!([{"type":"setProperty","nodeId":vector_id,"path":"/content/vector","value":value}]),
        );
        session.apply_transaction(&change.to_string()).unwrap();
        let before = snapshot(&session);
        assert_eq!(
            session
                .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false,)
                .unwrap_err()
                .code,
            "UNSUPPORTED_FEATURE"
        );
        assert_eq!(snapshot(&session), before);
    }
    let overflow = transaction(
        &session,
        &[vector_id],
        json!([{"type":"setProperty","nodeId":vector_id,"path":"/content/vector",
            "value":{"paths":[],"shapes":[{"type":"circle","cx":1e308,"cy":0,"r":1e308}]}}]),
    );
    session.apply_transaction(&overflow.to_string()).unwrap();
    assert_eq!(
        session
            .transform_point_json(&marker.to_string(), 0.0, 0.0, 1.0, 0.0, 0.0, false, false,)
            .unwrap_err()
            .code,
        "INVALID_DOCUMENT"
    );
}

fn apply_ok(session: &ProjectSession, scope: &[Uuid], operations: Value) {
    let tx = transaction(session, scope, operations);
    session.apply_transaction(&tx.to_string()).unwrap();
}

fn apply_rejected(session: &ProjectSession, scope: &[Uuid], operations: Value) -> String {
    let before = snapshot(session);
    let tx = transaction(session, scope, operations);
    let error = session.apply_transaction(&tx.to_string()).unwrap_err();
    assert_eq!(snapshot(session), before);
    error.code.to_owned()
}

fn saved_node(path: &std::path::Path, id: Uuid) -> Value {
    let document: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == json!(id.to_string()))
        .cloned()
        .unwrap()
}

#[test]
fn new_visual_properties_survive_undo_redo_and_save() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Visual").unwrap();
    let root = root(&session);
    let frame = Uuid::new_v4();
    let text = Uuid::new_v4();
    let image = Uuid::new_v4();
    apply_ok(
        &session,
        &[root, frame, text, image],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(frame, root, "frame")},
            {"type": "insertNode", "parentId": frame, "index": 0, "node": node(text, frame, "text")},
            {"type": "insertNode", "parentId": frame, "index": 1, "node": node(image, frame, "image")}
        ]),
    );
    let changes = [
        (frame, "/layout/minW", json!(10)),
        (frame, "/layout/minH", json!(20)),
        (frame, "/layout/maxW", json!(800)),
        (frame, "/layout/maxH", json!(600)),
        (frame, "/style/radius", json!(12)),
        (frame, "/style/rotate", json!(15.5)),
        (frame, "/style/overflow", json!("hidden")),
        (
            frame,
            "/style/stroke",
            json!({"color": "#112233", "width": 2}),
        ),
        (
            frame,
            "/style/shadows",
            json!([{"x": 1, "y": 2, "blur": 3, "spread": 4, "color": "#ffffff"}]),
        ),
        (text, "/style/textAlign", json!("center")),
        (text, "/style/maxLines", json!(3)),
        (image, "/style/fit", json!("contain")),
        (frame, "/hidden", json!(true)),
        (frame, "/locked", json!(true)),
    ];
    for (id, ptr, value) in changes {
        apply_ok(
            &session,
            &[id],
            json!([{"type": "setProperty", "nodeId": id, "path": ptr, "value": value}]),
        );
    }
    let scope = [frame, text, image];
    session.undo().unwrap();
    session.undo().unwrap();
    session.redo().unwrap();
    session.redo().unwrap();
    session.save().unwrap();
    let frame_saved = saved_node(&path, frame);
    assert_eq!(frame_saved["layout"]["minW"], 10.0);
    assert_eq!(frame_saved["layout"]["minH"], 20.0);
    assert_eq!(frame_saved["layout"]["maxW"], 800.0);
    assert_eq!(frame_saved["layout"]["maxH"], 600.0);
    assert_eq!(frame_saved["style"]["radius"], 12.0);
    assert_eq!(frame_saved["style"]["rotate"], 15.5);
    assert_eq!(frame_saved["style"]["overflow"], "hidden");
    assert_eq!(
        frame_saved["style"]["stroke"],
        json!({"color": "#112233", "width": 2.0})
    );
    assert_eq!(frame_saved["style"]["shadows"].as_array().unwrap().len(), 1);
    assert_eq!(frame_saved["hidden"], true);
    assert_eq!(frame_saved["locked"], true);
    assert_eq!(saved_node(&path, text)["style"]["textAlign"], "center");
    assert_eq!(saved_node(&path, text)["style"]["maxLines"], 3);
    assert_eq!(saved_node(&path, image)["style"]["fit"], "contain");
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(snapshot(&reopened)["readOnly"], false);
    assert!(scope
        .iter()
        .all(|id| snapshot(&reopened)["document"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["id"] == json!(id.to_string()))));
}

#[test]
fn flow_padding_and_gap_can_be_written_individually() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Flow").unwrap();
    let root = root(&session);
    apply_ok(
        &session,
        &[root],
        json!([{"type": "setProperty", "nodeId": root, "path": "/layout/flow", "value": {
            "direction": "row", "gap": 0,
            "padding": {"top": 0, "right": 0, "bottom": 0, "left": 0}, "align": "start"}}]),
    );
    for (ptr, value) in [
        ("/layout/flow/gap", json!(16)),
        ("/layout/flow/padding/top", json!(1)),
        ("/layout/flow/padding/right", json!(2)),
        ("/layout/flow/padding/bottom", json!(3)),
        ("/layout/flow/padding/left", json!(4)),
    ] {
        apply_ok(
            &session,
            &[root],
            json!([{"type": "setProperty", "nodeId": root, "path": ptr, "value": value}]),
        );
    }
    session.save().unwrap();
    let flow = saved_node(&path, root)["layout"]["flow"].clone();
    assert_eq!(flow["gap"], 16.0);
    assert_eq!(
        flow["padding"],
        json!({"top": 1.0, "right": 2.0, "bottom": 3.0, "left": 4.0})
    );
    let text = Uuid::new_v4();
    assert_eq!(
        apply_rejected(
            &session,
            &[root, text],
            json!([
                {"type": "insertNode", "parentId": root, "index": 0, "node": node(text, root, "text")},
                {"type": "setProperty", "nodeId": text, "path": "/layout/flow/gap", "value": 4}
            ]),
        ),
        "INVALID_DOCUMENT"
    );
}

#[test]
fn invalid_visual_values_are_rejected_without_mutation() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Invalid").unwrap();
    let root = root(&session);
    let frame = Uuid::new_v4();
    let text = Uuid::new_v4();
    let image = Uuid::new_v4();
    apply_ok(
        &session,
        &[root, frame, text, image],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": node(frame, root, "group")},
            {"type": "insertNode", "parentId": frame, "index": 0, "node": node(text, frame, "text")},
            {"type": "insertNode", "parentId": frame, "index": 1, "node": node(image, frame, "image")}
        ]),
    );
    apply_ok(
        &session,
        &[frame],
        json!([
            {"type": "setProperty", "nodeId": frame, "path": "/layout/minW", "value": 10},
            {"type": "setProperty", "nodeId": frame, "path": "/layout/maxW", "value": 20}
        ]),
    );
    for (id, ptr, value) in [
        (frame, "/layout/minW", json!(30)),
        (frame, "/layout/maxW", json!(-1)),
        (frame, "/style/radius", json!(-1)),
        (frame, "/style/rotate", json!(4000)),
        (frame, "/style/stroke", json!({"color": "red", "width": 1})),
        (
            frame,
            "/style/shadows",
            json!((0..9)
                .map(|_| json!({"x": 0, "y": 0, "blur": 0, "spread": 0, "color": "#000000"}))
                .collect::<Vec<_>>()),
        ),
        (text, "/style/maxLines", json!(0)),
    ] {
        assert_eq!(
            apply_rejected(
                &session,
                &[id],
                json!([{"type": "setProperty", "nodeId": id, "path": ptr, "value": value}]),
            ),
            "INVALID_DOCUMENT",
            "{ptr} {value}"
        );
    }
    let vector = Uuid::new_v4();
    assert_eq!(
        apply_rejected(
            &session,
            &[root, vector],
            json!([
                {"type": "insertNode", "parentId": root, "index": 0, "node": node(vector, root, "vector")},
                {"type": "setProperty", "nodeId": vector, "path": "/style/radius", "value": -1}
            ]),
        ),
        "INVALID_DOCUMENT"
    );
    assert_eq!(
        apply_rejected(
            &session,
            &[root, vector],
            json!([
                {"type": "insertNode", "parentId": root, "index": 0, "node": node(vector, root, "vector")},
                {"type": "setProperty", "nodeId": vector, "path": "/style/textAlign", "value": "center"}
            ]),
        ),
        "UNSUPPORTED_FEATURE"
    );
}

fn png_asset(path: &std::path::Path, width: u32, height: u32) {
    use image::{ExtendedColorType, ImageEncoder};
    let file = std::fs::File::create(path).unwrap();
    let pixels = vec![128_u8; (width * height * 4) as usize];
    image::codecs::png::PngEncoder::new(file)
        .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
        .unwrap();
}

fn import_asset(session: &ProjectSession, source: &std::path::Path) -> Uuid {
    let asset: Value = serde_json::from_str(&session.import_asset(source).unwrap()).unwrap();
    asset["id"].as_str().unwrap().parse().unwrap()
}

#[test]
fn layout_json_reports_flow_positions_and_computed_sizes() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Layout").unwrap();
    let root = root(&session);
    let frame = Uuid::new_v4();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let free = Uuid::new_v4();
    let mut frame_node = node(frame, root, "frame");
    frame_node["layout"] = json!({"x": 10, "y": 20, "w": 300, "h": 100});
    let mut first_node = node(first, frame, "group");
    first_node["layout"] = json!({"x": 0, "y": 0, "w": 50, "h": 30});
    let mut second_node = node(second, frame, "group");
    second_node["layout"]["widthMode"] = json!("fill");
    let mut free_node = node(free, root, "group");
    free_node["layout"] = json!({"x": 400, "y": 50, "w": 20, "h": 20});
    apply_ok(
        &session,
        &[root, frame, first, second, free],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": free_node},
            {"type": "insertNode", "parentId": root, "index": 1, "node": frame_node},
            {"type": "insertNode", "parentId": frame, "index": 0, "node": first_node},
            {"type": "insertNode", "parentId": frame, "index": 1, "node": second_node},
            {"type": "setProperty", "nodeId": frame, "path": "/layout/flow", "value": {
                "direction": "row", "gap": 10,
                "padding": {"top": 5, "right": 5, "bottom": 5, "left": 5}, "align": "center"}}
        ]),
    );
    let bounds: Value = serde_json::from_str(&session.layout_json(None).unwrap()).unwrap();
    let find = |id: Uuid| {
        bounds
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == json!(id.to_string()))
            .cloned()
            .unwrap()
    };
    let root_bounds = find(root);
    assert_eq!(root_bounds["x"], 0.0);
    assert_eq!(root_bounds["w"], 1440.0);
    let frame_bounds = find(frame);
    assert_eq!(
        (frame_bounds["x"].as_f64(), frame_bounds["y"].as_f64()),
        (Some(10.0), Some(20.0))
    );
    let first_bounds = find(first);
    // Cross-axis alignment centres the 30 px child in the 90 px content box.
    assert_eq!(
        (first_bounds["x"].as_f64(), first_bounds["y"].as_f64()),
        (Some(15.0), Some(55.0))
    );
    assert_eq!(first_bounds["inFlow"], true);
    let second_bounds = find(second);
    assert_eq!(second_bounds["x"], 75.0);
    assert_eq!(second_bounds["w"], 230.0);
    let free_bounds = find(free);
    assert_eq!(
        (free_bounds["x"].as_f64(), free_bounds["y"].as_f64()),
        (Some(400.0), Some(50.0))
    );
    assert_eq!(free_bounds["inFlow"], false);
    // The canvas needs the parent for nesting and the rotation for its markers.
    assert_eq!(frame_bounds["parentId"], json!(root.to_string()));
    assert_eq!(root_bounds["parentId"], Value::Null);
    assert_eq!(frame_bounds["rotation"], 0.0);
}

#[test]
fn preview_html_is_deterministic_self_contained_and_escaped() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Preview").unwrap();
    let root = root(&session);
    let frame = Uuid::new_v4();
    let text = Uuid::new_v4();
    let image = Uuid::new_v4();
    let vector = Uuid::new_v4();
    let png = temp.path().join("pixel.png");
    png_asset(&png, 4, 2);
    let asset = import_asset(&session, &png);
    let mut text_node = node(text, root, "text");
    text_node["content"]["text"] = json!("Привет <b>&\"мир\"</b>");
    text_node["style"] = json!({"fontSize": 20, "color": "#ff0000"});
    let mut frame_node = node(frame, root, "frame");
    frame_node["style"] = json!({"radius": 4, "overflow": "hidden", "fill": "#eeeeee"});
    let mut image_node = node(image, root, "image");
    image_node["content"]["assetId"] = json!(asset);
    image_node["style"] = json!({"fit": "contain", "stroke": {"color": "#00ff00", "width": 2}});
    let mut vector_node = node(vector, root, "vector");
    vector_node["content"]["vector"] =
        json!({"paths": [], "shapes": [{"type": "circle", "cx": 10, "cy": 10, "r": 5}]});
    apply_ok(
        &session,
        &[root, frame, text, image, vector],
        json!([
            {"type": "insertNode", "parentId": root, "index": 0, "node": frame_node},
            {"type": "insertNode", "parentId": root, "index": 1, "node": text_node},
            {"type": "insertNode", "parentId": root, "index": 2, "node": image_node},
            {"type": "insertNode", "parentId": root, "index": 3, "node": vector_node}
        ]),
    );
    let first = session.preview_html(None).unwrap();
    let second = session.preview_html(None).unwrap();
    assert_eq!(first, second);
    let lower = first.to_ascii_lowercase();
    assert!(!lower.contains("<script"));
    assert!(!lower.contains("http://") && !lower.contains("https://"));
    assert!(first.contains("data:image/png;base64,"));
    assert!(first.contains("Привет &lt;b&gt;&amp;\"мир\"&lt;/b&gt;"));
    let frame_html = first.split(&format!("data-id=\"{frame}\"")).nth(1).unwrap();
    let frame_style = frame_html
        .split("style=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    for declaration in [
        "border-radius:4px;",
        "overflow:hidden;",
        "background-color:#eeeeee;",
    ] {
        assert!(
            frame_style.contains(declaration),
            "{declaration}: {frame_style}"
        );
    }
    assert!(first.contains("object-fit:contain;"));
    assert!(first.contains("outline:2px solid #00ff00;"));
    assert!(first.contains("<circle cx=\"10\" cy=\"10\" r=\"5\"/>"));
    assert!(first.contains(&format!("data-id=\"{image}\"")));
    // Editing the document changes the preview.
    apply_ok(
        &session,
        &[text],
        json!([{"type": "setProperty", "nodeId": text, "path": "/content/text", "value": "Другое"} ]),
    );
    let third = session.preview_html(None).unwrap();
    assert_ne!(first, third);
    assert!(third.contains("Другое"));
    // Missing assets are reported by the dedicated code, not a layout metric error.
    std::fs::remove_file(path.join({
        let doc: Value =
            serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
        doc["assets"][0]["path"].as_str().unwrap().to_owned()
    }))
    .unwrap();
    let error = session.preview_html(None).unwrap_err();
    assert!(
        matches!(error.code, "MISSING_ASSET" | "CONFLICT"),
        "{error:?}"
    );
}

#[test]
fn line_breaks_follow_the_measured_text_width() {
    let temp = tempdir().unwrap();
    let session = create_project(temp.path().join("project"), "Lines").unwrap();
    let root = root(&session);
    let text = Uuid::new_v4();
    let mut text_node = node(text, root, "text");
    text_node["layout"] = json!({"x": 0, "y": 0, "w": 200, "h": 100});
    text_node["content"]["text"] = json!("Один два три четыре пять шесть семь восемь");
    apply_ok(
        &session,
        &[root, text],
        json!([{"type": "insertNode", "parentId": root, "index": 0, "node": text_node}]),
    );
    let narrow: Vec<usize> =
        serde_json::from_str(&session.line_breaks_json(&text.to_string(), 60.0).unwrap()).unwrap();
    let wide: Vec<usize> =
        serde_json::from_str(&session.line_breaks_json(&text.to_string(), 2000.0).unwrap())
            .unwrap();
    assert!(!narrow.is_empty());
    assert!(wide.is_empty());
    assert!(narrow.windows(2).all(|pair| pair[0] < pair[1]));
    // Breaks always fall on a word boundary offset inside the text.
    let source = "Один два три четыре пять шесть семь восемь";
    for offset in narrow {
        assert!(source.is_char_boundary(offset));
        assert_ne!(source.as_bytes()[offset], b' ');
    }
    assert_eq!(
        session
            .line_breaks_json(&root.to_string(), 100.0)
            .unwrap_err()
            .code,
        "UNSUPPORTED_FEATURE"
    );
}

fn json_of(value: aic_core::Transaction) -> Value {
    serde_json::to_value(value).unwrap()
}

fn apply(session: &ProjectSession, transaction: Value) -> Value {
    let result = session.apply_transaction(&transaction.to_string()).unwrap();
    let snapshot: Value = serde_json::from_str(&result).unwrap();
    snapshot["document"].clone()
}

fn find(document: &Value, id: Uuid) -> &Value {
    document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == json!(id))
        .unwrap_or_else(|| panic!("node {id} is missing in {document}"))
}

fn layout(session: &ProjectSession) -> Value {
    serde_json::from_str(&session.layout_json(None).unwrap()).unwrap()
}

fn box_of(session: &ProjectSession, id: Uuid) -> (f64, f64, f64, f64) {
    let layout = layout(session);
    let item = layout
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == json!(id))
        .unwrap();
    let value = |key: &str| item[key].as_f64().unwrap();
    (value("x"), value("y"), value("w"), value("h"))
}

fn document_of(session: &ProjectSession) -> Value {
    snapshot(session)["document"].clone()
}

#[test]
fn group_and_ungroup_keep_screen_positions_in_one_step() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Group").unwrap();
    let root = root(&session);
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut first_node = node(first, root, "frame");
    first_node["children"] = json!([]);
    first_node["layout"] = json!({"x": 40, "y": 20, "w": 100, "h": 50});
    let mut second_node = node(second, root, "frame");
    second_node["layout"] = json!({"x": 200, "y": 100, "w": 60, "h": 40});
    apply(
        &session,
        transaction(
            &session,
            &[root, first, second],
            json!([
                {"type": "insertNode", "parentId": root, "index": 0, "node": first_node},
                {"type": "insertNode", "parentId": root, "index": 1, "node": second_node}
            ]),
        ),
    );
    let grouped = apply(
        &session,
        json_of(
            session
                .group_json(
                    vec![first.to_string(), second.to_string()],
                    Some("Hero".into()),
                )
                .unwrap(),
        ),
    );
    let shallow = |value: &Value| value["nodes"][0]["children"].as_array().unwrap().clone();
    let root_children = shallow(&grouped);
    assert_eq!(root_children.len(), 1);
    let group_id = Uuid::parse_str(root_children[0].as_str().unwrap()).unwrap();
    let group = find(&grouped, group_id);
    assert_eq!(group["kind"], "group");
    assert_eq!(group["name"], "Hero");
    assert_eq!(group["layout"]["x"], 40.0);
    assert_eq!(group["layout"]["y"], 20.0);
    assert_eq!(group["layout"]["w"], 220.0);
    assert_eq!(group["layout"]["h"], 120.0);
    assert_eq!(find(&grouped, first)["layout"]["x"], 0.0);
    assert_eq!(find(&grouped, second)["layout"]["y"], 80.0);
    assert_eq!(
        find(&grouped, first)["propertyMeta"]["/layout/x"]["origin"],
        "user"
    );
    assert_eq!(box_of(&session, second), (200.0, 100.0, 60.0, 40.0));

    session.undo().unwrap();
    assert!(snapshot(&session)["document"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|node| node["kind"] != "group"));

    let regrouped = apply(
        &session,
        json_of(
            session
                .group_json(vec![first.to_string(), second.to_string()], None)
                .unwrap(),
        ),
    );
    let root_children = shallow(&regrouped);
    assert_eq!(root_children.len(), 1);
    let group_id = Uuid::parse_str(root_children[0].as_str().unwrap()).unwrap();
    let ungrouped = apply(
        &session,
        json_of(session.ungroup_json(&group_id.to_string()).unwrap()),
    );
    assert!(ungrouped["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|node| node["kind"] != "group"));
    assert_eq!(box_of(&session, first), (40.0, 20.0, 100.0, 50.0));
    assert_eq!(box_of(&session, second), (200.0, 100.0, 60.0, 40.0));
    assert_eq!(ungrouped["nodes"][0]["children"], json!([first, second]));
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(box_of(&reopened, second), (200.0, 100.0, 60.0, 40.0));
}

#[test]
fn group_rejects_mixed_parents_roots_and_ancestors() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Group errors").unwrap();
    let root = root(&session);
    let frame = Uuid::new_v4();
    let child = Uuid::new_v4();
    let stray = Uuid::new_v4();
    let mut frame_node = node(frame, root, "frame");
    frame_node["children"] = json!([]);
    apply(
        &session,
        transaction(
            &session,
            &[root, frame, child, stray],
            json!([
                {"type": "insertNode", "parentId": root, "index": 0, "node": frame_node},
                {"type": "insertNode", "parentId": frame, "index": 0, "node": node(child, frame, "text")},
                {"type": "insertNode", "parentId": root, "index": 1, "node": node(stray, root, "text")}
            ]),
        ),
    );
    // The minimal fixture caps the node count, so the ancestor pairing is
    // exercised after grouping an existing child instead of on the raw tree.
    let first_group = apply(
        &session,
        json_of(session.group_json(vec![stray.to_string()], None).unwrap()),
    );
    let group = Uuid::parse_str(first_group["nodes"][0]["children"][1].as_str().unwrap()).unwrap();
    let ancestor = find(&first_group, group)["children"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let before = snapshot(&session);
    let error = session
        .group_json(vec![group.to_string(), ancestor], None)
        .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_FEATURE");
    for (ids, codes) in [
        (
            vec![frame.to_string(), stray.to_string()],
            &["UNSUPPORTED_FEATURE"][..],
        ),
        (
            vec![frame.to_string(), child.to_string()],
            &["UNSUPPORTED_FEATURE"][..],
        ),
        (
            vec![stray.to_string(), stray.to_string()],
            &["INVALID_DOCUMENT"][..],
        ),
        (vec![Uuid::new_v4().to_string()], &["INVALID_DOCUMENT"][..]),
    ] {
        let error = session.group_json(ids, None).unwrap_err();
        assert!(codes.contains(&error.code), "{error:?}");
    }
    assert_eq!(snapshot(&session), before);
}

#[test]
fn paste_rewrites_ids_actions_and_keeps_assets_but_drops_agent_locks() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    drop(create_project(&path, "Paste").unwrap());
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(path.join("document.json")).unwrap()).unwrap();
    let root = Uuid::parse_str(document["pages"][0]["rootId"].as_str().unwrap()).unwrap();
    let frame = Uuid::new_v4();
    let text = Uuid::new_v4();
    let action = Uuid::new_v4();
    let mut frame_node = node(frame, root, "frame");
    frame_node["children"] = json!([]);
    frame_node["propertyMeta"] =
        json!({"/name": {"origin": "agent", "lastRevision": 0, "agentLocked": true}});
    let mut text_node = node(text, frame, "text");
    text_node["content"]["text"] = json!("Привет");
    text_node["events"] = json!({"click": action});
    document["nodes"][0]["children"] = json!([frame]);
    frame_node["children"] = json!([text]);
    document["nodes"]
        .as_array_mut()
        .unwrap()
        .extend([frame_node, text_node]);
    document["actions"] =
        json!([{"id": action, "kind": {"type": "openUrl", "url": "https://example.com"}}]);
    write_document_fixture(&path, &document);
    let session = open_project(&path).unwrap();
    let pasted = apply(
        &session,
        json_of(
            session
                .paste_nodes_json(vec![frame.to_string()], &root.to_string(), None)
                .unwrap(),
        ),
    );
    let document = &pasted;
    let root_children = document["nodes"][0]["children"].as_array().unwrap();
    assert_eq!(root_children.len(), 2);
    let copy_id = root_children[1].as_str().unwrap();
    let copy = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == json!(copy_id))
        .unwrap();
    assert_ne!(copy_id, frame.to_string());
    let copied_text_id = copy["children"][0].as_str().unwrap().to_owned();
    assert_ne!(copied_text_id, text.to_string());
    // The insert itself locks every property as a user edit, so the source's
    // agent lock cannot leak through.
    assert!(copy["propertyMeta"]
        .as_object()
        .unwrap()
        .values()
        .all(|meta| meta["origin"] == "user"));
    let copied_text = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == json!(copied_text_id.as_str()))
        .unwrap();

    assert_eq!(copied_text["parentId"], copy["id"]);
    assert_eq!(copied_text["content"]["text"], "Привет");
    let new_action = copied_text["events"]["click"].as_str().unwrap();
    assert_ne!(new_action, action.to_string());
    assert!(document["actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == json!(new_action) && item["kind"]["type"] == "openUrl"));
    session.undo().unwrap();
    assert_eq!(document_of(&session)["nodes"].as_array().unwrap().len(), 3);
}

#[test]
fn paste_from_another_project_reuses_shared_assets_and_rejects_missing_ones() {
    let temp = tempdir().unwrap();
    let file = temp.path().join("pixel.png");
    png_asset(&file, 1, 1);

    let source_path = temp.path().join("source");
    let source = create_project(&source_path, "Source").unwrap();
    let source_root = root(&source);
    let asset_id = import_asset(&source, &file);
    let image = Uuid::new_v4();
    let mut image_node = node(image, source_root, "image");
    image_node["layout"] = json!({"x": 10, "y": 10, "w": 10, "h": 10});
    image_node["content"] = json!({"assetId": asset_id});
    // The cross-project paste re-reads the source from disk.
    apply(
        &source,
        transaction(
            &source,
            &[source_root, image],
            json!([{"type": "insertNode", "parentId": source_root, "index": 0, "node": image_node}]),
        ),
    );
    source.save().unwrap();

    let missing_path = temp.path().join("missing");
    let missing = create_project(&missing_path, "Missing").unwrap();
    let missing_root = root(&missing);
    let error = missing
        .paste_from_project_json(
            &source_path,
            vec![image.to_string()],
            &missing_root.to_string(),
            None,
        )
        .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_FEATURE");
    assert!(error.message.contains("assets/"), "{}", error.message);

    let target_path = temp.path().join("target");
    let target = create_project(&target_path, "Target").unwrap();
    let target_root = root(&target);
    let shared = import_asset(&target, &file);
    let pasted = apply(
        &target,
        json_of(
            target
                .paste_from_project_json(
                    &source_path,
                    vec![image.to_string()],
                    &target_root.to_string(),
                    None,
                )
                .unwrap(),
        ),
    );
    let copy = pasted["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["kind"] == "image")
        .unwrap();
    assert_eq!(copy["content"]["assetId"], json!(shared));
    assert_ne!(copy["id"], json!(image));
}

#[test]
fn component_lifecycle_resolves_instances_and_breaking_is_deep() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Components").unwrap();
    let root = root(&session);
    let card = Uuid::new_v4();
    let label = Uuid::new_v4();
    let mut card_node = node(card, root, "frame");
    card_node["layout"] = json!({"x": 100, "y": 50, "w": 200, "h": 80});
    card_node["children"] = json!([]);
    let mut label_node = node(label, card, "text");
    label_node["content"]["text"] = json!("Карточка");
    apply(
        &session,
        transaction(
            &session,
            &[root, card, label],
            json!([
                {"type": "insertNode", "parentId": root, "index": 0, "node": card_node},
                {"type": "insertNode", "parentId": card, "index": 0, "node": label_node}
            ]),
        ),
    );
    let component_id = Uuid::new_v4();
    let created = apply(
        &session,
        transaction(
            &session,
            &[card],
            json!([{"type": "setComponent", "componentId": component_id, "rootId": card, "name": "Card"}]),
        ),
    );
    let component = created["components"][0].clone();
    assert_eq!(component["rootId"], json!(card));
    assert_eq!(component["name"], "Card");
    let inserted = apply(
        &session,
        json_of(
            session
                .insert_component_instance_json(
                    &component_id.to_string(),
                    &root.to_string(),
                    None,
                    400.0,
                    300.0,
                )
                .unwrap(),
        ),
    );
    let instance_id =
        Uuid::parse_str(inserted["nodes"][0]["children"][1].as_str().unwrap()).unwrap();
    let instance = find(&inserted, instance_id);
    assert_eq!(instance["kind"], "componentInstance");
    assert_eq!(instance["layout"]["w"], 200.0);
    assert_eq!(instance["children"].as_array().unwrap().len(), 1);
    assert_eq!(box_of(&session, instance_id), (400.0, 300.0, 200.0, 80.0));
    let copy_label = instance["children"][0].as_str().unwrap();
    assert_ne!(copy_label, label.to_string());
    assert_eq!(
        find(&inserted, Uuid::parse_str(copy_label).unwrap())["content"]["text"],
        "Карточка"
    );

    let broken = apply(
        &session,
        json_of(
            session
                .break_component_link_json(&instance_id.to_string())
                .unwrap(),
        ),
    );
    let instance = find(&broken, instance_id);
    assert_eq!(instance["kind"], "frame");
    assert!(instance["content"].get("componentId").is_none());
    assert_eq!(instance["children"].as_array().unwrap().len(), 1);
    assert_eq!(box_of(&session, instance_id), (400.0, 300.0, 200.0, 80.0));
    assert_eq!(
        broken["components"],
        json!([{
            "id": component["id"], "rootId": card, "name": "Card"
        }])
    );
    session.undo().unwrap();
    assert!(find(&document_of(&session), instance_id)["content"]["componentId"].is_string());
    let error = session
        .create_component_json(vec![instance_id.to_string()], None)
        .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_FEATURE");
    session.save().unwrap();
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(box_of(&reopened, instance_id), (400.0, 300.0, 200.0, 80.0));
}

#[test]
fn defining_a_component_checks_the_root_and_records_history() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Detached").unwrap();
    let root = root(&session);
    let frame = Uuid::new_v4();
    let component = Uuid::new_v4();
    apply(
        &session,
        transaction(
            &session,
            &[root, frame],
            json!([{"type": "insertNode", "parentId": root, "index": 0, "node": node(frame, root, "frame")}]),
        ),
    );
    let defined = apply(
        &session,
        json_of(
            session
                .set_component_json(
                    &component.to_string(),
                    &frame.to_string(),
                    Some("Direct".into()),
                )
                .unwrap(),
        ),
    );
    assert_eq!(defined["components"].as_array().unwrap().len(), 1);
    assert_eq!(defined["components"][0]["rootId"], json!(frame));
    assert_eq!(defined["components"][0]["name"], "Direct");
    // Reusing the component ID would silently rewrite an existing definition.
    assert!(session
        .set_component_json(&component.to_string(), &frame.to_string(), None)
        .unwrap_err()
        .message
        .contains("already exists"));
    let error = session
        .set_component_json(
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
            None,
        )
        .unwrap_err();
    assert_eq!(error.code, "INVALID_DOCUMENT");
    assert!(
        error.message.starts_with("Unknown node"),
        "{}",
        error.message
    );
    let mut defined = document_of(&session);
    defined.as_object_mut().unwrap().remove("revision");
    session.undo().unwrap();
    assert!(document_of(&session)["components"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(document_of(&session)["nodes"].as_array().unwrap().len(), 2);
    session.redo().unwrap();
    let mut after = document_of(&session);
    after.as_object_mut().unwrap().remove("revision");
    assert_eq!(after, defined);
}
