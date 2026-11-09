// Editor gestures become one transaction each so history records what the user
// did. Positions come from the shared layout pass, never from the caller.
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use serde_json::json;
use uuid::Uuid;

use super::domain::{Action, ActionKind, Animation, Binding, CodeModule, FeatureMap, NodeStyle};
use super::geometry::NodeBounds;
use super::{
    CoreError, Document, Layout, Node, NodeContent, NodeKind, Operation, Origin, Result,
    Transaction,
};

fn container_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Frame | NodeKind::Group | NodeKind::Button | NodeKind::ComponentInstance
    )
}

fn empty_node(id: Uuid, parent: Option<Uuid>, kind: NodeKind, name: &str, w: f64, h: f64) -> Node {
    Node {
        id,
        parent_id: parent,
        kind,
        name: name.into(),
        children: Vec::new(),
        layout: Layout {
            x: 0.0,
            y: 0.0,
            w,
            h,
            flow: None,
            width_mode: None,
            height_mode: None,
            min_w: None,
            min_h: None,
            max_w: None,
            max_h: None,
        },
        source_bounds: None,
        source_density: None,
        responsive: Vec::new(),
        hidden: false,
        locked: false,
        style: FeatureMap::Known(NodeStyle::default()),
        content: FeatureMap::Known(NodeContent::default()),
        a11y: FeatureMap::Known(Default::default()),
        bindings: FeatureMap::Known(Default::default()),
        events: FeatureMap::Known(Default::default()),
        property_meta: Default::default(),
    }
}

fn instance_node(id: Uuid, parent: Uuid, component_id: Uuid, w: f64, h: f64) -> Node {
    let mut node = empty_node(
        id,
        Some(parent),
        NodeKind::ComponentInstance,
        "Component",
        w,
        h,
    );
    if let FeatureMap::Known(content) = &mut node.content {
        content.component_id = Some(component_id);
    }
    node
}

impl Document {
    pub(crate) fn node(&self, id: Uuid) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub(crate) fn parent_of(&self, id: Uuid) -> Option<&Node> {
        self.node(id)
            .and_then(|node| node.parent_id)
            .and_then(|id| self.node(id))
    }

    fn child_index(&self, id: Uuid) -> usize {
        self.parent_of(id)
            .and_then(|parent| parent.children.iter().position(|child| *child == id))
            .unwrap_or(0)
    }
}

fn union(bounds: &[NodeBounds], ids: &HashSet<Uuid>) -> Result<(f64, f64, f64, f64)> {
    let mut union: Option<(f64, f64, f64, f64)> = None;
    for item in bounds.iter().filter(|item| ids.contains(&item.id)) {
        union = Some(match union {
            None => (item.x, item.y, item.x + item.w, item.y + item.h),
            Some((x0, y0, x1, y1)) => (
                x0.min(item.x),
                y0.min(item.y),
                x1.max(item.x + item.w),
                y1.max(item.y + item.h),
            ),
        });
    }
    union.ok_or_else(|| CoreError::invalid("Selection has no layout"))
}

pub(crate) fn transaction(
    document: &Document,
    scope: impl IntoIterator<Item = Uuid>,
    operations: Vec<Operation>,
    fingerprint: &str,
) -> Transaction {
    let mut scope_node_ids: Vec<Uuid> = scope.into_iter().collect();
    scope_node_ids.sort();
    Transaction {
        transaction_id: Uuid::new_v4(),
        base_revision: document.revision,
        base_fingerprint: fingerprint.to_owned(),
        origin: Origin::User,
        scope_node_ids,
        operations,
    }
}

/// Rejects the transaction before it reaches history if the tree cannot apply.
pub(crate) fn check(document: &Document, transaction: &Transaction) -> Result<()> {
    let mut next = document.clone();
    next.apply(transaction)?;
    Ok(())
}

