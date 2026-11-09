use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use uuid::Uuid;

use super::domain::{Alignment, Binding, FeatureItem, FlowDirection, Scalar, SizeMode};
use super::{text::TextEngine, CoreError, Document, Node, NodeKind, Result};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Axis {
    Horizontal,
    Vertical,
}

struct LayoutContext<'a> {
    text: TextEngine<'a>,
    document: &'a Document,
    project_path: &'a Path,
    images: HashMap<Uuid, (f64, f64)>,
    vectors: HashMap<Uuid, (f64, f64)>,
    active: HashSet<(Uuid, Axis)>,
    active_stack: Vec<(Uuid, Axis)>,
}

impl<'a> LayoutContext<'a> {
    fn new(document: &'a Document, project_path: &'a Path) -> Self {
        Self {
            text: TextEngine::new(document, project_path),
            document,
            project_path,
            images: HashMap::new(),
            vectors: HashMap::new(),
            active: HashSet::new(),
            active_stack: Vec::new(),
        }
    }

    fn number(&self, node: &Node, path: &str, fallback: f64) -> Result<(f64, bool)> {
        let bindings = node
            .bindings
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown layout bindings"))?;
        let Some(binding) = bindings.get(path) else {
            return Ok((fallback, false));
        };
        let value = match binding {
            Binding::Token { token_id } => self
                .document
                .tokens
                .iter()
                .find(|token| token.id == *token_id)
                .map(|token| &token.value),
            Binding::Variable { variable_id } => self
                .document
                .variables
                .iter()
                .find(|variable| variable.id == *variable_id)
                .map(|variable| &variable.value),
        };
        match value {
            Some(Scalar::Number(value)) if value.is_finite() => Ok((*value, true)),
            _ => Err(CoreError::invalid(format!(
                "Invalid layout binding {path} on node {}",
                node.id
            ))),
        }
    }

    fn image_size(&mut self, node: &Node) -> Result<(f64, f64)> {
        let id = node
            .content
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown image content"))?
            .asset_id
            .ok_or_else(|| {
                CoreError::new(
                    "MISSING_ASSET",
                    format!("Missing image on node {}", node.id),
                )
            })?;
        if let Some(size) = self.images.get(&id) {
            return Ok(*size);
        }
        let asset = self
            .document
            .assets
            .iter()
            .find(|asset| asset.id == id)
            .ok_or_else(|| {
                CoreError::new(
                    "MISSING_ASSET",
                    format!("Missing image {id} on node {}", node.id),
                )
            })?;
        let (width, height) = crate::storage::image_dimensions(self.project_path, &asset.path)?;
        let size = (f64::from(width), f64::from(height));
        self.images.insert(id, size);
        Ok(size)
    }

    fn vector_size(&mut self, node: &Node) -> Result<(f64, f64)> {
        if let Some(size) = self.vectors.get(&node.id) {
            return Ok(*size);
        }
        let vector = node
            .content
            .known()
            .and_then(|content| content.vector.as_ref())
            .ok_or_else(|| {
                CoreError::unsupported(format!("No supported vector geometry on node {}", node.id))
            })?;
        let size = super::vector::intrinsic_size(vector).map_err(|error| {
            CoreError::new(error.code, format!("{} on node {}", error.message, node.id))
        })?;
        self.vectors.insert(node.id, size);
        Ok(size)
    }
}

fn responsive(node: &Node, breakpoint: Option<f64>) -> Option<&super::ResponsiveOverride> {
    breakpoint.and_then(|width| node.responsive.iter().find(|item| item.breakpoint == width))
}

fn position(
    node: &Node,
    breakpoint: Option<f64>,
    context: &LayoutContext<'_>,
) -> Result<(f64, f64)> {
    let override_ = responsive(node, breakpoint);
    let x = match override_.and_then(|item| item.x) {
        Some(x) => x,
        None => context.number(node, "/layout/x", node.layout.x)?.0,
    };
    let y = match override_.and_then(|item| item.y) {
        Some(y) => y,
        None => context.number(node, "/layout/y", node.layout.y)?.0,
    };
    Ok((x, y))
}

