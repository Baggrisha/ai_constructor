//! Editor gestures as the canvas performs them: every gesture is one
//! transaction against the current document, applied through the session the
//! same way `EditingActions.swift` does, and one undo step away.
//!
//! The core commands return a transaction; applying it and undoing are the
//! only public entry points the Swift side uses, so the tests use those too.

use aic_core::serde_json::{json, Value};
use aic_core::{create_project, open_project, ProjectSession};
use std::path::PathBuf;
use uuid::Uuid;

struct Project {
    dir: PathBuf,
    session: ProjectSession,
}

impl Drop for Project {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn temp_project(name: &str) -> Project {
    let dir = std::env::temp_dir().join(format!("aic-gestures-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let session = create_project(&dir, name).expect("create project");
    Project { dir, session }
}

fn snapshot(project: &Project) -> Value {
    serde_json::from_str(&project.session.snapshot_json().expect("snapshot")).expect("json")
}

fn nodes(project: &Project) -> Vec<Value> {
    snapshot(project)["document"]["nodes"]
        .as_array()
        .expect("nodes")
        .clone()
}

fn find(project: &Project, id: &str) -> Option<Value> {
    nodes(project)
        .into_iter()
        .find(|node| node["id"].as_str() == Some(id))
}

fn root_id(project: &Project) -> String {
    snapshot(project)["document"]["pages"][0]["rootId"]
        .as_str()
        .expect("root")
        .to_owned()
}

fn revision(project: &Project) -> u64 {
    snapshot(project)["document"]["revision"]
        .as_u64()
        .expect("revision")
}

fn history_undo_len(project: &Project) -> usize {
    let state: Value = serde_json::from_slice(
        &std::fs::read(project.dir.join("history/state.json")).expect("history"),
    )
    .expect("history json");
    state["undo"].as_array().expect("undo stack").len()
}

fn transaction(project: &Project, scope: &[String], operations: Vec<Value>) -> String {
    let state = snapshot(project);
    json!({
        "transactionId": Uuid::new_v4(),
        "baseRevision": state["document"]["revision"],
        "baseFingerprint": state["fingerprint"],
        "origin": "user",
        "scopeNodeIds": scope,
        "operations": operations,
    })
    .to_string()
}

fn encoded(transaction: aic_core::Transaction) -> String {
    serde_json::to_string(&transaction).expect("encode transaction")
}

fn apply(project: &Project, transaction: &str) {
    project
        .session
        .apply_transaction(transaction)
        .expect("apply transaction");
}

/// `operation_insert` takes the node as the model sees it: a UUID instead of
/// the string form used in operation JSON.
fn insert(project: &Project, parent: &str, index: u32, operation_node: Value) -> String {
    let mut node = model_node(operation_node);
    node["parentId"] = json!(parent);
    let id = node["id"].to_string().trim_matches('"').to_owned();
    let transaction = transaction(
        project,
        &[parent.to_owned(), id.clone()],
        vec![json!({"type": "insertNode", "parentId": parent, "index": index, "node": node})],
    );
    apply(project, &transaction);
    id
}

fn model_node(mut value: Value) -> Value {
    let object = value.as_object_mut().expect("node object");
    object.insert("parentId".into(), Value::Null);
    for key in ["a11y", "bindings", "events", "propertyMeta"] {
        object.entry(key).or_insert_with(|| json!({}));
    }
    value
}

fn set(project: &Project, id: &str, path: &str, value: Value) {
    let transaction = transaction(
        project,
        &[id.to_owned()],
        vec![json!({"type": "setProperty", "nodeId": id, "path": path, "value": value})],
    );
    apply(project, &transaction);
}

fn text_node(name: &str, x: f64, y: f64) -> Value {
    json!({
        "id": Uuid::new_v4(),
        "kind": "text", "name": name, "children": [],
        "layout": {"x": x, "y": y, "w": 200.0, "h": 40.0},
        "style": {"fontSize": 16.0, "color": "#111827"},
        "content": {"text": name},
    })
}

fn frame_node(name: &str) -> Value {
    json!({
        "id": Uuid::new_v4(),
        "kind": "frame", "name": name, "children": [],
        "layout": {"x": 20.0, "y": 20.0, "w": 320.0, "h": 200.0},
        "style": {}, "content": {},
    })
}

fn image_node() -> Value {
    json!({
        "id": Uuid::new_v4(),
        "kind": "image", "name": "Картинка", "children": [],
        "layout": {"x": 40.0, "y": 40.0, "w": 120.0, "h": 90.0},
        "style": {"fit": "cover"}, "content": {},
    })
}

#[test]
fn grouping_siblings_is_one_undo_step() {
    let project = temp_project("group");
    let root = root_id(&project);
    let first = insert(&project, &root, 0, text_node("Заголовок", 0.0, 0.0));
    let second = insert(&project, &root, 1, text_node("Подпись", 0.0, 60.0));
    let before = revision(&project);
    let count = nodes(&project).len();
    project.session.save().expect("save before grouping");
    let transactions_before = history_undo_len(&project);

    let transaction = encoded(
        project
            .session
            .group_json(vec![first.clone(), second.clone()], None)
            .expect("group"),
    );
    apply(&project, &transaction);

    let group = nodes(&project)
        .into_iter()
        .find(|node| node["kind"] == "group")
        .expect("group node");
    let group_id = group["id"].as_str().expect("group id").to_owned();
    assert_eq!(group["children"].as_array().expect("children").len(), 2);
    assert!(revision(&project) > before);
    project.session.save().expect("save after grouping");
    assert_eq!(history_undo_len(&project), transactions_before + 1);

    project.session.undo().expect("undo");
    assert_eq!(nodes(&project).len(), count);
    assert!(find(&project, &group_id).is_none());
}

#[test]
fn ungrouping_keeps_children_where_they_were() {
    let project = temp_project("ungroup");
    let root = root_id(&project);
    let first = insert(&project, &root, 0, text_node("Заголовок", 0.0, 0.0));
    let second = insert(&project, &root, 1, text_node("Подпись", 0.0, 60.0));
    let transaction = encoded(
        project
            .session
            .group_json(vec![first.clone(), second.clone()], None)
            .expect("group"),
    );
    apply(&project, &transaction);
    let group = nodes(&project)
        .into_iter()
        .find(|node| node["kind"] == "group")
        .expect("group");
    let group_id = group["id"].as_str().expect("id").to_owned();
    let (offset_x, offset_y) = (
        group["layout"]["x"].as_f64().expect("x"),
        group["layout"]["y"].as_f64().expect("y"),
    );

    let transaction = encoded(project.session.ungroup_json(&group_id).expect("ungroup"));
    apply(&project, &transaction);

    assert!(find(&project, &group_id).is_none());
    let moved = find(&project, &first).expect("first");
    assert_eq!(moved["parentId"].as_str(), Some(root.as_str()));
    assert!((moved["layout"]["x"].as_f64().expect("x") - offset_x).abs() < 0.001);
    assert!((moved["layout"]["y"].as_f64().expect("y") - offset_y).abs() < 0.001);
    project.session.undo().expect("undo");
    assert!(find(&project, &group_id).is_some());
}

#[test]
fn duplicate_reissues_identifiers_and_keeps_children() {
    let project = temp_project("duplicate");
    let root = root_id(&project);
    let frame = insert(&project, &root, 0, frame_node("Карточка"));
    let label = insert(&project, &frame, 0, text_node("Заголовок", 8.0, 8.0));

    let transaction = encoded(
        project
            .session
            .paste_nodes_json(vec![frame.clone()], &root, None)
            .expect("paste"),
    );
    apply(&project, &transaction);

    let all = nodes(&project);
    assert_eq!(
        all.len(),
        5,
        "root, frame, label and the copy with its label"
    );
    assert_eq!(
        all.iter()
            .filter(|node| node["id"] == frame.as_str())
            .count(),
        1
    );
    assert_eq!(
        all.iter()
            .filter(|node| node["id"] == label.as_str())
            .count(),
        1
    );
    let copies: Vec<&Value> = all
        .iter()
        .filter(|node| node["id"] != frame.as_str() && node["parentId"] == root.as_str())
        .collect();
    assert_eq!(copies.len(), 1);
    let copy_id = copies[0]["id"].as_str().expect("copy id");
    let copy_child = all
        .iter()
        .find(|node| node["parentId"].as_str() == Some(copy_id))
        .expect("copied child");
    assert_ne!(copy_child["id"].as_str(), Some(label.as_str()));
    assert_eq!(copy_child["content"]["text"], "Заголовок");
}

#[test]
fn component_create_insert_and_break_round_trip() {
    let project = temp_project("components");
    let root = root_id(&project);
    let card = insert(&project, &root, 0, frame_node("Карточка"));
    let label = insert(&project, &card, 0, text_node("Заголовок", 8.0, 8.0));
    let component = Uuid::new_v4().to_string();

    let define = transaction(
        &project,
        std::slice::from_ref(&card),
        vec![
            json!({"type": "setComponent", "componentId": component, "rootId": card, "name": "Карточка"}),
        ],
    );
    apply(&project, &define);
    assert_eq!(
        snapshot(&project)["document"]["components"][0]["rootId"],
        card.as_str()
    );

    let transaction = encoded(
        project
            .session
            .insert_component_instance_json(&component, &root, None, 80.0, 80.0)
            .expect("insert instance"),
    );
    apply(&project, &transaction);
    let inserted = nodes(&project)
        .into_iter()
        .find(|node| {
            node["layout"]["x"].as_f64() == Some(80.0) && node["kind"] == "componentInstance"
        })
        .expect("new instance");
    let inserted_id = inserted["id"].as_str().expect("instance id").to_owned();
    let copied_label = inserted["children"][0]
        .as_str()
        .expect("copied label")
        .to_owned();
    assert_ne!(copied_label, label);
    assert_eq!(
        find(&project, &copied_label).expect("copy")["content"]["text"],
        "Заголовок"
    );

    let transaction = encoded(
        project
            .session
            .break_component_link_json(&inserted_id)
            .expect("break"),
    );
    apply(&project, &transaction);
    let broken = find(&project, &inserted_id).expect("broken");
    assert_eq!(broken["kind"], "frame");
    assert!(broken["children"].as_array().expect("children").len() == 1);
    let detached = broken["children"][0].as_str().expect("child").to_owned();
    assert_ne!(detached, copied_label);
    assert_eq!(find(&project, &detached).expect("detached")["kind"], "text");

    project.session.undo().expect("undo");
    assert_eq!(
        find(&project, &inserted_id).expect("instance")["kind"],
        "componentInstance"
    );
}

#[test]
fn defining_a_component_from_a_selection_replaces_it_with_an_instance() {
    let project = temp_project("components-selection");
    let root = root_id(&project);
    let first = insert(&project, &root, 0, text_node("Заголовок", 0.0, 0.0));
    let second = insert(&project, &root, 1, text_node("Подпись", 0.0, 60.0));

    let transaction = encoded(
        project
            .session
            .create_component_json(
                vec![first.clone(), second.clone()],
                Some("Группа".to_owned()),
            )
            .expect("create component"),
    );
    apply(&project, &transaction);
    assert_eq!(
        snapshot(&project)["document"]["components"]
            .as_array()
            .expect("components")
            .len(),
        1
    );
    let root_node = find(&project, &root).expect("root");
    let root_children = root_node["children"].as_array().expect("children");
    assert_eq!(root_children.len(), 1, "the selection becomes one instance");
    // The definition reuses the frame it created as its root.
    let group = find(&project, root_children[0].as_str().expect("instance id")).expect("frame");
    assert_eq!(group["kind"], "frame");
    assert_eq!(group["children"].as_array().expect("children").len(), 2);
    let nodes = nodes(&project);
    assert_eq!(nodes.len(), 4, "root, frame, two children");
    project.session.undo().expect("undo");
    assert!(find(&project, &first).is_some());
}

#[test]
fn restore_as_text_swaps_an_image_and_undo_brings_it_back() {
    let project = temp_project("restore");
    let root = root_id(&project);
    let image = insert(&project, &root, 0, image_node());
    let layout = find(&project, &image).expect("image")["layout"].clone();

    let replacement = json!({
        "id": image, "parentId": root, "kind": "text", "name": "Картинка",
        "children": [], "layout": layout, "hidden": false, "locked": false,
        "style": {"color": "#111827"}, "content": {"text": "Восстановленный текст"},
        "a11y": {}, "bindings": {}, "events": {}, "propertyMeta": {},
        "sourceBounds": null, "sourceDensity": null, "responsive": [],
    });
    let transaction = transaction(
        &project,
        &[image.clone(), root.clone()],
        vec![json!({"type": "swapNode", "node": replacement})],
    );
    apply(&project, &transaction);
    let swapped = find(&project, &image).expect("node");
    assert_eq!(swapped["kind"], "text");
    assert_eq!(swapped["content"]["text"], "Восстановленный текст");

    project.session.undo().expect("undo");
    assert_eq!(find(&project, &image).expect("image")["kind"], "image");
}

#[test]
fn restored_text_survives_save_and_reopen() {
    let project = temp_project("restore-reopen");
    let root = root_id(&project);
    let image = insert(&project, &root, 0, image_node());
    let layout = find(&project, &image).expect("image")["layout"].clone();
    let replacement = json!({
        "id": image, "parentId": root, "kind": "text", "name": "Картинка",
        "children": [], "layout": layout, "style": {}, "content": {"text": "Текст"},
        "a11y": {}, "bindings": {}, "events": {}, "propertyMeta": {},
    });
    let transaction = transaction(
        &project,
        &[image.clone(), root.clone()],
        vec![json!({"type": "swapNode", "node": replacement})],
    );
    apply(&project, &transaction);
    let path = project.dir.clone();
    project.session.save().expect("save");
    std::mem::forget(project);

    let session = open_project(&path).expect("reopen");
    session.save().expect("save after reopen");
    let value: Value = serde_json::from_str(&session.snapshot_json().expect("snapshot")).unwrap();
    let node = value["document"]["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|node| node["id"] == image.as_str())
        .expect("node")
        .clone();
    assert_eq!(node["kind"], "text");
    drop(session);
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn token_change_is_one_undo_step() {
    let project = temp_project("tokens");
    let root = root_id(&project);
    let label = insert(&project, &root, 0, text_node("Заголовок", 0.0, 0.0));
    let token = Uuid::new_v4().to_string();

    let transaction = transaction(
        &project,
        &[root.clone(), label.clone()],
        vec![json!({
            "type": "setToken",
            "token": {"id": token, "name": "brand", "value": "#1d4ed8"},
        })],
    );
    apply(&project, &transaction);
    assert_eq!(
        snapshot(&project)["document"]["tokens"]
            .as_array()
            .expect("tokens")
            .len(),
        1
    );
    set(&project, &label, "/style/color", json!("#1d4ed8"));
    assert_eq!(
        find(&project, &label).expect("label")["style"]["color"],
        "#1d4ed8"
    );

    project.session.undo().expect("undo");
    project.session.undo().expect("undo");
    assert_eq!(
        find(&project, &label).expect("label")["style"]["color"],
        "#111827"
    );
    assert!(snapshot(&project)["document"]["tokens"]
        .as_array()
        .expect("tokens")
        .is_empty());
}

#[test]
fn operations_on_unknown_nodes_are_refused_without_changing_the_document() {
    let project = temp_project("refusals");
    let root = root_id(&project);
    let before = snapshot(&project);
    let missing = Uuid::new_v4().to_string();
    let error = project
        .session
        .group_json(vec![missing.clone(), root.clone()], None)
        .expect_err("unknown node");
    assert!(error.to_string().contains("Unknown node"), "{error}");
    assert_eq!(before["document"], snapshot(&project)["document"]);
    let error = project
        .session
        .ungroup_json(&root)
        .expect_err("the page root is not a group");
    assert!(!error.to_string().is_empty());
    let error = project
        .session
        .break_component_link_json(&root)
        .expect_err("the root is not an instance");
    assert!(!error.to_string().is_empty());
}

#[test]
fn swap_node_must_keep_parent_and_drop_children() {
    let project = temp_project("swap-rules");
    let root = root_id(&project);
    let frame = insert(&project, &root, 0, frame_node("Карточка"));
    insert(&project, &frame, 0, text_node("Заголовок", 0.0, 0.0));
    let other = insert(&project, &root, 1, frame_node("Другая"));
    let replacement = |parent: &str| {
        json!({"type": "swapNode", "node": {
            "id": frame, "parentId": parent, "kind": "text", "name": "x",
            "children": [], "layout": {"x": 0.0, "y": 0.0, "w": 10.0, "h": 10.0},
            "style": {}, "content": {}, "a11y": {}, "bindings": {}, "events": {},
            "propertyMeta": {},
        }})
    };
    let with_children = transaction(
        &project,
        &[frame.clone(), root.clone()],
        vec![replacement(&root)],
    );
    let error = project
        .session
        .apply_transaction(&with_children)
        .expect_err("children");
    assert!(error.to_string().contains("cyclic"), "{error}");

    // Swapping into another parent is refused: the old parent still lists the
    // node, so the replacement would be reached twice.
    let moved_parent = transaction(
        &project,
        &[frame.clone(), root.clone(), other.clone()],
        vec![replacement(&other)],
    );
    let error = project
        .session
        .apply_transaction(&moved_parent)
        .expect_err("parent");
    assert!(!error.to_string().is_empty(), "swap into another parent");
    assert_eq!(find(&project, &frame).expect("frame")["kind"], "frame");

    let label = nodes(&project)
        .into_iter()
        .find(|node| node["parentId"] == frame.as_str())
        .expect("label")["id"]
        .as_str()
        .expect("label id")
        .to_owned();
    let text = json!({"type": "swapNode", "node": {
        "id": label, "parentId": frame, "kind": "text", "name": "Заголовок",
        "children": [], "layout": {"x": 0.0, "y": 0.0, "w": 100.0, "h": 20.0},
        "style": {}, "content": {"text": "Восстановленный"}, "a11y": {},
        "bindings": {}, "events": {}, "propertyMeta": {},
    }});
    let valid = transaction(&project, &[label.clone(), frame.clone()], vec![text]);
    apply(&project, &valid);
    assert_eq!(
        find(&project, &label).expect("label")["content"]["text"],
        "Восстановленный"
    );
    assert_eq!(find(&project, &frame).expect("frame")["kind"], "frame");
}

#[test]
fn inspector_paths_the_sections_write_are_all_accepted() {
    let project = temp_project("inspector");
    let root = root_id(&project);
    let frame = insert(&project, &root, 0, frame_node("Карточка"));
    let text = insert(&project, &frame, 0, text_node("Заголовок", 0.0, 0.0));
    let image = insert(&project, &root, 1, image_node());
    let rounds: &[(&str, &str, Value)] = &[
        (&frame, "/layout/x", json!(48.5)),
        (&frame, "/layout/w", json!(280.0)),
        (&frame, "/layout/minW", json!(100.0)),
        (&frame, "/layout/maxH", json!(900.0)),
        (&frame, "/style/radius", json!(12.0)),
        (&frame, "/style/opacity", json!(0.8)),
        (&frame, "/style/rotate", json!(15.0)),
        (&frame, "/style/fill", json!("#ffffff")),
        (
            &frame,
            "/style/stroke",
            json!({"color": "#111827", "width": 2.0}),
        ),
        (
            &frame,
            "/style/shadows",
            json!([{"x": 0.0, "y": 2.0, "blur": 8.0, "spread": 0.0, "color": "#000000"}]),
        ),
        (&frame, "/style/overflow", json!("hidden")),
        (&text, "/style/fontSize", json!(18.0)),
        (&text, "/style/textAlign", json!("center")),
        (&text, "/style/textWrap", json!("word")),
        (&text, "/style/maxLines", json!(2)),
        (&text, "/content/text", json!("Привет")),
        (&image, "/style/fit", json!("contain")),
        (&image, "/hidden", json!(true)),
        (&image, "/locked", json!(true)),
    ];
    for (id, path, value) in rounds {
        set(&project, id, path, value.clone());
    }
    let frame_after = find(&project, &frame).expect("frame");
    assert_eq!(frame_after["layout"]["x"].as_f64(), Some(48.5));
    assert_eq!(frame_after["style"]["radius"].as_f64(), Some(12.0));
    assert_eq!(frame_after["style"]["stroke"]["width"].as_f64(), Some(2.0));
    let text_after = find(&project, &text).expect("text");
    assert_eq!(text_after["content"]["text"], "Привет");
    assert_eq!(text_after["style"]["maxLines"].as_u64(), Some(2));
    let image_after = find(&project, &image).expect("image");
    assert_eq!(image_after["hidden"], true);
    assert_eq!(image_after["style"]["fit"], "contain");
    for _ in 0..rounds.len() {
        project.session.undo().expect("undo");
    }
    assert_ne!(
        find(&project, &image).expect("image")["hidden"],
        true,
        "undo clears the flag"
    );
    assert_eq!(
        find(&project, &text).expect("text")["content"]["text"],
        "Заголовок"
    );
}

#[test]
fn agent_lock_paths_the_inspector_toggle_writes_are_accepted() {
    let project = temp_project("agent-lock");
    let root = root_id(&project);
    let frame = insert(&project, &root, 0, frame_node("Карточка"));
    set(
        &project,
        &frame,
        "/propertyMeta/~1layout/agentLocked",
        json!(true),
    );
    set(
        &project,
        &frame,
        "/propertyMeta/~1style/agentLocked",
        json!(true),
    );
    let locked = find(&project, &frame).expect("frame");
    assert_eq!(locked["propertyMeta"]["/layout"]["agentLocked"], true);
    assert_eq!(locked["propertyMeta"]["/style"]["agentLocked"], true);
    set(
        &project,
        &frame,
        "/propertyMeta/~1layout/agentLocked",
        json!(false),
    );
    assert_eq!(
        find(&project, &frame).expect("frame")["propertyMeta"]["/layout"]["agentLocked"],
        false
    );
}

#[test]
fn vector_points_move_as_one_transaction_over_the_whole_vector() {
    let project = temp_project("vector");
    let root = root_id(&project);
    let mut vector = frame_node("Логотип");
    vector["kind"] = json!("vector");
    vector["content"] = json!({"vector": {"paths": [[{"type": "moveTo", "x": 0.0, "y": 0.0},
                                                     {"type": "lineTo", "x": 40.0, "y": 0.0},
                                                     {"type": "close"}]]}});
    let id = insert(&project, &root, 0, vector);
    let moved = json!({"paths": [[{"type": "moveTo", "x": 10.0, "y": 5.0},
                                  {"type": "lineTo", "x": 40.0, "y": 0.0},
                                  {"type": "close"}]]});
    set(&project, &id, "/content/vector", moved.clone());
    assert_eq!(
        find(&project, &id).expect("vector")["content"]["vector"],
        moved
    );
    project.session.undo().expect("undo");
    assert_eq!(
        find(&project, &id).expect("vector")["content"]["vector"]["paths"][0][0]["x"],
        0.0
    );
}

#[test]
fn image_replacement_points_the_node_at_the_new_asset() {
    let project = temp_project("image");
    let root = root_id(&project);
    let image = insert(&project, &root, 0, image_node());
    let path = image_file();
    let imported: Value =
        serde_json::from_str(&project.session.import_asset(&path).expect("import")).unwrap();
    let asset = imported["id"].as_str().expect("asset id").to_owned();
    set(&project, &image, "/content/assetId", json!(asset));
    assert_eq!(
        find(&project, &image).expect("image")["content"]["assetId"],
        asset.as_str()
    );
    set(&project, &image, "/style/fit", json!("contain"));
    assert_eq!(
        find(&project, &image).expect("image")["style"]["fit"],
        "contain"
    );
    let unknown = Uuid::new_v4().to_string();
    let before = snapshot(&project);
    let refused = transaction(
        &project,
        std::slice::from_ref(&image),
        vec![
            json!({"type": "setProperty", "nodeId": image, "path": "/content/assetId", "value": unknown}),
        ],
    );
    assert!(
        project.session.apply_transaction(&refused).is_err(),
        "an image cannot point at a missing asset"
    );
    assert_eq!(before["document"], snapshot(&project)["document"]);
    let _ = std::fs::remove_file(&path);
}

fn image_file() -> std::path::PathBuf {
    use image::{ExtendedColorType, ImageEncoder};
    let path = std::env::temp_dir().join(format!("aic-gestures-{}.png", std::process::id()));
    let file = std::fs::File::create(&path).expect("create png");
    image::codecs::png::PngEncoder::new(file)
        .write_image(&[128_u8; 16], 2, 2, ExtendedColorType::Rgba8)
        .expect("write png");
    path
}

#[test]
fn transactions_on_a_stale_base_are_refused() {
    let project = temp_project("stale");
    let root = root_id(&project);
    let label = insert(&project, &root, 0, text_node("Заголовок", 0.0, 0.0));
    let stale = transaction(
        &project,
        std::slice::from_ref(&label),
        vec![json!({"type": "setProperty", "nodeId": label, "path": "/layout/x", "value": 10.0})],
    );
    set(&project, &label, "/layout/y", json!(30.0));
    let error = project
        .session
        .apply_transaction(&stale)
        .expect_err("stale base");
    assert!(error.to_string().contains("revision"), "{error}");
    assert_eq!(
        find(&project, &label).expect("label")["layout"]["x"].as_f64(),
        Some(0.0)
    );
}