/// Selected ids sharing one parent, without ancestor/descendant pairs and
/// without the page root, returned in sibling order.
fn selection(document: &Document, ids: &[Uuid]) -> Result<(Uuid, Vec<Uuid>)> {
    let selected: HashSet<Uuid> = ids.iter().copied().collect();
    if selected.len() != ids.len() {
        return Err(CoreError::invalid("Duplicate node in selection"));
    }
    let mut parent = None;
    for id in ids {
        let node = document
            .node(*id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))?;
        let current = node
            .parent_id
            .filter(|parent| document.node(*parent).is_some())
            .ok_or_else(|| CoreError::unsupported("The page root cannot be grouped"))?;
        if *parent.get_or_insert(current) != current {
            return Err(CoreError::unsupported(
                "Grouping requires nodes with the same parent",
            ));
        }
        let mut cursor = document.parent_of(current).and_then(|node| node.parent_id);
        while let Some(ancestor) = cursor {
            if selected.contains(&ancestor) {
                return Err(CoreError::unsupported(
                    "Selecting a node together with its ancestor is ambiguous",
                ));
            }
            cursor = document.node(ancestor).and_then(|node| node.parent_id);
        }
    }
    let parent = parent.expect("selection is not empty");
    let ordered: Vec<Uuid> = document.node(parent).map_or_else(Vec::new, |node| {
        node.children
            .iter()
            .filter(|id| selected.contains(id))
            .copied()
            .collect()
    });
    if ordered.len() != ids.len() {
        return Err(CoreError::invalid("Selection is not part of one parent"));
    }
    Ok((parent, ordered))
}

