use std::collections::{HashMap, HashSet};

use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use super::domain::{ActionKind, Binding, Bounds, FeatureMap, Flow, ResponsiveOverride, SizeMode};
use super::validation::{identifier, node_supported, property_kind};
use super::{
    CoreError, Document, Node, NodeKind, Operation, Origin, PropertyMeta, Result, Transaction,
};

fn scoped(transaction: &Transaction, id: Uuid) -> Result<()> {
    if transaction.scope_node_ids.contains(&id) {
        Ok(())
    } else {
        Err(CoreError::invalid(format!(
            "Node {id} is outside transaction scope"
        )))
    }
}

fn related_path(a: &str, b: &str) -> bool {
    a == b
        || (a.starts_with(b) && a.as_bytes().get(b.len()) == Some(&b'/'))
        || (b.starts_with(a) && b.as_bytes().get(a.len()) == Some(&b'/'))
}

fn unlocked(node: &Node, path: &str, origin: Origin) -> Result<()> {
    if matches!(origin, Origin::Agent)
        && node
            .property_meta
            .iter()
            .any(|(key, meta)| meta.agent_locked && related_path(key, path))
    {
        return Err(CoreError::locked(format!("Node {}: {path}", node.id)));
    }
    Ok(())
}

fn mark(node: &mut Node, path: &str, origin: Origin, revision: u64) {
    let agent_locked = node
        .property_meta
        .get(path)
        .is_some_and(|meta| meta.agent_locked);
    node.property_meta.insert(
        path.to_owned(),
        PropertyMeta {
            origin,
            last_revision: revision,
            agent_locked,
        },
    );
}

fn escaped(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn decode_token(token: &str) -> Result<String> {
    let mut result = String::new();
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '/' => return Err(CoreError::invalid("Unescaped slash in metadata path")),
            '~' => match chars.next() {
                Some('0') => result.push('~'),
                Some('1') => result.push('/'),
                _ => return Err(CoreError::invalid("Invalid JSON Pointer escape")),
            },
            _ => result.push(ch),
        }
    }
    Ok(result)
}

fn lock_target(node: &Node, path: &str) -> Result<String> {
    let token = path
        .strip_prefix("/propertyMeta/")
        .and_then(|p| p.strip_suffix("/agentLocked"))
        .ok_or_else(|| CoreError::invalid("Invalid lock metadata path"))?;
    let target = decode_token(token)?;
    let supported = property_kind(node, &target).is_some()
        || matches!(
            target.as_str(),
            "/kind"
                | "/parentId"
                | "/children"
                | "/layout"
                | "/layout/flow"
                | "/layout/widthMode"
                | "/layout/heightMode"
                | "/sourceBounds"
                | "/sourceDensity"
                | "/responsive"
                | "/style"
                | "/content"
                | "/a11y"
                | "/events"
                | "/bindings"
        )
        || target.strip_prefix("/events/").is_some_and(identifier)
        || (target == "/content/assetId" && matches!(node.kind, NodeKind::Image))
        || (target == "/content/vector" && matches!(node.kind, NodeKind::Vector))
        || (matches!(target.as_str(), "/content/moduleId" | "/content/props")
            && matches!(node.kind, NodeKind::CodeComponent))
        || (target == "/content/componentId" && matches!(node.kind, NodeKind::ComponentInstance))
        || target
            .strip_prefix("/bindings/")
            .is_some_and(|p| decode_token(p).is_ok_and(|p| property_kind(node, &p).is_some()));
    if !supported {
        return Err(CoreError::unsupported(format!(
            "Cannot lock {target} on node {}",
            node.id
        )));
    }
    Ok(target)
}

fn node_index(document: &Document, id: Uuid) -> Result<usize> {
    document
        .nodes
        .iter()
        .position(|node| node.id == id)
        .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))
}

fn number(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| CoreError::invalid("Expected a finite number"))
}

fn optional<T: DeserializeOwned>(value: &Value) -> Result<Option<T>> {
    if value.is_null() {
        Ok(None)
    } else {
        serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|e| CoreError::invalid(e.to_string()))
    }
}

fn known_mut<T>(map: &mut FeatureMap<T>) -> Result<&mut T> {
    map.known_mut()
        .ok_or_else(|| CoreError::unsupported("Node contains unsupported content"))
}

