use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path};

use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use super::domain::{
    ActionKind, Binding, Component as DocumentComponent, FeatureItem, FeatureMap, NodeStyle,
    Scalar, ValueKind, VectorCommand, VectorShape,
};
use super::{CoreError, Document, Node, NodeKind, Result};

pub(super) fn property_kind(node: &Node, path: &str) -> Option<ValueKind> {
    use ValueKind::{Number, String};
    match path {
        "/name"
        | "/style/fill"
        | "/style/color"
        | "/style/fontFamily"
        | "/style/fontFallback"
        | "/layout/flow/direction"
        | "/layout/flow/align"
        | "/style/textAlign"
        | "/style/overflow"
        | "/a11y/label"
        | "/a11y/alt" => Some(String),
        "/layout/x"
        | "/layout/y"
        | "/layout/w"
        | "/layout/h"
        | "/layout/minW"
        | "/layout/minH"
        | "/layout/maxW"
        | "/layout/maxH"
        | "/layout/flow/gap"
        | "/layout/flow/padding/top"
        | "/layout/flow/padding/right"
        | "/layout/flow/padding/bottom"
        | "/layout/flow/padding/left"
        | "/style/opacity"
        | "/style/fontSize"
        | "/style/fontWeight"
        | "/style/lineHeight"
        | "/style/letterSpacing"
        | "/style/radius"
        | "/style/rotate" => Some(Number),
        "/content/text" if matches!(node.kind, NodeKind::Text | NodeKind::Button) => Some(String),
        "/content/placeholder" if matches!(node.kind, NodeKind::Input | NodeKind::Textarea) => {
            Some(String)
        }
        _ => None,
    }
}

pub(super) fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn pointer(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let mut chars = path.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

pub(super) fn node_supported(node: &Node) -> bool {
    let (Some(_), Some(content), Some(_), Some(bindings), Some(events)) = (
        node.style.known(),
        node.content.known(),
        node.a11y.known(),
        node.bindings.known(),
        node.events.known(),
    ) else {
        return false;
    };
    (!matches!(node.kind, NodeKind::ComponentInstance) || content.component_id.is_some())
        && bindings
            .keys()
            .all(|path| property_kind(node, path).is_some())
        && events.keys().all(|key| identifier(key))
}

fn valid_color(value: &str) -> bool {
    let value = value.trim();
    let hex = value.strip_prefix('#').unwrap_or(value);
    (value.starts_with('#') && (hex.len() == 3 || hex.len() == 6))
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sorted_bounds(minimum: Option<f64>, maximum: Option<f64>) -> bool {
    minimum.zip(maximum).is_none_or(|(min, max)| min <= max)
}

fn validate_visual_style(node: &Node, style: &NodeStyle) -> Result<()> {
    let invalid = |reason: &str| Err(CoreError::invalid(format!("{reason} on node {}", node.id)));
    if style.radius.is_some_and(|n| !n.is_finite() || n < 0.0) {
        return invalid("Invalid corner radius");
    }
    if style
        .rotate
        .is_some_and(|n| !n.is_finite() || !(-3600.0..=3600.0).contains(&n))
    {
        return invalid("Invalid rotation");
    }
    if style.max_lines == Some(0) {
        return invalid("Invalid max lines");
    }
    if style.text_align.is_some() && !matches!(node.kind, NodeKind::Text | NodeKind::Button) {
        return invalid("Text alignment requires a text node");
    }
    if style.fit.is_some() && !matches!(node.kind, NodeKind::Image) {
        return invalid("Image fit requires an image node");
    }
    if let Some(stroke) = &style.stroke {
        if !valid_color(&stroke.color)
            || !stroke.width.is_finite()
            || !(0.0..=10_000.0).contains(&stroke.width)
        {
            return invalid("Invalid stroke");
        }
    }
    if style.shadows.len() > 8
        || style.shadows.iter().any(|shadow| {
            [shadow.x, shadow.y, shadow.blur, shadow.spread]
                .into_iter()
                .any(|n| !n.is_finite())
                || shadow.blur < 0.0
                || shadow.spread < 0.0
                || !valid_color(&shadow.color)
        })
    {
        return invalid("Invalid shadows");
    }
    let layout = &node.layout;
    for (min, max) in [(layout.min_w, layout.max_w), (layout.min_h, layout.max_h)] {
        if [min, max]
            .into_iter()
            .flatten()
            .any(|n| !n.is_finite() || n < 0.0)
        {
            return invalid("Invalid min or max size");
        }
        if !sorted_bounds(min, max) {
            return invalid("Minimum size exceeds maximum size");
        }
    }
    Ok(())
}

fn relative_path(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix)
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.len() > prefix.len()
}

fn records<'a, T: Clone + DeserializeOwned>(
    items: &'a [FeatureItem<T>],
    fields: &[&str],
    required: &[&str],
) -> Result<(Vec<Cow<'a, T>>, bool)> {
    let mut result = Vec::with_capacity(items.len());
    let mut unresolved = false;
    for item in items {
        match item {
            FeatureItem::Known(value) => result.push(Cow::Borrowed(value)),
            FeatureItem::Unsupported(Value::Object(object))
                if required.iter().skip(1).any(|key| object.contains_key(*key)) =>
            {
                let mut known = object.clone();
                known.retain(|key, _| fields.contains(&key.as_str()));
                result.push(Cow::Owned(
                    serde_json::from_value(Value::Object(known)).map_err(|error| {
                        CoreError::invalid(format!("Invalid document record: {error}"))
                    })?,
                ));
            }
            FeatureItem::Unsupported(_) => unresolved = true,
        }
    }
    Ok((result, unresolved))
}