fn display_name(name: Option<&str>, fallback: &str) -> String {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn moves(
    document: &Document,
    members: &[Uuid],
    parent_id: Uuid,
    offset: (f64, f64),
) -> Vec<Operation> {
    let mut operations = Vec::with_capacity(members.len() * 3);
    for (position, id) in members.iter().enumerate() {
        let node = document.node(*id).expect("validated member");
        // Membership is written before the node's own parent is repointed.
        operations.push(Operation::SetProperty {
            node_id: *id,
            path: "/layout/x".into(),
            value: json!(node.layout.x + offset.0),
        });
        operations.push(Operation::SetProperty {
            node_id: *id,
            path: "/layout/y".into(),
            value: json!(node.layout.y + offset.1),
        });
        operations.push(Operation::MoveNode {
            node_id: *id,
            parent_id,
            index: position,
        });
    }
    operations
}

/// Wraps the selection in a new group spanning its computed bounds.
pub fn group(
    document: &Document,
    project_path: &Path,
    page_id: Uuid,
    ids: &[Uuid],
    name: Option<&str>,
) -> Result<Transaction> {
    let (parent_id, members) = selection(document, ids)?;
    let set: HashSet<Uuid> = members.iter().copied().collect();
    let (x0, y0, x1, y1) = union(&document.layout_bounds(project_path, page_id)?, &set)?;
    let group_id = Uuid::new_v4();
    let mut group = empty_node(
        group_id,
        Some(parent_id),
        NodeKind::Group,
        &display_name(name, "Group"),
        x1 - x0,
        y1 - y0,
    );
    group.layout.x = x0;
    group.layout.y = y0;
    let mut scope = vec![parent_id, group_id];
    scope.extend(members.iter().copied());
    let mut operations = Vec::with_capacity(members.len() * 3 + 1);
    operations.push(Operation::InsertNode {
        parent_id,
        index: document.child_index(members[0]),
        node: Box::new(group),
    });
    operations.extend(moves(document, &members, group_id, (-x0, -y0)));
    Ok(transaction(document, scope, operations, ""))
}

/// Removes a group and keeps its children at the same place on screen. A flow
/// child is refused because its position comes from the parent's layout.
pub fn ungroup(document: &Document, group_id: Uuid) -> Result<Transaction> {
    let group = document
        .node(group_id)
        .ok_or_else(|| CoreError::invalid(format!("Unknown node {group_id}")))?;
    if !matches!(group.kind, NodeKind::Group) {
        return Err(CoreError::unsupported(format!(
            "Node {group_id} is not a group"
        )));
    }
    let parent_id = group
        .parent_id
        .ok_or_else(|| CoreError::unsupported("The page root cannot be ungrouped"))?;
    if document
        .parent_of(group_id)
        .is_some_and(|parent| parent.layout.flow.is_some())
    {
        return Err(CoreError::unsupported(
            "Ungrouping a flow child would change layout",
        ));
    }
    let children = group.children.clone();
    let index = document.child_index(group_id);
    let (x0, y0) = (group.layout.x, group.layout.y);
    let mut scope = vec![parent_id, group_id];
    scope.extend(children.iter().copied());
    let mut operations = Vec::with_capacity(children.len() * 3 + 1);
    for (position, id) in children.iter().enumerate() {
        // A child stays where it is on screen; the parent's offset replaces
        // the group's own offset.
        let node = document.node(*id).expect("validated child");
        operations.push(Operation::SetProperty {
            node_id: *id,
            path: "/layout/x".into(),
            value: json!(node.layout.x + x0),
        });
        operations.push(Operation::SetProperty {
            node_id: *id,
            path: "/layout/y".into(),
            value: json!(node.layout.y + y0),
        });
        operations.push(Operation::MoveNode {
            node_id: *id,
            parent_id,
            index: index + position,
        });
    }
    operations.push(Operation::RemoveNode { node_id: group_id });
    Ok(transaction(document, scope, operations, ""))
}

/// Defines a component from the selection and resolves every definition
/// instance in the project into detached subtrees so each page renders what
/// the canvas shows. Linking instances to definitions would need renderer
/// support that does not exist yet.
pub fn create_component(
    document: &Document,
    project_path: &Path,
    page_id: Uuid,
    ids: &[Uuid],
    name: Option<&str>,
) -> Result<Transaction> {
    let (parent_id, members) = selection(document, ids)?;
    let set: HashSet<Uuid> = members.iter().copied().collect();
    let (x0, y0, x1, y1) = union(&document.layout_bounds(project_path, page_id)?, &set)?;
    if members.len() == 1
        && document
            .node(members[0])
            .is_some_and(|node| node.kind == NodeKind::ComponentInstance)
    {
        return Err(CoreError::unsupported(
            "Komponent не может состоять из экземпляра другого компонента",
        ));
    }
    let component_id = Uuid::new_v4();
    let component_name = display_name(name, "Component");
    let mut scope = vec![parent_id];
    scope.extend(members.iter().copied());
    let mut operations = Vec::new();
    let root_id = if members.len() == 1 {
        members[0]
    } else {
        let root_id = Uuid::new_v4();
        scope.push(root_id);
        let mut root = empty_node(
            root_id,
            Some(parent_id),
            NodeKind::Frame,
            &component_name,
            x1 - x0,
            y1 - y0,
        );
        root.layout.x = x0;
        root.layout.y = y0;
        operations.push(Operation::InsertNode {
            parent_id,
            index: document.child_index(members[0]),
            node: Box::new(root),
        });
        operations.extend(moves(document, &members, root_id, (-x0, -y0)));
        root_id
    };
    operations.push(Operation::SetComponent {
        component_id,
        root_id,
        name: Some(component_name),
    });
    Ok(transaction(document, scope, operations, ""))
}

/// Inserts an instance of an existing component into a container.
pub fn insert_component_instance(
    document: &Document,
    component_id: Uuid,
    parent_id: Uuid,
    index: Option<usize>,
    x: f64,
    y: f64,
) -> Result<Transaction> {
    let parent = document
        .node(parent_id)
        .ok_or_else(|| CoreError::invalid(format!("Unknown parent {parent_id}")))?;
    if !container_kind(parent.kind) {
        return Err(CoreError::invalid(format!(
            "Node {parent_id} cannot contain children"
        )));
    }
    let root_id = document
        .component_root(component_id)
        .ok_or_else(|| CoreError::unsupported(format!("Unknown component {component_id}")))?;
    let root = document
        .node(root_id)
        .ok_or_else(|| CoreError::invalid(format!("Missing component root {root_id}")))?;
    let id = Uuid::new_v4();
    let mut node = instance_node(id, parent_id, component_id, root.layout.w, root.layout.h);
    node.layout.x = x;
    node.layout.y = y;
    let index = index
        .unwrap_or(parent.children.len())
        .min(parent.children.len());
    let mut operations = vec![Operation::InsertNode {
        parent_id,
        index,
        node: Box::new(node),
    }];
    let resolved = resolve(document, component_id, &[id])?;
    let mut scope = vec![parent_id, id, root_id];
    scope.extend(resolved.iter().filter_map(|operation| match operation {
        Operation::InsertNode { node, .. } => Some(node.id),
        _ => None,
    }));
    operations.extend(resolved);
    Ok(transaction(document, scope, operations, ""))
}

/// Subtree ids with every parent before its children, `root` excluded.
fn subtree_of(document: &Document, root: Uuid) -> Result<Vec<Uuid>> {
    let mut order = Vec::new();
    let mut pending: Vec<Uuid> = document
        .node(root)
        .ok_or_else(|| CoreError::invalid(format!("Unknown node {root}")))?
        .children
        .iter()
        .rev()
        .copied()
        .collect();
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(CoreError::invalid(format!("Node cycle at {id}")));
        }
        let node = document
            .node(id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))?;
        order.push(id);
        pending.extend(node.children.iter().rev().copied());
    }
    Ok(order)
}