fn dimension(
    node: &Node,
    axis: Axis,
    breakpoint: Option<f64>,
    context: &LayoutContext<'_>,
) -> Result<(f64, Option<SizeMode>)> {
    let override_ = responsive(node, breakpoint);
    let (value, mode, explicit, path) = match axis {
        Axis::Horizontal => (
            node.layout.w,
            node.layout.width_mode,
            override_.and_then(|item| item.w),
            "/layout/w",
        ),
        Axis::Vertical => (
            node.layout.h,
            node.layout.height_mode,
            override_.and_then(|item| item.h),
            "/layout/h",
        ),
    };
    if let Some(explicit) = explicit {
        return Ok((explicit, Some(SizeMode::Fixed)));
    }
    let (value, bound) = context.number(node, path, value)?;
    if bound && matches!(mode, Some(SizeMode::Hug | SizeMode::Fill)) {
        return Err(CoreError::unsupported(format!(
            "Bound {path} with hug or fill is ambiguous on node {}",
            node.id
        )));
    }
    if !value.is_finite() || value <= 0.0 {
        return Err(CoreError::invalid(format!(
            "Invalid {path} on node {}",
            node.id
        )));
    }
    Ok((value, mode))
}

// Intrinsic sizes come from text, raster images, vector geometry, flow children,
// or a component's template. Other leaves need renderer-defined metrics.
fn measured_size(
    id: Uuid,
    axis: Axis,
    nodes: &HashMap<Uuid, &Node>,
    breakpoint: Option<f64>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut LayoutContext<'_>,
) -> Result<f64> {
    let initial = text.active_stack.len();
    let result = measured_size_inner(id, axis, nodes, breakpoint, sizes, text);
    while text.active_stack.len() > initial {
        let key = text.active_stack.pop().expect("active length was checked");
        text.active.remove(&key);
    }
    result
}

