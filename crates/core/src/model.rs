use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{CoreError, Result};
mod domain;
mod geometry;
mod operations;
mod render;
mod text;
mod validation;
mod vector;

use domain::{
    Action, Animation, Asset, Binding, Bounds, CodeModule, Component, FeatureItem, FeatureMap,
    Flow, NodeAccessibility, NodeContent, NodeStyle, ReferenceBaseline, ResponsiveOverride,
    SizeMode, Target, Token, Variable,
};

fn required_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub schema_version: u32,
    pub project_id: Uuid,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_history_version: Option<u32>,
    pub pages: Vec<Page>,
    pub nodes: Vec<Node>,
    pub components: Vec<FeatureItem<Component>>,
    pub tokens: Vec<Token>,
    pub variables: Vec<Variable>,
    pub actions: Vec<Action>,
    pub animations: Vec<Animation>,
    pub assets: Vec<Asset>,
    pub code_modules: Vec<CodeModule>,
    pub targets: Vec<FeatureItem<Target>>,
    pub reference_baselines: Vec<FeatureItem<ReferenceBaseline>>,
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
    #[serde(deserialize_with = "required_option")]
    pub parent_id: Option<Uuid>,
    pub kind: NodeKind,
    pub name: String,
    pub children: Vec<Uuid>,
    pub layout: Layout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bounds: Option<Bounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_density: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub responsive: Vec<ResponsiveOverride>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    pub style: FeatureMap<NodeStyle>,
    pub content: FeatureMap<NodeContent>,
    pub a11y: FeatureMap<NodeAccessibility>,
    pub bindings: FeatureMap<BTreeMap<String, Binding>>,
    pub events: FeatureMap<BTreeMap<String, Uuid>>,
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

impl NodeKind {
    pub fn token(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Group => "group",
            Self::Text => "text",
            Self::Image => "image",
            Self::Vector => "vector",
            Self::Button => "button",
            Self::Input => "input",
            Self::Textarea => "textarea",
            Self::Checkbox => "checkbox",
            Self::Select => "select",
            Self::ComponentInstance => "componentInstance",
            Self::CodeComponent => "codeComponent",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_mode: Option<SizeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_mode: Option<SizeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_h: Option<f64>,
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
    InsertNode {
        parent_id: Uuid,
        index: usize,
        node: Box<Node>,
    },
    RemoveNode {
        node_id: Uuid,
    },
    MoveNode {
        node_id: Uuid,
        parent_id: Uuid,
        index: usize,
    },
    SetProperty {
        node_id: Uuid,
        path: String,
        value: Value,
    },
    SetBinding {
        node_id: Uuid,
        path: String,
        #[serde(deserialize_with = "required_option")]
        binding: Option<domain::Binding>,
    },
    UpsertAction {
        action: Action,
    },
    UpsertAnimation {
        animation: Animation,
    },
    UpsertCodeModule {
        code_module: CodeModule,
    },
    SetToken {
        token: Token,
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
            schema_version: 2,
            project_id,
            revision: 0,
            file_history_version: None,
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
                    flow: None,
                    width_mode: None,
                    height_mode: None,
                    min_w: None,
                    min_h: None,
                    max_w: None,
                    max_h: None,
                    h: 900.0,
                },
                source_bounds: None,
                source_density: None,
                responsive: Vec::new(),
                hidden: false,
                locked: false,
                style: FeatureMap::Known(NodeStyle::default()),
                content: FeatureMap::Known(NodeContent::default()),
                a11y: FeatureMap::Known(NodeAccessibility::default()),
                bindings: FeatureMap::Known(BTreeMap::new()),
                events: FeatureMap::Known(BTreeMap::new()),
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
}