fn set_property(node: &mut Node, path: &str, value: &Value) -> Result<()> {
    match path {
        "/name" => {
            node.name = value
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| CoreError::invalid("Name must not be empty"))?
                .to_owned()
        }
        "/layout/x" => node.layout.x = number(value)?,
        "/layout/y" => node.layout.y = number(value)?,
        "/layout/w" => node.layout.w = number(value)?,
        "/layout/h" => node.layout.h = number(value)?,
        "/layout/flow" => node.layout.flow = optional::<Flow>(value)?,
        "/layout/widthMode" => node.layout.width_mode = optional::<SizeMode>(value)?,
        "/layout/heightMode" => node.layout.height_mode = optional::<SizeMode>(value)?,
        "/sourceBounds" => node.source_bounds = optional::<Bounds>(value)?,
        "/sourceDensity" => {
            node.source_density = if value.is_null() {
                None
            } else {
                Some(number(value)?)
            }
        }
        "/responsive" => {
            node.responsive = serde_json::from_value::<Vec<ResponsiveOverride>>(value.clone())
                .map_err(|e| CoreError::invalid(e.to_string()))?
        }
        "/style/fill" => known_mut(&mut node.style)?.fill = optional(value)?,
        "/style/color" => known_mut(&mut node.style)?.color = optional(value)?,
        "/style/opacity" => known_mut(&mut node.style)?.opacity = optional(value)?,
        "/style/fontFamily" => known_mut(&mut node.style)?.font_family = optional(value)?,
        "/style/fontFallback" => known_mut(&mut node.style)?.font_fallback = optional(value)?,
        "/style/fontAssetId" => known_mut(&mut node.style)?.font_asset_id = optional(value)?,
        "/style/fontSize" => known_mut(&mut node.style)?.font_size = optional(value)?,
        "/style/fontWeight" => known_mut(&mut node.style)?.font_weight = optional(value)?,
        "/style/fontStyle" => known_mut(&mut node.style)?.font_style = optional(value)?,
        "/style/lineHeight" => known_mut(&mut node.style)?.line_height = optional(value)?,
        "/style/letterSpacing" => known_mut(&mut node.style)?.letter_spacing = optional(value)?,
        "/style/textWrap" => known_mut(&mut node.style)?.text_wrap = optional(value)?,
        "/a11y/label" => known_mut(&mut node.a11y)?.label = optional(value)?,
        "/a11y/alt" => known_mut(&mut node.a11y)?.alt = optional(value)?,
        "/content/text" if matches!(node.kind, NodeKind::Text | NodeKind::Button) => {
            known_mut(&mut node.content)?.text = optional(value)?
        }
        "/content/placeholder" if matches!(node.kind, NodeKind::Input | NodeKind::Textarea) => {
            known_mut(&mut node.content)?.placeholder = optional(value)?
        }
        "/content/assetId" if matches!(node.kind, NodeKind::Image) => {
            known_mut(&mut node.content)?.asset_id = optional(value)?
        }
        "/content/vector" if matches!(node.kind, NodeKind::Vector) => {
            known_mut(&mut node.content)?.vector = optional(value)?
        }
        "/content/moduleId" if matches!(node.kind, NodeKind::CodeComponent) => {
            known_mut(&mut node.content)?.module_id = optional(value)?
        }
        "/content/componentId" if matches!(node.kind, NodeKind::ComponentInstance) => {
            known_mut(&mut node.content)?.component_id = optional(value)?
        }
        "/content/props" if matches!(node.kind, NodeKind::CodeComponent) => {
            known_mut(&mut node.content)?.props = optional(value)?
        }
        _ if path.strip_prefix("/events/").is_some_and(identifier) => {
            let key = &path["/events/".len()..];
            let events = known_mut(&mut node.events)?;
            if let Some(id) = optional::<Uuid>(value)? {
                events.insert(key.to_owned(), id);
            } else {
                events.remove(key);
            }
        }
        _ => {
            return Err(CoreError::unsupported(format!(
                "Unsupported property {path} on node {}",
                node.id
            )))
        }
    }
    Ok(())
}

fn upsert<T>(values: &mut Vec<T>, value: T, id: Uuid, get_id: impl Fn(&T) -> Uuid) {
    if let Some(existing) = values.iter_mut().find(|item| get_id(item) == id) {
        *existing = value;
    } else {
        values.push(value);
    }
    values.sort_by_key(get_id);
}

#[derive(Default)]
struct GlobalChanges {
    actions: HashSet<Uuid>,
    animations: HashSet<Uuid>,
    modules: HashSet<Uuid>,
    tokens: HashSet<Uuid>,
}