fn measured_size_inner(
    id: Uuid,
    axis: Axis,
    nodes: &HashMap<Uuid, &Node>,
    breakpoint: Option<f64>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut LayoutContext<'_>,
) -> Result<f64> {
    let mut pending = vec![(id, false)];
    while let Some((id, finish)) = pending.pop() {
        if sizes.contains_key(&(id, axis)) {
            continue;
        }
        if !finish {
            if !text.active.insert((id, axis)) {
                return Err(CoreError::unsupported(format!(
                    "Cyclic layout size on node {id}"
                )));
            }
            text.active_stack.push((id, axis));
        }
        let node = nodes[&id];
        let (fixed, mode) = dimension(node, axis, breakpoint, text)?;
        match mode {
            None | Some(SizeMode::Fixed) => {
                sizes.insert((id, axis), fixed);
            }
            Some(SizeMode::Fill) => {
                return Err(CoreError::unsupported(format!(
                    "Fill size depends on the available parent space for node {id}"
                )));
            }
            Some(SizeMode::Hug) => {
                if matches!(node.kind, NodeKind::Text) {
                    let width = if axis == Axis::Vertical
                        && !matches!(
                            dimension(node, Axis::Horizontal, breakpoint, text)?.1,
                            Some(SizeMode::Hug)
                        ) {
                        Some(actual_axis_size(
                            id,
                            Axis::Horizontal,
                            nodes,
                            breakpoint,
                            sizes,
                            text,
                        )?)
                    } else {
                        None
                    };
                    let measured = text.text.measure(node, width)?;
                    sizes.insert(
                        (id, axis),
                        if axis == Axis::Horizontal {
                            measured.0
                        } else {
                            measured.1
                        },
                    );
                    continue;
                }
                if matches!(node.kind, NodeKind::Image) {
                    let (width, height) = text.image_size(node)?;
                    let (natural, other_natural, other_axis) = if axis == Axis::Horizontal {
                        (width, height, Axis::Vertical)
                    } else {
                        (height, width, Axis::Horizontal)
                    };
                    let size = if matches!(
                        dimension(node, other_axis, breakpoint, text)?.1,
                        Some(SizeMode::Hug)
                    ) {
                        natural
                    } else {
                        let other =
                            actual_axis_size(id, other_axis, nodes, breakpoint, sizes, text)?;
                        natural / other_natural * other
                    };
                    if !size.is_finite() {
                        return Err(CoreError::invalid(format!(
                            "Layout size overflow on node {id}"
                        )));
                    }
                    sizes.insert((id, axis), size);
                    continue;
                }
                if matches!(node.kind, NodeKind::Vector) {
                    let (width, height) = text.vector_size(node)?;
                    sizes.insert(
                        (id, axis),
                        if axis == Axis::Horizontal {
                            width
                        } else {
                            height
                        },
                    );
                    continue;
                }
                if matches!(node.kind, NodeKind::ComponentInstance) && node.children.is_empty() {
                    let component_id = node
                        .content
                        .known()
                        .and_then(|content| content.component_id)
                        .ok_or_else(|| {
                            CoreError::invalid(format!("Missing component on node {id}"))
                        })?;
                    let root = text
                        .document
                        .components
                        .iter()
                        .find_map(|component| match component {
                            FeatureItem::Known(component) if component.id == component_id => {
                                Some(component.root_id)
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            CoreError::invalid(format!("Missing component {component_id}"))
                        })?;
                    if !finish {
                        pending.push((id, true));
                        pending.push((root, false));
                    } else {
                        sizes.insert((id, axis), sizes[&(root, axis)]);
                    }
                    continue;
                }
                let flow = node.layout.flow.as_ref().ok_or_else(|| {
                    CoreError::unsupported(format!("Intrinsic metrics are required for node {id}"))
                })?;
                if !finish {
                    pending.push((id, true));
                    pending.extend(node.children.iter().map(|child| (*child, false)));
                    continue;
                }
                let main = matches!(
                    (flow.direction, axis),
                    (FlowDirection::Row, Axis::Horizontal)
                        | (FlowDirection::Column, Axis::Vertical)
                );
                let padding = match axis {
                    Axis::Horizontal => flow.padding.left + flow.padding.right,
                    Axis::Vertical => flow.padding.top + flow.padding.bottom,
                };
                let content = if main {
                    node.children
                        .iter()
                        .map(|child| sizes[&(*child, axis)])
                        .sum::<f64>()
                        + flow.gap * node.children.len().saturating_sub(1) as f64
                } else {
                    node.children
                        .iter()
                        .map(|child| sizes[&(*child, axis)])
                        .fold(0.0_f64, f64::max)
                };
                let size = padding + content;
                if !size.is_finite() {
                    return Err(CoreError::invalid(format!(
                        "Layout size overflow on node {id}"
                    )));
                }
                sizes.insert((id, axis), size);
            }
        }
    }
    Ok(sizes[&(id, axis)])
}

fn actual_size(
    id: Uuid,
    nodes: &HashMap<Uuid, &Node>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut LayoutContext<'_>,
) -> Result<(f64, f64)> {
    Ok((
        actual_axis_size(id, Axis::Horizontal, nodes, None, sizes, text)?,
        actual_axis_size(id, Axis::Vertical, nodes, None, sizes, text)?,
    ))
}

fn actual_axis_size(
    id: Uuid,
    axis: Axis,
    nodes: &HashMap<Uuid, &Node>,
    breakpoint: Option<f64>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut LayoutContext<'_>,
) -> Result<f64> {
    let mut current = nodes[&id];
    let mut pending = Vec::new();
    loop {
        let mode = dimension(current, axis, breakpoint, text)?.1;
        let parent = current
            .parent_id
            .and_then(|parent| nodes.get(&parent).copied());
        let flow = parent.and_then(|parent| parent.layout.flow.as_ref());
        let cross_axis = |direction| {
            matches!(
                (direction, axis),
                (FlowDirection::Column, Axis::Horizontal) | (FlowDirection::Row, Axis::Vertical)
            )
        };
        let stretch = mode.is_none()
            && flow.is_some_and(|flow| {
                cross_axis(flow.direction) && matches!(flow.align, Alignment::Stretch)
            });
        if !matches!(mode, Some(SizeMode::Fill)) && !stretch {
            break;
        }
        let parent = parent
            .filter(|parent| parent.layout.flow.is_some())
            .ok_or_else(|| {
                CoreError::unsupported(format!(
                    "Fill size requires a flow parent for node {}",
                    current.id
                ))
            })?;
        if pending.len() >= nodes.len() {
            return Err(CoreError::invalid("Cycle while resolving fill size"));
        }
        pending.push((parent, current));
        current = parent;
    }
    let mut size = measured_size(current.id, axis, nodes, breakpoint, sizes, text)?;
    while let Some((parent, child)) = pending.pop() {
        let flow = parent.layout.flow.as_ref().expect("flow parent");
        let padding = match axis {
            Axis::Horizontal => flow.padding.left + flow.padding.right,
            Axis::Vertical => flow.padding.top + flow.padding.bottom,
        };
        let inner = (size - padding).max(0.0);
        let cross = matches!(
            (flow.direction, axis),
            (FlowDirection::Column, Axis::Horizontal) | (FlowDirection::Row, Axis::Vertical)
        );
        size = if cross {
            inner
        } else {
            let mut occupied = flow.gap * parent.children.len().saturating_sub(1) as f64;
            let mut fills = 0usize;
            for sibling in &parent.children {
                if matches!(
                    dimension(nodes[sibling], axis, breakpoint, text)?.1,
                    Some(SizeMode::Fill)
                ) {
                    fills += 1;
                } else {
                    occupied += measured_size(*sibling, axis, nodes, breakpoint, sizes, text)?;
                }
            }
            if fills == 0 || !occupied.is_finite() {
                return Err(CoreError::invalid(format!(
                    "Invalid fill size on node {}",
                    child.id
                )));
            }
            (inner - occupied).max(0.0) / fills as f64
        };
    }
    Ok(size)
}

// CSS flex subset: fixed/hug items do not shrink; fill is flex: 1 1 0 with
// minimum size 0. Unspecified sizes are fixed except stretch on the cross axis.
fn flow_child(
    parent: &Node,
    parent_size: (f64, f64),
    child: &Node,
    nodes: &HashMap<Uuid, &Node>,
    breakpoint: Option<f64>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut LayoutContext<'_>,
) -> Result<((f64, f64), (f64, f64))> {
    let flow = parent.layout.flow.as_ref().expect("flow parent");
    let row = matches!(flow.direction, FlowDirection::Row);
    let (main_axis, cross_axis) = if row {
        (Axis::Horizontal, Axis::Vertical)
    } else {
        (Axis::Vertical, Axis::Horizontal)
    };
    let (main_size, cross_size, main_start, main_end, cross_start, cross_end) = if row {
        (
            parent_size.0,
            parent_size.1,
            flow.padding.left,
            flow.padding.right,
            flow.padding.top,
            flow.padding.bottom,
        )
    } else {
        (
            parent_size.1,
            parent_size.0,
            flow.padding.top,
            flow.padding.bottom,
            flow.padding.left,
            flow.padding.right,
        )
    };
    let main_padding = main_start + main_end;
    let cross_padding = cross_start + cross_end;
    if !main_padding.is_finite() || !cross_padding.is_finite() {
        return Err(CoreError::invalid(format!(
            "Layout overflow on node {}",
            parent.id
        )));
    }
    let inner_main = (main_size - main_padding).max(0.0);
    let inner_cross = (cross_size - cross_padding).max(0.0);
    let mut fixed = 0.0;
    let mut fills = 0usize;
    for id in &parent.children {
        if matches!(
            dimension(nodes[id], main_axis, breakpoint, text)?.1,
            Some(SizeMode::Fill)
        ) {
            fills += 1;
        } else {
            fixed += measured_size(*id, main_axis, nodes, breakpoint, sizes, text)?;
        }
    }
    let gaps = flow.gap * parent.children.len().saturating_sub(1) as f64;
    let used = fixed + gaps;
    if !used.is_finite() {
        return Err(CoreError::invalid(format!(
            "Layout overflow on node {}",
            parent.id
        )));
    }
    let fill = if fills == 0 {
        0.0
    } else {
        (inner_main - used).max(0.0) / fills as f64
    };
    let mut main_position = main_start;
    for id in &parent.children {
        if *id == child.id {
            break;
        }
        main_position += if matches!(
            dimension(nodes[id], main_axis, breakpoint, text)?.1,
            Some(SizeMode::Fill)
        ) {
            fill
        } else {
            measured_size(*id, main_axis, nodes, breakpoint, sizes, text)?
        } + flow.gap;
    }
    let child_main = if matches!(
        dimension(child, main_axis, breakpoint, text)?.1,
        Some(SizeMode::Fill)
    ) {
        fill
    } else {
        measured_size(child.id, main_axis, nodes, breakpoint, sizes, text)?
    };
    let cross_mode = dimension(child, cross_axis, breakpoint, text)?.1;
    let child_cross = if matches!(cross_mode, Some(SizeMode::Fill))
        || (cross_mode.is_none() && matches!(flow.align, Alignment::Stretch))
    {
        inner_cross
    } else {
        measured_size(child.id, cross_axis, nodes, breakpoint, sizes, text)?
    };
    let cross_position = cross_start
        + match flow.align {
            Alignment::Center => (inner_cross - child_cross) / 2.0,
            Alignment::End => inner_cross - child_cross,
            Alignment::Start | Alignment::Stretch => 0.0,
        };
    if row {
        Ok(((main_position, cross_position), (child_main, child_cross)))
    } else {
        Ok(((cross_position, main_position), (child_cross, child_main)))
    }
}

/// Absolute document-space box of one node, in logical CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeBounds {
    pub id: Uuid,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub in_flow: bool,
}