/// Fresh-id copies of `source`'s subtree below `root_id`, parents first.
fn detached(
    source: &Document,
    root_id: Uuid,
    order: &[Uuid],
) -> Result<(HashMap<Uuid, Uuid>, Vec<Node>)> {
    let map: HashMap<Uuid, Uuid> = order.iter().map(|id| (*id, Uuid::new_v4())).collect();
    let mut copies = Vec::with_capacity(order.len());
    for id in order {
        let node = source
            .node(*id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))?;
        let parent = node.parent_id.expect("subtree node has a parent");
        let mut copy = node.clone();
        copy.id = map[id];
        copy.parent_id = Some(if parent == root_id {
            root_id
        } else {
            *map.get(&parent)
                .ok_or_else(|| CoreError::invalid(format!("Node {id} precedes its parent")))?
        });
        copy.children = node.children.iter().map(|child| map[child]).collect();
        copy.property_meta.clear();
        copies.push(copy);
    }
    Ok((map, copies))
}

struct Copy {
    original: Uuid,
    map: HashMap<Uuid, Uuid>,
    copies: Vec<Node>,
    /// Set when the copy lands in an instance that is inserted by the same
    /// transaction and therefore is not part of `document` yet.
    fresh_instance: Option<Uuid>,
}

fn copies(document: &Document, root_id: Uuid) -> Result<Copy> {
    let order = subtree_of(document, root_id)?;
    let (map, copies) = detached(document, root_id, &order)?;
    Ok(Copy {
        original: root_id,
        map,
        copies,
        fresh_instance: None,
    })
}

fn resolved(copy: &Copy) -> Vec<Operation> {
    resolved_into(copy, copy.fresh_instance.expect("fresh instance"))
}

fn resolved_into(copy: &Copy, instance: Uuid) -> Vec<Operation> {
    let mut operations = Vec::with_capacity(copy.copies.len());
    let mut next: HashMap<Uuid, usize> = HashMap::new();
    for node in &copy.copies {
        let original = node
            .parent_id
            .and_then(|parent| {
                copy.map
                    .iter()
                    .find(|(_, fresh)| **fresh == parent)
                    .map(|(original, _)| *original)
            })
            .unwrap_or(copy.original);
        let parent_id = if original == copy.original {
            instance
        } else {
            copy.map[&original]
        };
        let mut node = node.clone();
        node.parent_id = Some(parent_id);
        let index = next.entry(parent_id).or_insert(0);
        operations.push(Operation::InsertNode {
            parent_id,
            index: *index,
            node: Box::new(node),
        });
        *index += 1;
    }
    operations
}