fn global_scope(document: &Document, tx: &Transaction, changes: &GlobalChanges) -> Result<()> {
    if changes.actions.is_empty()
        && changes.animations.is_empty()
        && changes.modules.is_empty()
        && changes.tokens.is_empty()
    {
        return Ok(());
    }
    for node in &document.nodes {
        let bindings = node
            .bindings
            .known()
            .ok_or_else(|| CoreError::unsupported("Unsupported node bindings"))?;
        for (path, binding) in bindings {
            if matches!(binding, Binding::Token { token_id } if changes.tokens.contains(token_id)) {
                scoped(tx, node.id)?;
                unlocked(node, path, tx.origin)?;
                unlocked(node, &format!("/bindings/{}", escaped(path)), tx.origin)?;
            }
        }
        let events = node
            .events
            .known()
            .ok_or_else(|| CoreError::unsupported("Unsupported node events"))?;
        for (event, id) in events {
            if changes.actions.contains(id)
                || document.actions.iter().any(|action| action.id == *id && matches!(&action.kind, ActionKind::PlayAnimation { animation_id } if changes.animations.contains(animation_id))) {
                scoped(tx, node.id)?;
                unlocked(node, &format!("/events/{event}"), tx.origin)?;
            }
        }
        let content = node
            .content
            .known()
            .ok_or_else(|| CoreError::unsupported("Unsupported node content"))?;
        if content
            .module_id
            .is_some_and(|id| changes.modules.contains(&id))
        {
            scoped(tx, node.id)?;
            unlocked(node, "/content", tx.origin)?;
            if matches!(tx.origin, Origin::Agent)
                && node.property_meta.values().any(|meta| meta.agent_locked)
            {
                return Err(CoreError::locked(format!(
                    "Code module affects a locked property on node {}",
                    node.id
                )));
            }
        }
    }
    for animation in &document.animations {
        if changes.animations.contains(&animation.id) {
            scoped(tx, animation.node_id)?;
            let node = &document.nodes[node_index(document, animation.node_id)?];
            unlocked(node, &animation.property, tx.origin)?;
        }
    }
    for action in &document.actions {
        if changes.actions.contains(&action.id) {
            match &action.kind {
                ActionKind::ToggleVisibility { node_id } => {
                    scoped(tx, *node_id)?;
                    let node = &document.nodes[node_index(document, *node_id)?];
                    unlocked(node, "/style", tx.origin)?;
                }
                ActionKind::PlayAnimation { animation_id } => {
                    if let Some(animation) = document
                        .animations
                        .iter()
                        .find(|item| item.id == *animation_id)
                    {
                        scoped(tx, animation.node_id)?;
                        let node = &document.nodes[node_index(document, animation.node_id)?];
                        unlocked(node, &animation.property, tx.origin)?;
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

impl Document {
    pub fn apply(&mut self, transaction: &Transaction) -> Result<()> {
        if transaction.operations.is_empty() || transaction.scope_node_ids.is_empty() {
            return Err(CoreError::invalid("Empty transaction or scope"));
        }
        if transaction
            .scope_node_ids
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != transaction.scope_node_ids.len()
        {
            return Err(CoreError::invalid("Duplicate node in transaction scope"));
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Revision overflow"))?;
        let mut global = GlobalChanges::default();
        for op in &transaction.operations {
            match op {
                Operation::UpsertAction { action } => {
                    global.actions.insert(action.id);
                }
                Operation::UpsertAnimation { animation } => {
                    global.animations.insert(animation.id);
                }
                Operation::UpsertCodeModule { code_module } => {
                    global.modules.insert(code_module.id);
                }
                Operation::SetToken { token } => {
                    global.tokens.insert(token.id);
                }
                _ => {}
            }
        }
        global_scope(self, transaction, &global)?;
        for op in &transaction.operations {
            match op {
                Operation::InsertNode {
                    parent_id,
                    index,
                    node,
                } => {
                    scoped(transaction, *parent_id)?;
                    scoped(transaction, node.id)?;
                    let parent = node_index(self, *parent_id)?;
                    unlocked(&self.nodes[parent], "/children", transaction.origin)?;
                    if self.nodes.iter().any(|n| n.id == node.id)
                        || node.parent_id != Some(*parent_id)
                        || !node.children.is_empty()
                        || !node.property_meta.is_empty()
                        || *index > self.nodes[parent].children.len()
                    {
                        return Err(CoreError::invalid(format!(
                            "Invalid insertion of node {}",
                            node.id
                        )));
                    }
                    if !node_supported(node) {
                        return Err(CoreError::unsupported(format!(
                            "Unsupported content on node {}",
                            node.id
                        )));
                    }
                    let mut new_node = node.as_ref().clone();
                    for path in [
                        "/kind",
                        "/name",
                        "/parentId",
                        "/layout/x",
                        "/layout/y",
                        "/layout/w",
                        "/layout/h",
                    ] {
                        mark(&mut new_node, path, transaction.origin, revision);
                    }
                    let style = node
                        .style
                        .known()
                        .ok_or_else(|| CoreError::unsupported("Unsupported node style"))?;
                    let content = node
                        .content
                        .known()
                        .ok_or_else(|| CoreError::unsupported("Unsupported node content"))?;
                    let a11y = node
                        .a11y
                        .known()
                        .ok_or_else(|| CoreError::unsupported("Unsupported node accessibility"))?;
                    for (present, path) in [
                        (style.fill.is_some(), "/style/fill"),
                        (style.color.is_some(), "/style/color"),
                        (style.opacity.is_some(), "/style/opacity"),
                        (style.font_family.is_some(), "/style/fontFamily"),
                        (style.font_fallback.is_some(), "/style/fontFallback"),
                        (style.font_asset_id.is_some(), "/style/fontAssetId"),
                        (style.font_size.is_some(), "/style/fontSize"),
                        (style.font_weight.is_some(), "/style/fontWeight"),
                        (style.font_style.is_some(), "/style/fontStyle"),
                        (style.line_height.is_some(), "/style/lineHeight"),
                        (style.letter_spacing.is_some(), "/style/letterSpacing"),
                        (style.text_wrap.is_some(), "/style/textWrap"),
                        (content.text.is_some(), "/content/text"),
                        (content.placeholder.is_some(), "/content/placeholder"),
                        (content.asset_id.is_some(), "/content/assetId"),
                        (content.module_id.is_some(), "/content/moduleId"),
                        (content.component_id.is_some(), "/content/componentId"),
                        (content.vector.is_some(), "/content/vector"),
                        (content.props.is_some(), "/content/props"),
                        (a11y.label.is_some(), "/a11y/label"),
                        (a11y.alt.is_some(), "/a11y/alt"),
                    ] {
                        if present {
                            mark(&mut new_node, path, transaction.origin, revision);
                        }
                    }
                    for key in node
                        .events
                        .known()
                        .ok_or_else(|| CoreError::unsupported("Unsupported node events"))?
                        .keys()
                    {
                        mark(
                            &mut new_node,
                            &format!("/events/{}", escaped(key)),
                            transaction.origin,
                            revision,
                        );
                    }
                    for path in node
                        .bindings
                        .known()
                        .ok_or_else(|| CoreError::unsupported("Unsupported node bindings"))?
                        .keys()
                    {
                        mark(
                            &mut new_node,
                            &format!("/bindings/{}", escaped(path)),
                            transaction.origin,
                            revision,
                        );
                    }
                    for (present, path) in [
                        (new_node.layout.flow.is_some(), "/layout/flow"),
                        (new_node.layout.width_mode.is_some(), "/layout/widthMode"),
                        (new_node.layout.height_mode.is_some(), "/layout/heightMode"),
                        (new_node.source_bounds.is_some(), "/sourceBounds"),
                        (new_node.source_density.is_some(), "/sourceDensity"),
                        (!new_node.responsive.is_empty(), "/responsive"),
                    ] {
                        if present {
                            mark(&mut new_node, path, transaction.origin, revision);
                        }
                    }
                    self.nodes[parent].children.insert(*index, node.id);
                    mark(
                        &mut self.nodes[parent],
                        "/children",
                        transaction.origin,
                        revision,
                    );
                    self.nodes.push(new_node);
                }
                Operation::RemoveNode { node_id } => {
                    scoped(transaction, *node_id)?;
                    let index = node_index(self, *node_id)?;
                    let parent_id = self.nodes[index].parent_id.ok_or_else(|| {
                        CoreError::invalid(format!("Cannot remove page root {node_id}"))
                    })?;
                    scoped(transaction, parent_id)?;
                    let parent = node_index(self, parent_id)?;
                    unlocked(&self.nodes[parent], "/children", transaction.origin)?;
                    let mut removed = HashSet::new();
                    let mut stack = vec![*node_id];
                    while let Some(id) = stack.pop() {
                        scoped(transaction, id)?;
                        if !removed.insert(id) {
                            return Err(CoreError::invalid("Node cycle"));
                        }
                        let node = &self.nodes[node_index(self, id)?];
                        if matches!(transaction.origin, Origin::Agent)
                            && node.property_meta.values().any(|meta| meta.agent_locked)
                        {
                            return Err(CoreError::locked(format!(
                                "Node {id} contains a locked property"
                            )));
                        }
                        stack.extend(node.children.iter().copied());
                    }
                    self.nodes[parent].children.retain(|id| *id != *node_id);
                    mark(
                        &mut self.nodes[parent],
                        "/children",
                        transaction.origin,
                        revision,
                    );
                    self.nodes.retain(|node| !removed.contains(&node.id));
                }
                Operation::MoveNode {
                    node_id,
                    parent_id,
                    index,
                } => {
                    scoped(transaction, *node_id)?;
                    scoped(transaction, *parent_id)?;
                    let child = node_index(self, *node_id)?;
                    let old_id = self.nodes[child].parent_id.ok_or_else(|| {
                        CoreError::invalid(format!("Cannot move page root {node_id}"))
                    })?;
                    scoped(transaction, old_id)?;
                    let old = node_index(self, old_id)?;
                    let new = node_index(self, *parent_id)?;
                    unlocked(&self.nodes[child], "/parentId", transaction.origin)?;
                    unlocked(&self.nodes[old], "/children", transaction.origin)?;
                    unlocked(&self.nodes[new], "/children", transaction.origin)?;
                    let mut cursor = Some(*parent_id);
                    while let Some(id) = cursor {
                        if id == *node_id {
                            return Err(CoreError::invalid(format!(
                                "Cannot move node {node_id} into itself or a descendant"
                            )));
                        }
                        cursor = self.nodes[node_index(self, id)?].parent_id;
                    }
                    let max = self.nodes[new].children.len() - usize::from(old_id == *parent_id);
                    if *index > max {
                        return Err(CoreError::invalid(format!(
                            "Invalid child index for node {parent_id}"
                        )));
                    }
                    self.nodes[old].children.retain(|id| id != node_id);
                    self.nodes[new].children.insert(*index, *node_id);
                    self.nodes[child].parent_id = Some(*parent_id);
                    mark(
                        &mut self.nodes[old],
                        "/children",
                        transaction.origin,
                        revision,
                    );
                    mark(
                        &mut self.nodes[new],
                        "/children",
                        transaction.origin,
                        revision,
                    );
                    mark(
                        &mut self.nodes[child],
                        "/parentId",
                        transaction.origin,
                        revision,
                    );
                }
                Operation::SetProperty {
                    node_id,
                    path,
                    value,
                } => {
                    scoped(transaction, *node_id)?;
                    let index = node_index(self, *node_id)?;
                    let node = &mut self.nodes[index];
                    if path.starts_with("/propertyMeta/") {
                        if !matches!(transaction.origin, Origin::User) {
                            return Err(CoreError::locked(format!(
                                "Only the user can change agent locks on node {node_id}"
                            )));
                        }
                        let target = lock_target(node, path)?;
                        let agent_locked = value.as_bool().ok_or_else(|| {
                            CoreError::invalid(format!("Expected a boolean lock on node {node_id}"))
                        })?;
                        node.property_meta.insert(
                            target,
                            PropertyMeta {
                                origin: Origin::User,
                                last_revision: revision,
                                agent_locked,
                            },
                        );
                    } else {
                        unlocked(node, path, transaction.origin)?;
                        if node
                            .bindings
                            .known()
                            .ok_or_else(|| CoreError::unsupported("Unsupported node bindings"))?
                            .contains_key(path)
                        {
                            return Err(CoreError::invalid(format!(
                                "Remove binding before setting {path} on node {node_id}"
                            )));
                        }
                        set_property(node, path, value)?;
                        mark(node, path, transaction.origin, revision);
                    }
                }
                Operation::SetBinding {
                    node_id,
                    path,
                    binding,
                } => {
                    scoped(transaction, *node_id)?;
                    let index = node_index(self, *node_id)?;
                    let node = &mut self.nodes[index];
                    if property_kind(node, path).is_none() {
                        return Err(CoreError::unsupported(format!(
                            "Cannot bind {path} on node {node_id}"
                        )));
                    }
                    let binding_path = format!("/bindings/{}", escaped(path));
                    unlocked(node, path, transaction.origin)?;
                    unlocked(node, &binding_path, transaction.origin)?;
                    let bindings = known_mut(&mut node.bindings)?;
                    if let Some(binding) = binding {
                        bindings.insert(path.clone(), binding.clone());
                    } else {
                        bindings.remove(path);
                    }
                    mark(node, &binding_path, transaction.origin, revision);
                }
                Operation::UpsertAction { action } => {
                    upsert(&mut self.actions, action.clone(), action.id, |v| v.id)
                }
                Operation::UpsertAnimation { animation } => {
                    upsert(&mut self.animations, animation.clone(), animation.id, |v| {
                        v.id
                    })
                }
                Operation::UpsertCodeModule { code_module } => upsert(
                    &mut self.code_modules,
                    code_module.clone(),
                    code_module.id,
                    |v| v.id,
                ),
                Operation::SetToken { token } => {
                    upsert(&mut self.tokens, token.clone(), token.id, |v| v.id)
                }
                Operation::SetViewport {
                    node_id,
                    width,
                    height,
                } => {
                    scoped(transaction, *node_id)?;
                    let index = node_index(self, *node_id)?;
                    let node = &mut self.nodes[index];
                    if !matches!(node.kind, NodeKind::Frame) || node.parent_id.is_some() {
                        return Err(CoreError::invalid(format!(
                            "Viewport must be a root frame: {node_id}"
                        )));
                    }
                    for path in ["/layout/w", "/layout/h"] {
                        unlocked(node, path, transaction.origin)?;
                        if node
                            .bindings
                            .known()
                            .ok_or_else(|| CoreError::unsupported("Unsupported node bindings"))?
                            .contains_key(path)
                        {
                            return Err(CoreError::invalid(format!(
                                "Remove binding before setting {path} on node {node_id}"
                            )));
                        }
                    }
                    node.layout.w = *width;
                    node.layout.h = *height;
                    for path in ["/layout/w", "/layout/h"] {
                        mark(node, path, transaction.origin, revision);
                    }
                }
            }
        }
        if !transaction
            .scope_node_ids
            .iter()
            .any(|id| self.nodes.iter().any(|node| node.id == *id))
        {
            return Err(CoreError::invalid(
                "Transaction scope has no surviving node",
            ));
        }
        self.revision = revision;
        self.validate()?;
        if !self.editable() {
            return Err(CoreError::unsupported(
                "Transaction creates content not supported for editing",
            ));
        }
        global_scope(self, transaction, &global)
    }

    pub fn mark_restored_properties(&mut self, before: &Document) {
        let previous: HashMap<_, _> = before.nodes.iter().map(|node| (node.id, node)).collect();
        for node in &mut self.nodes {
            if let Some(old) = previous.get(&node.id) {
                let mut paths = Vec::new();
                let old_value =
                    serde_json::to_value(old).expect("Node serialization is infallible");
                let new_value =
                    serde_json::to_value(&*node).expect("Node serialization is infallible");
                changed_paths("", &old_value, &new_value, &mut paths);
                let lock_changes: Vec<_> = old
                    .property_meta
                    .keys()
                    .chain(node.property_meta.keys())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .filter_map(|path| {
                        let old_locked =
                            old.property_meta.get(path).is_some_and(|m| m.agent_locked);
                        let new_locked =
                            node.property_meta.get(path).is_some_and(|m| m.agent_locked);
                        (old_locked != new_locked)
                            .then(|| (String::from(path.as_str()), new_locked))
                    })
                    .collect();
                node.property_meta = old.property_meta.clone();
                for path in paths {
                    mark(node, &path, Origin::User, self.revision);
                }
                for (path, agent_locked) in lock_changes {
                    node.property_meta.insert(
                        path,
                        PropertyMeta {
                            origin: Origin::User,
                            last_revision: self.revision,
                            agent_locked,
                        },
                    );
                }
            }
        }
    }
}

fn changed_paths(prefix: &str, before: &Value, after: &Value, paths: &mut Vec<String>) {
    if before == after {
        return;
    }
    if let (Some(left), Some(right)) = (before.as_object(), after.as_object()) {
        let keys: HashSet<_> = left.keys().chain(right.keys()).collect();
        for key in keys {
            if prefix.is_empty() && key == "propertyMeta" {
                continue;
            }
            let path = format!("{prefix}/{}", escaped(key));
            match (left.get(key), right.get(key)) {
                (Some(a), Some(b)) => changed_paths(&path, a, b, paths),
                _ => paths.push(path),
            }
        }
    } else if !prefix.is_empty() {
        paths.push(prefix.to_owned());
    }
}
