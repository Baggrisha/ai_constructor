use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Unknown fields are retained only so older builds can open the document read-only.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum FeatureMap<T> {
    Known(T),
    Unsupported(BTreeMap<String, Value>),
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for FeatureMap<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let map = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("Expected an object"))?;
        match serde_json::from_value::<T>(value.clone()) {
            Ok(known) => Ok(Self::Known(known)),
            Err(error) => {
                let unknown = |error: &serde_json::Error| {
                    let message = error.to_string();
                    message.contains("unknown field") || message.contains("unknown variant")
                };
                if !unknown(&error) {
                    return Err(serde::de::Error::custom(error));
                }
                for (key, field) in map {
                    let single =
                        Value::Object(std::iter::once((key.clone(), field.clone())).collect());
                    if let Err(error) = serde_json::from_value::<T>(single) {
                        if !unknown(&error) {
                            return Err(serde::de::Error::custom(error));
                        }
                    }
                }
                Ok(Self::Unsupported(
                    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                ))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FeatureItem<T> {
    Known(T),
    Unsupported(Value),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Component {
    pub id: Uuid,
    pub root_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub id: Uuid,
    pub adapter: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReferenceBaseline {
    pub id: Uuid,
    pub page_id: Uuid,
    pub asset_id: Uuid,
}

impl<T> FeatureMap<T> {
    pub fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unsupported(_) => None,
        }
    }
    pub fn known_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unsupported(_) => None,
        }
    }
}

impl<T: Clone + serde::de::DeserializeOwned> FeatureMap<T> {
    pub fn recognized(&self) -> Option<Cow<'_, T>> {
        match self {
            Self::Known(value) => Some(Cow::Borrowed(value)),
            Self::Unsupported(fields) => {
                let mut known = serde_json::Map::new();
                for (key, value) in fields {
                    let single =
                        Value::Object(std::iter::once((key.clone(), value.clone())).collect());
                    if serde_json::from_value::<T>(single).is_ok() {
                        known.insert(key.clone(), value.clone());
                    }
                }
                serde_json::from_value(Value::Object(known))
                    .ok()
                    .map(Cow::Owned)
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_asset_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_style: Option<FontStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letter_spacing: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_wrap: Option<TextWrap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_align: Option<TextAlign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overflow: Option<Overflow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadows: Vec<Shadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<ImageFit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stroke {
    pub color: String,
    pub width: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Shadow {
    pub x: f64,
    pub y: f64,
    pub blur: f64,
    pub spread: f64,
    pub color: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextAlign {
    Start,
    Center,
    End,
    Justify,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFit {
    Fill,
    Contain,
    Cover,
    None,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextWrap {
    None,
    Word,
    WordOrGlyph,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<VectorData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<BTreeMap<String, Scalar>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeAccessibility {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Boolean(bool),
    Number(f64),
    String(String),
}

impl Scalar {
    pub fn finite(&self) -> bool {
        !matches!(self, Self::Number(n) if !n.is_finite())
    }
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Boolean(_) => ValueKind::Boolean,
            Self::Number(_) => ValueKind::Number,
            Self::String(_) => ValueKind::String,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueKind {
    Boolean,
    Number,
    String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Token {
    pub id: Uuid,
    pub name: String,
    pub value: Scalar,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Variable {
    pub id: Uuid,
    pub name: String,
    pub value: Scalar,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Binding {
    Token { token_id: Uuid },
    Variable { variable_id: Uuid },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Action {
    pub id: Uuid,
    pub kind: ActionKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ActionKind {
    Navigate { page_id: Uuid },
    OpenUrl { url: String },
    ToggleVisibility { node_id: Uuid },
    PlayAnimation { animation_id: Uuid },
    SetVariable { variable_id: Uuid, value: Scalar },
    ToggleVariable { variable_id: Uuid },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Animation {
    pub id: Uuid,
    pub node_id: Uuid,
    pub property: String,
    pub duration_ms: u32,
    pub delay_ms: u32,
    pub keyframes: Vec<Keyframe>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Keyframe {
    pub offset: f64,
    pub value: Scalar,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub id: Uuid,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeModule {
    pub id: Uuid,
    pub path: String,
    pub props: BTreeMap<String, ValueKind>,
    pub events: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Flow {
    pub direction: FlowDirection,
    pub gap: f64,
    pub padding: Padding,
    pub align: Alignment,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlowDirection {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Alignment {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SizeMode {
    Fixed,
    Hug,
    Fill,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Padding {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponsiveOverride {
    pub breakpoint: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorData {
    pub paths: Vec<Vec<VectorCommand>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shapes: Vec<VectorShape>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum VectorShape {
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rx: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ry: Option<f64>,
    },
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
    Polyline {
        points: Vec<VectorPoint>,
    },
    Polygon {
        points: Vec<VectorPoint>,
    },
}

impl VectorShape {
    pub fn valid(&self) -> bool {
        match self {
            Self::Rect { x, y, w, h, rx, ry } => {
                [*x, *y, *w, *h].into_iter().all(f64::is_finite)
                    && *w > 0.0
                    && *h > 0.0
                    && [*rx, *ry]
                        .into_iter()
                        .flatten()
                        .all(|r| r.is_finite() && r >= 0.0)
            }
            Self::Circle { cx, cy, r } => {
                [*cx, *cy, *r].into_iter().all(f64::is_finite) && *r > 0.0
            }
            Self::Ellipse { cx, cy, rx, ry } => {
                [*cx, *cy, *rx, *ry].into_iter().all(f64::is_finite) && *rx > 0.0 && *ry > 0.0
            }
            Self::Line { x1, y1, x2, y2 } => [*x1, *y1, *x2, *y2].into_iter().all(f64::is_finite),
            Self::Polyline { points } | Self::Polygon { points } => {
                let minimum = if matches!(self, Self::Polygon { .. }) {
                    3
                } else {
                    2
                };
                points.len() >= minimum
                    && points
                        .iter()
                        .all(|point| point.x.is_finite() && point.y.is_finite())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VectorCommand {
    MoveTo {
        x: f64,
        y: f64,
    },
    LineTo {
        x: f64,
        y: f64,
    },
    CurveTo {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        x: f64,
        y: f64,
    },
    Close {},
}

impl VectorCommand {
    pub fn finite(&self) -> bool {
        match self {
            Self::MoveTo { x, y } | Self::LineTo { x, y } => x.is_finite() && y.is_finite(),
            Self::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => [*x1, *y1, *x2, *y2, *x, *y].into_iter().all(f64::is_finite),
            Self::Close {} => true,
        }
    }
}