/// Operations that give every instance of `component_id`, and of components
/// nested inside it, a detached copy of its definition. Instances that already
/// carry children are kept as they are.
fn resolve(document: &Document, component_id: Uuid, inserted: &[Uuid]) -> Result<Vec<Operation>> {
    let mut operations = Vec::new();
    let mut pending = vec![component_id];
    let mut seen = HashSet::new();
    while let Some(component_id) = pending.pop() {
        if !seen.insert(component_id) {
            continue;
        }
        let root_id = document
            .component_root(component_id)
            .ok_or_else(|| CoreError::unsupported(format!("Unknown component {component_id}")))?;
        for id in subtree_of(document, root_id)? {
            if let Some(nested) = document
                .node(id)
                .and_then(|node| node.content.known())
                .and_then(|content| content.component_id)
            {
                pending.push(nested);
            }
        }
        let existing = document
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::ComponentInstance))
            .filter(|node| {
                node.content
                    .known()
                    .and_then(|content| content.component_id)
                    == Some(component_id)
                    && node.children.is_empty()
            })
            .map(|node| node.id);
        let fresh = inserted.iter().copied();
        for instance in existing.chain(fresh).collect::<Vec<_>>() {
            let mut copy = copies(document, root_id)?;
            copy.fresh_instance = Some(instance);
            operations.extend(resolved(&copy));
        }
    }
    Ok(operations)
}

/// Turns an instance into an independent frame holding a detached copy of its
/// component. Nested instances are resolved too, so the copy no longer depends
/// on definitions.
pub fn break_component_link(document: &Document, instance_id: Uuid) -> Result<Transaction> {
    let node = document
        .node(instance_id)
        .ok_or_else(|| CoreError::invalid(format!("Unknown node {instance_id}")))?;
    if !matches!(node.kind, NodeKind::ComponentInstance) {
        return Err(CoreError::unsupported(format!(
            "Node {instance_id} is not a component instance"
        )));
    }
    let parent_id = node
        .parent_id
        .ok_or_else(|| CoreError::unsupported("The page root cannot be broken"))?;
    let component_id = node
        .content
        .known()
        .and_then(|content| content.component_id)
        .ok_or_else(|| {
            CoreError::unsupported(format!("Instance {instance_id} has no component"))
        })?;
    let root_id = document
        .component_root(component_id)
        .ok_or_else(|| CoreError::unsupported(format!("Unknown component {component_id}")))?;
    let mut scope = vec![parent_id, instance_id, root_id];
    let mut operations = Vec::new();
    // The definitions decide the content, so whatever the instance holds now
    // is replaced by a fresh detached copy.
    for child in node.children.iter() {
        scope.push(*child);
        scope.extend(subtree_of(document, *child)?);
        operations.push(Operation::RemoveNode { node_id: *child });
    }
    let mut broken = node.clone();
    broken.kind = NodeKind::Frame;
    broken.children.clear();
    if let FeatureMap::Known(content) = &mut broken.content {
        content.component_id = None;
    }
    operations.push(Operation::SwapNode {
        node: Box::new(broken),
    });
    let copy = copies(document, root_id)?;
    operations.extend(resolved_into(&copy, instance_id));
    scope.extend(copy.map.values().copied());
    Ok(transaction(document, scope, operations, ""))
}