fn validate_component_cycles(
    components: &[Cow<'_, DocumentComponent>],
    nodes: &HashMap<Uuid, &Node>,
) -> Result<()> {
    let ids: HashSet<_> = components.iter().map(|component| component.id).collect();
    let mut edges = HashMap::new();
    for component in components {
        let mut dependencies = HashSet::new();
        let mut pending = vec![component.root_id];
        while let Some(id) = pending.pop() {
            let node = nodes[&id];
            if let Some(id) = node
                .content
                .known()
                .and_then(|content| content.component_id)
            {
                if ids.contains(&id) {
                    dependencies.insert(id);
                }
            }
            pending.extend(node.children.iter().copied());
        }
        edges.insert(component.id, dependencies);
    }
    let mut visited = HashSet::new();
    let mut active = HashSet::new();
    for component in components {
        let mut pending = vec![(component.id, false)];
        while let Some((id, leave)) = pending.pop() {
            if leave {
                active.remove(&id);
                visited.insert(id);
            } else if active.contains(&id) {
                return Err(CoreError::invalid(format!("Component cycle at {id}")));
            } else if !visited.contains(&id) {
                active.insert(id);
                pending.push((id, true));
                pending.extend(edges[&id].iter().map(|dependency| (*dependency, false)));
            }
        }
    }
    Ok(())
}

fn validate_known_vector_fields(node: &Node) -> Result<()> {
    let FeatureMap::Unsupported(content) = &node.content else {
        return Ok(());
    };
    let Some(vector) = content.get("vector").and_then(Value::as_object) else {
        return Ok(());
    };
    let Some(paths) = vector.get("paths") else {
        return Ok(());
    };
    if !matches!(node.kind, NodeKind::Vector) {
        return Err(CoreError::invalid(format!(
            "Content does not match node {}",
            node.id
        )));
    }
    let paths = paths
        .as_array()
        .ok_or_else(|| CoreError::invalid(format!("Invalid vector paths on node {}", node.id)))?;
    for path in paths {
        let commands = path.as_array().ok_or_else(|| {
            CoreError::invalid(format!("Invalid vector path on node {}", node.id))
        })?;
        for command in commands {
            let Some(object) = command.as_object() else {
                return Err(CoreError::invalid(format!(
                    "Invalid vector command on node {}",
                    node.id
                )));
            };
            let fields: &[&str] = match object.get("type").and_then(Value::as_str) {
                Some("moveTo" | "lineTo") => &["type", "x", "y"],
                Some("curveTo") => &["type", "x1", "y1", "x2", "y2", "x", "y"],
                Some("close") => &["type"],
                _ => continue,
            };
            let mut known = object.clone();
            known.retain(|key, _| fields.contains(&key.as_str()));
            let command: VectorCommand =
                serde_json::from_value(Value::Object(known)).map_err(|error| {
                    CoreError::invalid(format!("Invalid vector on node {}: {error}", node.id))
                })?;
            if !command.finite() {
                return Err(CoreError::invalid(format!(
                    "Invalid vector on node {}",
                    node.id
                )));
            }
        }
    }
    if let Some(shapes) = vector.get("shapes") {
        let shapes = shapes.as_array().ok_or_else(|| {
            CoreError::invalid(format!("Invalid vector shapes on node {}", node.id))
        })?;
        for shape in shapes {
            let object = shape.as_object().ok_or_else(|| {
                CoreError::invalid(format!("Invalid vector shape on node {}", node.id))
            })?;
            let fields: &[&str] = match object.get("type").and_then(Value::as_str) {
                Some("rect") => &["type", "x", "y", "w", "h", "rx", "ry"],
                Some("circle") => &["type", "cx", "cy", "r"],
                Some("ellipse") => &["type", "cx", "cy", "rx", "ry"],
                Some("line") => &["type", "x1", "y1", "x2", "y2"],
                Some("polyline" | "polygon") => &["type", "points"],
                _ => continue,
            };
            let mut known = object.clone();
            known.retain(|key, _| fields.contains(&key.as_str()));
            if let Some(Value::Array(points)) = known.get_mut("points") {
                for point in points {
                    if let Some(fields) = point.as_object_mut() {
                        fields.retain(|key, _| matches!(key.as_str(), "x" | "y"));
                    }
                }
            }
            let shape: VectorShape =
                serde_json::from_value(Value::Object(known)).map_err(|error| {
                    CoreError::invalid(format!("Invalid vector on node {}: {error}", node.id))
                })?;
            if !shape.valid() {
                return Err(CoreError::invalid(format!(
                    "Invalid vector on node {}",
                    node.id
                )));
            }
        }
    }
    Ok(())
}

impl Document {
    // Unknown current-schema content remains readable but cannot be rewritten.
    pub fn editable(&self) -> bool {
        self.file_history_version.is_none_or(|version| version == 1)
            && self
                .components
                .iter()
                .all(|v| matches!(v, FeatureItem::Known(_)))
            && self
                .targets
                .iter()
                .all(|v| matches!(v, FeatureItem::Known(_)))
            && self
                .reference_baselines
                .iter()
                .all(|v| matches!(v, FeatureItem::Known(_)))
            && self.nodes.iter().all(node_supported)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_with(false)
    }

    pub(super) fn validate_transaction(&self) -> Result<()> {
        self.validate_with(true)
    }

    fn validate_with(&self, strict_bound_sizes: bool) -> Result<()> {
        if self.schema_version != 2 || self.pages.is_empty() || self.file_history_version == Some(0)
        {
            return Err(CoreError::invalid("Unsupported or empty document"));
        }
        let (components, unknown_components) = records(
            &self.components,
            &["id", "rootId", "name"],
            &["id", "rootId"],
        )?;
        let (targets, _) = records(&self.targets, &["id", "adapter"], &["id", "adapter"])?;
        let (baselines, _) = records(
            &self.reference_baselines,
            &["id", "pageId", "assetId"],
            &["id", "pageId", "assetId"],
        )?;
        let mut all_ids = HashSet::new();
        let mut insert_id = |id| {
            if all_ids.insert(id) {
                Ok(())
            } else {
                Err(CoreError::invalid(format!("Duplicate ID {id}")))
            }
        };
        insert_id(self.project_id)?;
        for id in self
            .pages
            .iter()
            .map(|v| v.id)
            .chain(self.nodes.iter().map(|v| v.id))
            .chain(self.tokens.iter().map(|v| v.id))
            .chain(self.variables.iter().map(|v| v.id))
            .chain(self.actions.iter().map(|v| v.id))
            .chain(self.animations.iter().map(|v| v.id))
            .chain(self.assets.iter().map(|v| v.id))
            .chain(self.code_modules.iter().map(|v| v.id))
            .chain(components.iter().map(|v| v.id))
            .chain(targets.iter().map(|v| v.id))
            .chain(baselines.iter().map(|v| v.id))
        {
            insert_id(id)?;
        }
        let nodes: HashMap<_, _> = self.nodes.iter().map(|n| (n.id, n)).collect();
        let pages: HashSet<_> = self.pages.iter().map(|p| p.id).collect();
        let tokens: HashMap<_, _> = self.tokens.iter().map(|v| (v.id, v)).collect();
        let variables: HashMap<_, _> = self.variables.iter().map(|v| (v.id, v)).collect();
        let actions: HashMap<_, _> = self.actions.iter().map(|v| (v.id, v)).collect();
        let animations: HashMap<_, _> = self.animations.iter().map(|v| (v.id, v)).collect();
        let assets: HashMap<_, _> = self.assets.iter().map(|v| (v.id, v)).collect();
        let modules: HashMap<_, _> = self.code_modules.iter().map(|v| (v.id, v)).collect();
        let component_ids: HashSet<_> = components.iter().map(|v| v.id).collect();
        for component in &components {
            if !nodes.contains_key(&component.root_id)
                || component
                    .name
                    .as_ref()
                    .is_some_and(|name| name.trim().is_empty())
            {
                return Err(CoreError::invalid(format!(
                    "Invalid component {}",
                    component.id
                )));
            }
        }
        for target in &targets {
            if target.adapter.trim().is_empty() {
                return Err(CoreError::invalid(format!("Invalid target {}", target.id)));
            }
        }
        for baseline in &baselines {
            if !pages.contains(&baseline.page_id) {
                return Err(CoreError::invalid(format!(
                    "Missing page for baseline {}",
                    baseline.id
                )));
            }
            if !assets.contains_key(&baseline.asset_id) {
                return Err(CoreError::new(
                    "MISSING_ASSET",
                    format!(
                        "Missing asset {} for baseline {}",
                        baseline.asset_id, baseline.id
                    ),
                ));
            }
        }
        for token in &self.tokens {
            if token.name.trim().is_empty() || !token.value.finite() {
                return Err(CoreError::invalid(format!("Invalid token {}", token.id)));
            }
        }
        for variable in &self.variables {
            if variable.name.trim().is_empty() || !variable.value.finite() {
                return Err(CoreError::invalid(format!(
                    "Invalid variable {}",
                    variable.id
                )));
            }
        }
        for asset in &self.assets {
            let file = asset.path.strip_prefix("assets/").unwrap_or("");
            let Some((digest, extension)) = file.rsplit_once('.') else {
                return Err(CoreError::invalid(format!(
                    "Invalid asset path for {}",
                    asset.id
                )));
            };
            if !relative_path(&asset.path, "assets/")
                || file.contains('/')
                || digest.len() != 64
                || !digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || extension.is_empty()
                || !extension.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(CoreError::invalid(format!(
                    "Invalid asset path for {}",
                    asset.id
                )));
            }
        }
        for module in &self.code_modules {
            if !relative_path(&module.path, "src/user/")
                || !(module.path.ends_with(".tsx") || module.path.ends_with(".ts"))
                || module.props.keys().any(|k| !identifier(k))
                || module.events.iter().any(|k| !identifier(k))
                || module.events.iter().collect::<HashSet<_>>().len() != module.events.len()
            {
                return Err(CoreError::invalid(format!(
                    "Invalid code module {}",
                    module.id
                )));
            }
        }
        for animation in &self.animations {
            let node = nodes.get(&animation.node_id).ok_or_else(|| {
                CoreError::invalid(format!("Animation {} has a missing node", animation.id))
            })?;
            let kind = property_kind(node, &animation.property).ok_or_else(|| {
                CoreError::invalid(format!("Invalid animation property on node {}", node.id))
            })?;
            if animation.duration_ms == 0
                || animation.keyframes.len() < 2
                || animation.keyframes.iter().any(|k| {
                    !k.offset.is_finite()
                        || !(0.0..=1.0).contains(&k.offset)
                        || !k.value.finite()
                        || k.value.kind() != kind
                })
                || animation
                    .keyframes
                    .windows(2)
                    .any(|pair| pair[0].offset >= pair[1].offset)
            {
                return Err(CoreError::invalid(format!(
                    "Invalid animation {}",
                    animation.id
                )));
            }
        }
        for action in &self.actions {
            let valid = match &action.kind {
                ActionKind::Navigate { page_id } => pages.contains(page_id),
                ActionKind::OpenUrl { url } => {
                    let rest = url
                        .strip_prefix("https://")
                        .or_else(|| url.strip_prefix("http://"));
                    rest.is_some_and(|s| {
                        s.split(['/', '?', '#'])
                            .next()
                            .is_some_and(|host| !host.is_empty())
                    }) && !url.chars().any(|ch| ch.is_whitespace() || ch.is_control())
                }
                ActionKind::ToggleVisibility { node_id } => nodes.contains_key(node_id),
                ActionKind::PlayAnimation { animation_id } => animations.contains_key(animation_id),
                ActionKind::SetVariable { variable_id, value } => {
                    value.finite()
                        && variables
                            .get(variable_id)
                            .is_some_and(|v| v.value.kind() == value.kind())
                }
                ActionKind::ToggleVariable { variable_id } => variables
                    .get(variable_id)
                    .is_some_and(|v| v.value.kind() == ValueKind::Boolean),
            };
            if !valid {
                return Err(CoreError::invalid(format!(
                    "Invalid target for action {}",
                    action.id
                )));
            }
        }
        let mut roots = HashSet::new();
        let mut stack = Vec::new();
        for page in &self.pages {
            let root = nodes
                .get(&page.root_id)
                .ok_or_else(|| CoreError::invalid(format!("Missing root for page {}", page.id)))?;
            if page.name.trim().is_empty()
                || root.parent_id.is_some()
                || !matches!(root.kind, NodeKind::Frame)
                || !roots.insert(root.id)
            {
                return Err(CoreError::invalid(format!(
                    "Invalid root for page {}",
                    page.id
                )));
            }
            stack.push(root.id);
        }
        for node in &self.nodes {
            let l = &node.layout;
            if node.name.trim().is_empty()
                || ![l.x, l.y, l.w, l.h].into_iter().all(f64::is_finite)
                || l.w <= 0.0
                || l.h <= 0.0
            {
                return Err(CoreError::invalid(format!(
                    "Invalid name or dimensions for node {}",
                    node.id
                )));
            }
            if let Some(bounds) = &node.source_bounds {
                if ![bounds.x, bounds.y, bounds.w, bounds.h]
                    .into_iter()
                    .all(f64::is_finite)
                    || bounds.w <= 0.0
                    || bounds.h <= 0.0
                {
                    return Err(CoreError::invalid(format!(
                        "Invalid source bounds on node {}",
                        node.id
                    )));
                }
            }
            if node
                .source_density
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
            {
                return Err(CoreError::invalid(format!(
                    "Invalid source density on node {}",
                    node.id
                )));
            }
            let mut breakpoints = HashSet::new();
            for override_ in &node.responsive {
                if !override_.breakpoint.is_finite()
                    || override_.breakpoint <= 0.0
                    || !breakpoints.insert(override_.breakpoint.to_bits())
                    || [override_.x, override_.y]
                        .into_iter()
                        .flatten()
                        .any(|n| !n.is_finite())
                    || [override_.w, override_.h]
                        .into_iter()
                        .flatten()
                        .any(|n| !n.is_finite() || n <= 0.0)
                {
                    return Err(CoreError::invalid(format!(
                        "Invalid responsive override on node {}",
                        node.id
                    )));
                }
            }
            if let Some(flow) = &l.flow {
                let p = &flow.padding;
                if !matches!(
                    node.kind,
                    NodeKind::Frame | NodeKind::Group | NodeKind::Button
                ) || [flow.gap, p.top, p.right, p.bottom, p.left]
                    .into_iter()
                    .any(|n| !n.is_finite() || n < 0.0)
                {
                    return Err(CoreError::invalid(format!(
                        "Invalid flow for node {}",
                        node.id
                    )));
                }
            }
            if !node.children.is_empty()
                && !matches!(
                    node.kind,
                    NodeKind::Frame
                        | NodeKind::Group
                        | NodeKind::Button
                        | NodeKind::ComponentInstance
                )
            {
                return Err(CoreError::invalid(format!(
                    "Node {} cannot contain children",
                    node.id
                )));
            }
            let mut children = HashSet::new();
            for child in &node.children {
                if !children.insert(child)
                    || nodes
                        .get(child)
                        .is_none_or(|n| n.parent_id != Some(node.id))
                {
                    return Err(CoreError::invalid(format!(
                        "Invalid child link on node {}",
                        node.id
                    )));
                }
            }
            for (path, meta) in &node.property_meta {
                if !pointer(path) || meta.last_revision > self.revision {
                    return Err(CoreError::invalid(format!(
                        "Invalid property metadata on node {}",
                        node.id
                    )));
                }
            }
            validate_known_vector_fields(node)?;
            let (Some(style), Some(content), Some(bindings), Some(events)) = (
                node.style.recognized(),
                node.content.recognized(),
                node.bindings.recognized(),
                node.events.recognized(),
            ) else {
                return Err(CoreError::invalid(format!("Unsupported node {}", node.id)));
            };
            if style
                .opacity
                .is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
            {
                return Err(CoreError::invalid(format!(
                    "Invalid opacity on node {}",
                    node.id
                )));
            }
            if style
                .font_family
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
                || style
                    .font_fallback
                    .as_ref()
                    .is_some_and(|s| s.trim().is_empty())
                || [style.font_size, style.line_height]
                    .into_iter()
                    .flatten()
                    .any(|n| {
                        !n.is_finite() || n <= 0.0 || !(n as f32).is_finite() || n as f32 == 0.0
                    })
                || style
                    .letter_spacing
                    .is_some_and(|n| !n.is_finite() || !(n as f32).is_finite())
                || style.font_weight.is_some_and(|n| !(1..=1000).contains(&n))
            {
                return Err(CoreError::invalid(format!(
                    "Invalid typography on node {}",
                    node.id
                )));
            }
            validate_visual_style(node, &style)?;
            if let Some(id) = style.font_asset_id {
                let asset = assets.get(&id).ok_or_else(|| {
                    CoreError::new(
                        "MISSING_ASSET",
                        format!("Missing font asset {id} on node {}", node.id),
                    )
                })?;
                if !matches!(
                    asset.path.rsplit_once('.').map(|(_, ext)| ext),
                    Some("ttf" | "otf" | "ttc")
                ) {
                    return Err(CoreError::invalid(format!("Asset {id} is not a font")));
                }
            }
            if (content.text.is_some() && !matches!(node.kind, NodeKind::Text | NodeKind::Button))
                || (content.placeholder.is_some()
                    && !matches!(node.kind, NodeKind::Input | NodeKind::Textarea))
                || (content.asset_id.is_some() && !matches!(node.kind, NodeKind::Image))
                || (content.vector.is_some() && !matches!(node.kind, NodeKind::Vector))
                || ((content.module_id.is_some() || content.props.is_some())
                    && !matches!(node.kind, NodeKind::CodeComponent))
                || (content.component_id.is_some()
                    && !matches!(node.kind, NodeKind::ComponentInstance))
            {
                return Err(CoreError::invalid(format!(
                    "Content does not match node {}",
                    node.id
                )));
            }
            if let Some(id) = content.component_id {
                if !component_ids.contains(&id) && !unknown_components {
                    return Err(CoreError::invalid(format!(
                        "Missing component {id} on node {}",
                        node.id
                    )));
                }
            }
            if let Some(id) = content.asset_id {
                let asset = assets.get(&id).ok_or_else(|| {
                    CoreError::new(
                        "MISSING_ASSET",
                        format!("Missing asset {id} on node {}", node.id),
                    )
                })?;
                if matches!(
                    asset.path.rsplit_once('.').map(|(_, ext)| ext),
                    Some("ttf" | "otf" | "ttc")
                ) {
                    return Err(CoreError::invalid(format!(
                        "Font asset {id} cannot be used as an image"
                    )));
                }
            }
            if content.vector.as_ref().is_some_and(|vector| {
                vector
                    .paths
                    .iter()
                    .flatten()
                    .any(|command| !command.finite())
                    || vector.shapes.iter().any(|shape| !shape.valid())
            }) {
                return Err(CoreError::invalid(format!(
                    "Invalid vector on node {}",
                    node.id
                )));
            }
            if matches!(node.kind, NodeKind::CodeComponent)
                && (content.module_id.is_some() || node.content.known().is_some())
            {
                let id = content.module_id.ok_or_else(|| {
                    CoreError::invalid(format!("Missing module on node {}", node.id))
                })?;
                let module = modules.get(&id).ok_or_else(|| {
                    CoreError::invalid(format!("Missing module for node {}", node.id))
                })?;
                if let Some(props) = &content.props {
                    for (name, value) in props {
                        if !value.finite() || module.props.get(name) != Some(&value.kind()) {
                            return Err(CoreError::invalid(format!(
                                "Invalid prop {name} on node {}",
                                node.id
                            )));
                        }
                    }
                }
                if events.keys().any(|name| !module.events.contains(name)) {
                    return Err(CoreError::invalid(format!(
                        "Undeclared event on node {}",
                        node.id
                    )));
                }
            }
            for (path, binding) in bindings.iter() {
                if property_kind(node, path).is_none() {
                    continue;
                }
                let value = match binding {
                    Binding::Token { token_id } => tokens.get(token_id).map(|v| &v.value),
                    Binding::Variable { variable_id } => {
                        variables.get(variable_id).map(|v| &v.value)
                    }
                };
                let kind = value.map(Scalar::kind);
                if kind.is_none() || kind != property_kind(node, path) {
                    return Err(CoreError::invalid(format!(
                        "Missing or incompatible binding on node {}",
                        node.id
                    )));
                }
                if strict_bound_sizes
                    && matches!(path.as_str(), "/layout/w" | "/layout/h")
                    && matches!(value, Some(Scalar::Number(number)) if *number <= 0.0)
                {
                    return Err(CoreError::invalid(format!(
                        "Invalid bound {path} on node {}",
                        node.id
                    )));
                }
            }
            for id in events.values() {
                if !actions.contains_key(id) {
                    return Err(CoreError::invalid(format!(
                        "Missing action {id} on node {}",
                        node.id
                    )));
                }
            }
        }
        let mut visited = HashSet::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                return Err(CoreError::invalid(format!(
                    "Node cycle or duplicate root at {id}"
                )));
            }
            let node = nodes[&id];
            stack.extend(node.children.iter().copied());
        }
        if visited.len() != nodes.len() {
            return Err(CoreError::invalid(
                "Document contains orphaned or cyclic nodes",
            ));
        }
        validate_component_cycles(&components, &nodes)?;
        Ok(())
    }

    pub fn validate_files(&self, files: &BTreeMap<String, String>) -> Result<()> {
        for asset in &self.assets {
            let Some(hash) = files.get(&asset.path) else {
                return Err(CoreError::new(
                    "MISSING_ASSET",
                    format!("Missing asset {} ({})", asset.id, asset.path),
                ));
            };
            let expected = asset
                .path
                .trim_start_matches("assets/")
                .split('.')
                .next()
                .unwrap_or("");
            if hash != expected {
                return Err(CoreError::invalid(format!(
                    "Asset hash does not match {}",
                    asset.path
                )));
            }
        }
        for module in &self.code_modules {
            if !files.contains_key(&module.path) {
                return Err(CoreError::invalid(format!(
                    "Missing source for code module {} ({})",
                    module.id, module.path
                )));
            }
        }
        Ok(())
    }
}
