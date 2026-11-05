use std::{collections::HashMap, path::Path};

use uuid::Uuid;

use super::domain::{Alignment, FlowDirection, SizeMode};
use super::{text::TextEngine, CoreError, Document, Node, NodeKind, Result};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Axis {
    Horizontal,
    Vertical,
}

fn responsive(node: &Node, breakpoint: Option<f64>) -> Option<&super::ResponsiveOverride> {
    breakpoint.and_then(|width| node.responsive.iter().find(|item| item.breakpoint == width))
}

fn position(node: &Node, breakpoint: Option<f64>) -> (f64, f64) {
    let override_ = responsive(node, breakpoint);
    (
        override_.and_then(|item| item.x).unwrap_or(node.layout.x),
        override_.and_then(|item| item.y).unwrap_or(node.layout.y),
    )
}

fn dimension(node: &Node, axis: Axis, breakpoint: Option<f64>) -> (f64, Option<SizeMode>) {
    let override_ = responsive(node, breakpoint);
    match axis {
        Axis::Horizontal => (
            override_.and_then(|item| item.w).unwrap_or(node.layout.w),
            node.layout.width_mode,
        ),
        Axis::Vertical => (
            override_.and_then(|item| item.h).unwrap_or(node.layout.h),
            node.layout.height_mode,
        ),
    }
}

// Intrinsic text uses shaped font metrics; other intrinsic leaf kinds remain unsupported.
fn measured_size(
    id: Uuid,
    axis: Axis,
    nodes: &HashMap<Uuid, &Node>,
    breakpoint: Option<f64>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut TextEngine<'_>,
) -> Result<f64> {
    let mut pending = vec![(id, false)];
    while let Some((id, finish)) = pending.pop() {
        if sizes.contains_key(&(id, axis)) {
            continue;
        }
        let node = nodes[&id];
        let (fixed, mode) = dimension(node, axis, breakpoint);
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
                        && !matches!(node.layout.width_mode, Some(SizeMode::Hug))
                    {
                        Some(actual_text_width(id, nodes, breakpoint, sizes, text)?)
                    } else {
                        None
                    };
                    let measured = text.measure(node, width)?;
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

fn actual_text_width(
    id: Uuid,
    nodes: &HashMap<Uuid, &Node>,
    breakpoint: Option<f64>,
    sizes: &mut HashMap<(Uuid, Axis), f64>,
    text: &mut TextEngine<'_>,
) -> Result<f64> {
    let mut current = nodes[&id];
    let mut pending = Vec::new();
    loop {
        let mode = dimension(current, Axis::Horizontal, breakpoint).1;
        let parent = current
            .parent_id
            .and_then(|parent| nodes.get(&parent).copied());
        let flow = parent.and_then(|parent| parent.layout.flow.as_ref());
        let stretch = mode.is_none()
            && flow.is_some_and(|flow| {
                matches!(flow.direction, FlowDirection::Column)
                    && matches!(flow.align, Alignment::Stretch)
            });
        if !matches!(mode, Some(SizeMode::Fill)) && !stretch {
            break;
        }
        let parent = parent
            .filter(|parent| parent.layout.flow.is_some())
            .ok_or_else(|| {
                CoreError::unsupported(format!(
                    "Fill width requires a flow parent for node {}",
                    current.id
                ))
            })?;
        if pending.len() >= nodes.len() {
            return Err(CoreError::invalid("Cycle while resolving text width"));
        }
        pending.push((parent, current));
        current = parent;
    }
    let mut width = measured_size(current.id, Axis::Horizontal, nodes, breakpoint, sizes, text)?;
    while let Some((parent, child)) = pending.pop() {
        let flow = parent.layout.flow.as_ref().expect("flow parent");
        let inner = (width - flow.padding.left - flow.padding.right).max(0.0);
        width = if matches!(flow.direction, FlowDirection::Column) {
            inner
        } else {
            let mut occupied = flow.gap * parent.children.len().saturating_sub(1) as f64;
            let mut fills = 0usize;
            for sibling in &parent.children {
                if matches!(
                    dimension(nodes[sibling], Axis::Horizontal, breakpoint).1,
                    Some(SizeMode::Fill)
                ) {
                    fills += 1;
                } else {
                    occupied +=
                        measured_size(*sibling, Axis::Horizontal, nodes, breakpoint, sizes, text)?;
                }
            }
            if fills == 0 || !occupied.is_finite() {
                return Err(CoreError::invalid(format!(
                    "Invalid fill width on node {}",
                    child.id
                )));
            }
            (inner - occupied).max(0.0) / fills as f64
        };
    }
    Ok(width)
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
    text: &mut TextEngine<'_>,
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
            dimension(nodes[id], main_axis, breakpoint).1,
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
            dimension(nodes[id], main_axis, breakpoint).1,
            Some(SizeMode::Fill)
        ) {
            fill
        } else {
            measured_size(*id, main_axis, nodes, breakpoint, sizes, text)?
        } + flow.gap;
    }
    let child_main = if matches!(
        dimension(child, main_axis, breakpoint).1,
        Some(SizeMode::Fill)
    ) {
        fill
    } else {
        measured_size(child.id, main_axis, nodes, breakpoint, sizes, text)?
    };
    let cross_mode = dimension(child, cross_axis, breakpoint).1;
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

impl Document {
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
        let mut offset = position(root, breakpoint);
        let mut sizes = HashMap::new();
        let mut text = TextEngine::new(self, project_path);
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
                    position(child, breakpoint),
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