/// Nodes, actions, animations and code modules get fresh ids; assets are
/// shared by content hash. Agent locks are not copied.
pub(crate) fn paste(
    document: &Document,
    source: &Document,
    ids: &[Uuid],
    parent_id: Uuid,
    index: Option<usize>,
) -> Result<Transaction> {
    let parent = document
        .node(parent_id)
        .ok_or_else(|| CoreError::invalid(format!("Unknown parent {parent_id}")))?;
    if !container_kind(parent.kind) {
        return Err(CoreError::invalid(format!(
            "Node {parent_id} cannot contain children"
        )));
    }
    let mut order = Vec::new();
    for id in ids {
        if order.contains(id) {
            continue;
        }
        let root = source
            .node(*id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))?;
        if root.parent_id.is_none() {
            return Err(CoreError::unsupported("The page root cannot be pasted"));
        }
        if order.iter().any(|copied| is_inside(source, *id, *copied)) {
            continue;
        }
        order.push(*id);
        order.extend(subtree_of(source, *id)?);
    }
    let map: HashMap<Uuid, Uuid> = order.iter().map(|id| (*id, Uuid::new_v4())).collect();
    let mut actions: Vec<Action> = Vec::new();
    let mut animations: Vec<Animation> = Vec::new();
    let mut modules: Vec<CodeModule> = Vec::new();
    let mut operations = Vec::new();
    let mut next: HashMap<Uuid, usize> = HashMap::new();
    let base = index
        .unwrap_or(parent.children.len())
        .min(parent.children.len());
    for id in &order {
        let node = source
            .node(*id)
            .cloned()
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))?;
        let target = if ids.contains(id) {
            parent_id
        } else {
            let original = node.parent_id.expect("copied node has a parent");
            *map.get(&original).ok_or_else(|| {
                CoreError::unsupported(format!(
                    "Node {id} depends on content outside the copied selection"
                ))
            })?
        };
        let mut copy = node;
        copy.id = map[id];
        copy.parent_id = Some(target);
        // InsertNode takes an empty node, so children are re-attached below.
        copy.children.clear();
        copy.property_meta.clear();
        // For a cut/paste inside one project the caller's snapshot may lag
        // behind the session, so links resolve against the live document.
        let origin = if source.project_id == document.project_id {
            document
        } else {
            source
        };
        copy_links(
            &mut copy,
            origin,
            document,
            &map,
            &mut actions,
            &mut animations,
            &mut modules,
        )?;
        let slot = next
            .entry(target)
            .or_insert(if target == parent_id { base } else { 0 });
        let index = if ids.contains(id) {
            let index = *slot;
            *slot += 1;
            index
        } else {
            operations
                .iter()
                .filter(|op| matches!(op, Operation::InsertNode { parent_id, .. } if *parent_id == target))
                .count()
        };
        operations.push(Operation::InsertNode {
            parent_id: target,
            index,
            node: Box::new(copy),
        });
    }
    let mut scope = vec![parent_id];
    scope.extend(map.values().copied());
    scope.extend(actions.iter().map(|action| action.id));
    scope.extend(animations.iter().map(|animation| animation.node_id));
    scope.extend(
        document
            .nodes
            .iter()
            .filter(|node| {
                modules.iter().any(|module| {
                    node.content.known().and_then(|content| content.module_id) == Some(module.id)
                })
            })
            .map(|node| node.id),
    );
    for action in &actions {
        operations.push(Operation::UpsertAction {
            action: action.clone(),
        });
    }
    for animation in &animations {
        operations.push(Operation::UpsertAnimation {
            animation: animation.clone(),
        });
    }
    for code_module in &modules {
        operations.push(Operation::UpsertCodeModule {
            code_module: code_module.clone(),
        });
    }
    Ok(transaction(document, scope, operations, ""))
}

fn is_inside(source: &Document, node: Uuid, ancestor: Uuid) -> bool {
    let mut cursor = Some(node);
    while let Some(id) = cursor {
        if id == ancestor {
            return true;
        }
        cursor = source.parent_of(id).and_then(|node| node.parent_id);
    }
    false
}