impl Document {
    /// Computed absolute bounds for every node of `page_id` in document
    /// coordinates. Flow children are placed by the same rules used for the
    /// canvas transform; fixed children use their own position.
    pub fn layout_bounds(&self, project_path: &Path, page_id: Uuid) -> Result<Vec<NodeBounds>> {
        let page = self
            .pages
            .iter()
            .find(|page| page.id == page_id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown page {page_id}")))?;
        let nodes: HashMap<_, _> = self.nodes.iter().map(|node| (node.id, node)).collect();
        let root = *nodes
            .get(&page.root_id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown root {}", page.root_id)))?;
        let mut sizes = HashMap::new();
        let mut text = LayoutContext::new(self, project_path);
        let mut result = Vec::with_capacity(self.nodes.len());
        let (x, y) = position(root, None, &text)?;
        let mut pending = vec![(root.id, (x, y))];
        while let Some((id, offset)) = pending.pop() {
            let node = nodes[&id];
            let (w, h) = actual_size(node.id, &nodes, &mut sizes, &mut text)?;
            result.push(NodeBounds {
                id,
                x: offset.0,
                y: offset.1,
                w,
                h,
                in_flow: node
                    .parent_id
                    .and_then(|parent| nodes.get(&parent))
                    .is_some_and(|parent| parent.layout.flow.is_some()),
            });
            for child in &node.children {
                let child = nodes[child];
                let position = if node.layout.flow.is_some() {
                    flow_child(node, (w, h), child, &nodes, None, &mut sizes, &mut text)?.0
                } else {
                    position(child, None, &text)?
                };
                pending.push((child.id, (offset.0 + position.0, offset.1 + position.1)));
            }
        }
        Ok(result)
    }

    /// Line-break offsets of a text node's `content.text` at `width`, in
    /// characters as measured by the shared text engine.
    pub fn line_breaks(
        &self,
        project_path: &Path,
        node_id: Uuid,
        width: f64,
    ) -> Result<Vec<usize>> {
        if !width.is_finite() || width <= 0.0 {
            return Err(CoreError::invalid("Invalid text width"));
        }
        let node = self
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {node_id}")))?;
        if !matches!(node.kind, NodeKind::Text | NodeKind::Button) {
            return Err(CoreError::unsupported(format!(
                "Node {node_id} has no editable text"
            )));
        }
        let mut text = LayoutContext::new(self, project_path);
        text.text.line_breaks(node, width)
    }

    /// Transform node-local CSS pixels (or source pixels relative to the node)
    /// to/from the viewport. `viewport` is (zoom, pan_x, pan_y). Source bounds
    /// are retained independently and never substituted for layout coordinates.
    pub fn transform_point(
        &self,
        project_path: &Path,
        node_id: Uuid,
        point: (f64, f64),
        viewport: (f64, f64, f64),
        inverse: bool,
        source_pixels: bool,
    ) -> Result<(f64, f64)> {
        self.transform_point_with_breakpoint(
            project_path,
            node_id,
            point,
            viewport,
            inverse,
            source_pixels,
            None,
        )
    }

    /// Apply only the override explicitly attached to `breakpoint`; the base
    /// document is not modified and no unrequested threshold rule is inferred.
    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the geometry API with its project font context"
    )]
    pub fn transform_point_at_breakpoint(
        &self,
        project_path: &Path,
        node_id: Uuid,
        point: (f64, f64),
        viewport: (f64, f64, f64),
        inverse: bool,
        source_pixels: bool,
        breakpoint: f64,
    ) -> Result<(f64, f64)> {
        if !breakpoint.is_finite() || breakpoint <= 0.0 {
            return Err(CoreError::invalid("Invalid breakpoint"));
        }
        self.transform_point_with_breakpoint(
            project_path,
            node_id,
            point,
            viewport,
            inverse,
            source_pixels,
            Some(breakpoint),
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Geometry transform and optional breakpoint"
    )]
    fn transform_point_with_breakpoint(
        &self,
        project_path: &Path,
        node_id: Uuid,
        point: (f64, f64),
        viewport: (f64, f64, f64),
        inverse: bool,
        source_pixels: bool,
        breakpoint: Option<f64>,
    ) -> Result<(f64, f64)> {
        let (zoom, pan_x, pan_y) = viewport;
        if ![point.0, point.1, zoom, pan_x, pan_y]
            .into_iter()
            .all(f64::is_finite)
            || zoom <= 0.0
        {
            return Err(CoreError::invalid("Invalid point or viewport transform"));
        }
        let nodes: HashMap<_, _> = self.nodes.iter().map(|node| (node.id, node)).collect();
        let node = *nodes
            .get(&node_id)
            .ok_or_else(|| CoreError::invalid(format!("Unknown node {node_id}")))?;
        let density = if source_pixels {
            node.source_density
                .filter(|density| density.is_finite() && *density > 0.0)
                .ok_or_else(|| {
                    CoreError::invalid(format!("Missing source density on node {node_id}"))
                })?
        } else {
            1.0
        };
        let mut chain = vec![node];
        let mut current = node;
        while let Some(parent_id) = current.parent_id {
            if chain.len() >= self.nodes.len() {
                return Err(CoreError::invalid(format!(
                    "Parent cycle on node {node_id}"
                )));
            }
            current = *nodes
                .get(&parent_id)
                .ok_or_else(|| CoreError::invalid(format!("Missing parent {parent_id}")))?;
            chain.push(current);
        }
        chain.reverse();
        let root = chain[0];
        let mut sizes = HashMap::new();
        let mut text = LayoutContext::new(self, project_path);
        let mut offset = position(root, breakpoint, &text)?;
        let mut size = if chain.len() > 1 && root.layout.flow.is_some() {
            (
                measured_size(
                    root.id,
                    Axis::Horizontal,
                    &nodes,
                    breakpoint,
                    &mut sizes,
                    &mut text,
                )?,
                measured_size(
                    root.id,
                    Axis::Vertical,
                    &nodes,
                    breakpoint,
                    &mut sizes,
                    &mut text,
                )?,
            )
        } else {
            (0.0, 0.0)
        };
        for (index, pair) in chain.windows(2).enumerate() {
            let parent = pair[0];
            let child = pair[1];
            let (position, child_size) = if parent.layout.flow.is_some() {
                flow_child(
                    parent, size, child, &nodes, breakpoint, &mut sizes, &mut text,
                )?
            } else {
                (
                    position(child, breakpoint, &text)?,
                    if index + 2 < chain.len() && child.layout.flow.is_some() {
                        (
                            measured_size(
                                child.id,
                                Axis::Horizontal,
                                &nodes,
                                breakpoint,
                                &mut sizes,
                                &mut text,
                            )?,
                            measured_size(
                                child.id,
                                Axis::Vertical,
                                &nodes,
                                breakpoint,
                                &mut sizes,
                                &mut text,
                            )?,
                        )
                    } else {
                        (0.0, 0.0)
                    },
                )
            };
            offset.0 += position.0;
            offset.1 += position.1;
            size = child_size;
        }
        let result = if inverse {
            (
                ((point.0 - pan_x) / zoom - offset.0) * density,
                ((point.1 - pan_y) / zoom - offset.1) * density,
            )
        } else {
            (
                (point.0 / density + offset.0) * zoom + pan_x,
                (point.1 / density + offset.1) * zoom + pan_y,
            )
        };
        if result.0.is_finite() && result.1.is_finite() {
            Ok(result)
        } else {
            Err(CoreError::invalid("Coordinate transform overflow"))
        }
    }
}
