use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, Result};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub schema_version: u32,
    pub project_id: Uuid,
    pub revision: u64,
    pub pages: Vec<Page>,
    pub nodes: Vec<Node>,
    pub components: Vec<Value>,
    pub tokens: Vec<Value>,
    pub variables: Vec<Value>,
    pub actions: Vec<Value>,
    pub animations: Vec<Value>,
    pub assets: Vec<Value>,
    pub code_modules: Vec<Value>,
    pub targets: Vec<Value>,
    pub reference_baselines: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Page {
    pub id: Uuid,
    pub name: String,
    pub root_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub kind: NodeKind,
    pub name: String,
    pub children: Vec<Uuid>,
    pub layout: Layout,
    pub style: BTreeMap<String, Value>,
    pub content: BTreeMap<String, Value>,
    pub a11y: BTreeMap<String, Value>,
    pub bindings: BTreeMap<String, Value>,
    pub events: BTreeMap<String, Value>,
    pub property_meta: BTreeMap<String, PropertyMeta>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    Frame,
    Group,
    Text,
    Image,
    Vector,
    Button,
    Input,
    Textarea,
    Checkbox,
    Select,
    ComponentInstance,
    CodeComponent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyMeta {
    pub origin: Origin,
    pub last_revision: u64,
    pub agent_locked: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    Import,
    User,
    Agent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transaction {
    pub transaction_id: Uuid,
    pub base_revision: u64,
    pub base_fingerprint: String,
    pub origin: Origin,
    pub scope_node_ids: Vec<Uuid>,
    pub operations: Vec<Operation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Operation {
    SetProperty {
        node_id: Uuid,
        path: String,
        value: Value,
    },
    SetViewport {
        node_id: Uuid,
        width: f64,
        height: f64,
    },
}

impl Document {
    pub fn new(project_id: Uuid) -> Self {
        let root_id = Uuid::new_v4();
        Self {
            schema_version: 1,
            project_id,
            revision: 0,
            pages: vec![Page {
                id: Uuid::new_v4(),
                name: "Page 1".into(),
                root_id,
            }],
            nodes: vec![Node {
                id: root_id,
                parent_id: None,
                kind: NodeKind::Frame,
                name: "Frame".into(),
                children: Vec::new(),
                layout: Layout {
                    x: 0.0,
                    y: 0.0,
                    w: 1440.0,
                    h: 900.0,
                },
                style: BTreeMap::new(),
                content: BTreeMap::new(),
                a11y: BTreeMap::new(),
                bindings: BTreeMap::new(),
                events: BTreeMap::new(),
                property_meta: BTreeMap::new(),
            }],
            components: vec![],
            tokens: vec![],
            variables: vec![],
            actions: vec![],
            animations: vec![],
            assets: vec![],
            code_modules: vec![],
            targets: vec![],
            reference_baselines: vec![],
        }
    }

    // Until the corresponding editors and reference validation exist, do not
    // rewrite documents containing data that this implementation cannot edit.
    pub fn editable(&self) -> bool {
        self.pages.len() == 1
            && self.nodes.len() == 1
            && matches!(self.nodes[0].kind, NodeKind::Frame)
            && self.nodes[0].parent_id.is_none()
            && self.nodes[0].children.is_empty()
            && self.nodes[0].style.is_empty()
            && self.nodes[0].content.is_empty()
            && self.nodes[0].a11y.is_empty()
            && self.nodes[0].bindings.is_empty()
            && self.nodes[0].events.is_empty()
            && self.components.is_empty()
            && self.tokens.is_empty()
            && self.variables.is_empty()
            && self.actions.is_empty()
            && self.animations.is_empty()
            && self.assets.is_empty()
            && self.code_modules.is_empty()
            && self.targets.is_empty()
            && self.reference_baselines.is_empty()
    }

    pub fn mark_restored_properties(&mut self, before: &Document) {
        for node in &mut self.nodes {
            if let Some(previous) = before.nodes.iter().find(|old| old.id == node.id) {
                for (path, changed) in [
                    ("/name", node.name != previous.name),
                    ("/layout/x", node.layout.x != previous.layout.x),
                    ("/layout/y", node.layout.y != previous.layout.y),
                    ("/layout/w", node.layout.w != previous.layout.w),
                    ("/layout/h", node.layout.h != previous.layout.h),
                ] {
                    if changed {
                        let agent_locked = previous
                            .property_meta
                            .get(path)
                            .is_some_and(|meta| meta.agent_locked);
                        node.property_meta.insert(
                            path.into(),
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

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.pages.is_empty() {
            return Err(CoreError::invalid("Unsupported or empty document"));
        }
        let ids: HashSet<_> = self.nodes.iter().map(|n| n.id).collect();
        if ids.len() != self.nodes.len() {
            return Err(CoreError::invalid("Duplicate node ID"));
        }
        let page_ids: HashSet<_> = self.pages.iter().map(|p| p.id).collect();
        if page_ids.len() != self.pages.len() {
            return Err(CoreError::invalid("Duplicate page ID"));
        }
        for node in &self.nodes {
            let l = &node.layout;
            if ![l.x, l.y, l.w, l.h].into_iter().all(f64::is_finite) || l.w <= 0.0 || l.h <= 0.0 {
                return Err(CoreError::invalid(format!(
                    "Invalid dimensions for node {}",
                    node.id
                )));
            }
            if let Some(parent) = node.parent_id {
                let p = self
                    .nodes
                    .iter()
                    .find(|n| n.id == parent)
                    .ok_or_else(|| CoreError::invalid("Missing parent"))?;
                if p.children.iter().filter(|id| **id == node.id).count() != 1 {
                    return Err(CoreError::invalid("Inconsistent parent/child links"));
                }
            }
            let mut children = HashSet::new();
            for child in &node.children {
                let child_node = self
                    .nodes
                    .iter()
                    .find(|n| n.id == *child)
                    .ok_or_else(|| CoreError::invalid("Missing child"))?;
                if !children.insert(child) || child_node.parent_id != Some(node.id) {
                    return Err(CoreError::invalid("Inconsistent parent/child links"));
                }
            }
            let mut ancestors = HashSet::new();
            let mut cursor = Some(node.id);
            while let Some(id) = cursor {
                if !ancestors.insert(id) {
                    return Err(CoreError::invalid("Node cycle"));
                }
                cursor = self
                    .nodes
                    .iter()
                    .find(|n| n.id == id)
                    .and_then(|n| n.parent_id);
            }
        }
        for page in &self.pages {
            let root = self
                .nodes
                .iter()
                .find(|n| n.id == page.root_id)
                .ok_or_else(|| CoreError::invalid("Missing page root"))?;
            if root.parent_id.is_some() {
                return Err(CoreError::invalid("Page root has a parent"));
            }
        }
        Ok(())
    }

    pub fn apply(&mut self, transaction: &Transaction) -> Result<()> {
        if transaction.operations.is_empty() {
            return Err(CoreError::invalid("Empty transaction"));
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Revision overflow"))?;
        for op in &transaction.operations {
            let id = match op {
                Operation::SetProperty { node_id, .. } | Operation::SetViewport { node_id, .. } => {
                    *node_id
                }
            };
            if !transaction.scope_node_ids.contains(&id) {
                return Err(CoreError::invalid(format!(
                    "Node {id} is outside transaction scope"
                )));
            }
            let node = self
                .nodes
                .iter_mut()
                .find(|node| node.id == id)
                .ok_or_else(|| CoreError::invalid(format!("Unknown node {id}")))?;
            match op {
                Operation::SetProperty { path, value, .. } => {
                    if matches!(transaction.origin, Origin::Agent)
                        && node.property_meta.get(path).is_some_and(|m| m.agent_locked)
                    {
                        return Err(CoreError::locked(format!("{id}: {path}")));
                    }
                    match path.as_str() {
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
                        _ => {
                            return Err(CoreError::unsupported(format!(
                                "Property {path} is not supported"
                            )))
                        }
                    }
                    let locked = node.property_meta.get(path).is_some_and(|m| m.agent_locked);
                    node.property_meta.insert(
                        path.clone(),
                        PropertyMeta {
                            origin: transaction.origin,
                            last_revision: revision,
                            agent_locked: locked,
                        },
                    );
                }
                Operation::SetViewport { width, height, .. } => {
                    if !matches!(node.kind, NodeKind::Frame) || node.parent_id.is_some() {
                        return Err(CoreError::invalid("Viewport must be a root frame"));
                    }
                    for path in ["/layout/w", "/layout/h"] {
                        if matches!(transaction.origin, Origin::Agent)
                            && node.property_meta.get(path).is_some_and(|m| m.agent_locked)
                        {
                            return Err(CoreError::locked(format!("{id}: {path}")));
                        }
                    }
                    node.layout.w = *width;
                    node.layout.h = *height;
                    for path in ["/layout/w", "/layout/h"] {
                        let locked = node.property_meta.get(path).is_some_and(|m| m.agent_locked);
                        node.property_meta.insert(
                            path.into(),
                            PropertyMeta {
                                origin: transaction.origin,
                                last_revision: revision,
                                agent_locked: locked,
                            },
                        );
                    }
                }
            }
        }
        self.revision = revision;
        self.validate()
    }
}

fn number(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| CoreError::invalid("Expected a finite number"))
}