/// Re-points references of a copy at the target project. Anything that cannot
/// be resolved is reported instead of being guessed.
fn copy_links(
    copy: &mut Node,
    source: &Document,
    target: &Document,
    map: &HashMap<Uuid, Uuid>,
    actions: &mut Vec<Action>,
    animations: &mut Vec<Animation>,
    modules: &mut Vec<CodeModule>,
) -> Result<()> {
    let FeatureMap::Known(events) = &mut copy.events else {
        return Err(CoreError::unsupported(format!(
            "Unsupported events on node {}",
            copy.id
        )));
    };
    let mut remapped = std::collections::BTreeMap::new();
    for (event, action_id) in events.iter() {
        let action = source
            .actions
            .iter()
            .find(|action| action.id == *action_id)
            .ok_or_else(|| CoreError::unsupported(format!("Missing action {action_id}")))?;
        let id = Uuid::new_v4();
        let kind = match &action.kind {
            ActionKind::Navigate { page_id } => {
                if !target.pages.iter().any(|page| page.id == *page_id) {
                    return Err(CoreError::unsupported(format!(
                        "Navigation target {page_id} is missing in this project"
                    )));
                }
                ActionKind::Navigate { page_id: *page_id }
            }
            ActionKind::ToggleVisibility { node_id } => ActionKind::ToggleVisibility {
                node_id: *map.get(node_id).ok_or_else(|| {
                    CoreError::unsupported("Toggle target is outside the copied selection")
                })?,
            },
            ActionKind::PlayAnimation { animation_id } => {
                let animation = source
                    .animations
                    .iter()
                    .find(|animation| animation.id == *animation_id)
                    .ok_or_else(|| CoreError::unsupported("Missing animation"))?;
                let node_id = *map.get(&animation.node_id).ok_or_else(|| {
                    CoreError::unsupported("Animation target is outside the copied selection")
                })?;
                let animation_id = Uuid::new_v4();
                let mut animated = animation.clone();
                animated.id = animation_id;
                animated.node_id = node_id;
                animations.push(animated);
                ActionKind::PlayAnimation { animation_id }
            }
            ActionKind::SetVariable { .. } | ActionKind::ToggleVariable { .. } => {
                return Err(CoreError::unsupported(
                    "Variable actions do not travel between projects",
                ))
            }
            ActionKind::OpenUrl { url } => ActionKind::OpenUrl { url: url.clone() },
        };
        actions.push(Action { id, kind });
        remapped.insert(event.clone(), id);
    }
    *events = remapped;
    let FeatureMap::Known(bindings) = &mut copy.bindings else {
        return Err(CoreError::unsupported(format!(
            "Unsupported bindings on node {}",
            copy.id
        )));
    };
    let mut rebound = std::collections::BTreeMap::new();
    for (path, binding) in bindings.iter() {
        let binding = match binding {
            Binding::Variable { .. } => {
                return Err(CoreError::unsupported(
                    "Variable bindings do not travel between projects",
                ))
            }
            Binding::Token { token_id } => {
                let token = source
                    .tokens
                    .iter()
                    .find(|token| token.id == *token_id)
                    .ok_or_else(|| CoreError::unsupported("Missing token"))?;
                match target.tokens.iter().find(|existing| {
                    existing.name == token.name && existing.value.kind() == token.value.kind()
                }) {
                    Some(existing) => Binding::Token {
                        token_id: existing.id,
                    },
                    None => {
                        return Err(CoreError::unsupported(format!(
                            "Token {} is missing in this project",
                            token.name
                        )))
                    }
                }
            }
        };
        rebound.insert(path.clone(), binding);
    }
    *bindings = rebound;
    if let FeatureMap::Known(content) = &mut copy.content {
        if let Some(asset_id) = content.asset_id {
            let asset = source
                .assets
                .iter()
                .find(|asset| asset.id == asset_id)
                .ok_or_else(|| CoreError::unsupported("Missing asset [A]"))?;
            content.asset_id = Some(
                target
                    .assets
                    .iter()
                    .find(|existing| existing.path == asset.path)
                    .map(|existing| existing.id)
                    .ok_or_else(|| {
                        CoreError::unsupported(format!(
                            "Asset {} is not part of this project",
                            asset.path
                        ))
                    })?,
            );
        }
        if let Some(module_id) = content.module_id {
            let module = source
                .code_modules
                .iter()
                .find(|module| module.id == module_id)
                .ok_or_else(|| CoreError::unsupported("Missing code module"))?;
            let new_id = match target
                .code_modules
                .iter()
                .find(|existing| existing.path == module.path)
            {
                Some(existing)
                    if existing.props == module.props && existing.events == module.events =>
                {
                    existing.id
                }
                Some(_) => {
                    return Err(CoreError::unsupported(format!(
                        "Code module path {} already holds a different component",
                        module.path
                    )))
                }
                None => {
                    let mut created = module.clone();
                    created.id = Uuid::new_v4();
                    modules.push(created.clone());
                    created.id
                }
            };
            content.module_id = Some(new_id);
        }
        if let Some(component_id) = content.component_id {
            return Err(CoreError::unsupported(format!(
                "Component instance {component_id} is not copied"
            )));
        }
    }
    Ok(())
}
