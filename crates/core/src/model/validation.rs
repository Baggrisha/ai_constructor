use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path};

use super::domain::{ActionKind, Binding, ValueKind};
use super::{CoreError, Document, Node, NodeKind, Result};

pub(super) fn property_kind(node: &Node, path: &str) -> Option<ValueKind> {
    use ValueKind::{Number, String};
    match path {
        "/name" | "/style/fill" | "/style/color" | "/a11y/label" | "/a11y/alt" => Some(String),
        "/layout/x" | "/layout/y" | "/layout/w" | "/layout/h" | "/style/opacity" => Some(Number),
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
    let (Some(_), Some(_), Some(_), Some(bindings), Some(events)) = (
        node.style.known(),
        node.content.known(),
        node.a11y.known(),
        node.bindings.known(),
        node.events.known(),
    ) else {
        return false;
    };
    !matches!(node.kind, NodeKind::ComponentInstance)
        && bindings
            .keys()
            .all(|path| property_kind(node, path).is_some())
        && events.keys().all(|key| identifier(key))
}

fn relative_path(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix)
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.len() > prefix.len()
}

impl Document {
    // Unknown current-schema content remains readable but cannot be rewritten.
    pub fn editable(&self) -> bool {
        self.components.is_empty()
            && self.targets.is_empty()
            && self.reference_baselines.is_empty()
            && self.nodes.iter().all(node_supported)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.pages.is_empty() {
            return Err(CoreError::invalid("Unsupported or empty document"));
        }
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
        {
            insert_id(id)?;
        }
        let nodes: HashMap<_, _> = self.nodes.iter().map(|n| (n.id, n)).collect();
        let pages: HashSet<_> = self.pages.iter().map(|p| p.id).collect();
        let tokens: HashMap<_, _> = self.tokens.iter().map(|v| (v.id, v)).collect();
        let variables: HashMap<_, _> = self.variables.iter().map(|v| (v.id, v)).collect();
        let actions: HashMap<_, _> = self.actions.iter().map(|v| (v.id, v)).collect();
        let animations: HashMap<_, _> = self.animations.iter().map(|v| (v.id, v)).collect();
        let assets: HashSet<_> = self.assets.iter().map(|v| v.id).collect();
        let modules: HashMap<_, _> = self.code_modules.iter().map(|v| (v.id, v)).collect();
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
            if !node_supported(node) {
                continue;
            }
            let (Some(style), Some(content), Some(bindings), Some(events)) = (
                node.style.known(),
                node.content.known(),
                node.bindings.known(),
                node.events.known(),
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
            if (content.text.is_some() && !matches!(node.kind, NodeKind::Text | NodeKind::Button))
                || (content.placeholder.is_some()
                    && !matches!(node.kind, NodeKind::Input | NodeKind::Textarea))
                || (content.asset_id.is_some() && !matches!(node.kind, NodeKind::Image))
                || (content.vector.is_some() && !matches!(node.kind, NodeKind::Vector))
                || ((content.module_id.is_some() || content.props.is_some())
                    && !matches!(node.kind, NodeKind::CodeComponent))
            {
                return Err(CoreError::invalid(format!(
                    "Content does not match node {}",
                    node.id
                )));
            }
            if let Some(id) = content.asset_id {
                if !assets.contains(&id) {
                    return Err(CoreError::new(
                        "MISSING_ASSET",
                        format!("Missing asset {id} on node {}", node.id),
                    ));
                }
            }
            if content.vector.as_ref().is_some_and(|vector| {
                vector
                    .paths
                    .iter()
                    .flatten()
                    .any(|command| !command.finite())
            }) {
                return Err(CoreError::invalid(format!(
                    "Invalid vector on node {}",
                    node.id
                )));
            }
            if matches!(node.kind, NodeKind::CodeComponent) {
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
            for (path, binding) in bindings {
                let kind = match binding {
                    Binding::Token { token_id } => tokens.get(token_id).map(|v| v.value.kind()),
                    Binding::Variable { variable_id } => {
                        variables.get(variable_id).map(|v| v.value.kind())
                    }
                };
                if kind.is_none() || kind != property_kind(node, path) {
                    return Err(CoreError::invalid(format!(
                        "Missing or incompatible binding on node {}",
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
