// Deterministic document preview shared by the canvas and, later, the runtime
// and export. The output contains no scripts and no external resources: assets
// are embedded as data URIs and every style comes from the document model.
use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    path::Path,
};

use serde_json::{json, Value};
use uuid::Uuid;

use super::domain::{
    FontStyle, ImageFit, NodeStyle, Overflow, TextAlign, TextWrap, VectorCommand, VectorShape,
};
use super::geometry::NodeBounds;
use super::{CoreError, Document, Node, NodeKind, Result};

const CSS_IDENTIFIER: &[u8] = b"-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

pub(super) fn text_align(value: TextAlign) -> &'static str {
    match value {
        TextAlign::Start => "start",
        TextAlign::Center => "center",
        TextAlign::End => "end",
        TextAlign::Justify => "justify",
    }
}

pub(super) fn image_fit(value: ImageFit) -> &'static str {
    match value {
        ImageFit::Fill => "fill",
        ImageFit::Contain => "contain",
        ImageFit::Cover => "cover",
        ImageFit::None => "none",
    }
}

pub(super) fn css_identifier(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => output.push(char::from(byte)),
            b'0'..=b'9' if index > 0 => output.push(char::from(byte)),
            _ => {
                let _ = write!(output, "\\{:x} ", byte);
            }
        }
    }
    output
}

pub(super) fn xml_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(ch),
        }
    }
    output
}

fn is_xml_character(ch: char) -> bool {
    matches!(ch,
        '\t' | '\n' | '\r'
            | '\u{20}'..='\u{D7FF}'
            | '\u{E000}'..='\u{FFFD}'
            | '\u{10000}'..='\u{10FFFF}')
}

pub(super) fn xml_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            ch if !is_xml_character(ch) => output.push('\u{FFFD}'),
            ch => output.push(ch),
        }
    }
    output
}

fn clean_css(value: &str, pointer: &str) -> Result<String> {
    if !value.is_empty() && value.bytes().all(|byte| CSS_IDENTIFIER.contains(&byte)) {
        return Ok(value.to_owned());
    }
    Err(CoreError::invalid(format!(
        "Font name at {pointer} contains unsupported characters"
    )))
}

fn number(value: f64) -> String {
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" {
        text = "0".into();
    }
    text
}

fn node_kind(node: &Node) -> &'static str {
    match node.kind {
        NodeKind::Frame => "k-frame",
        NodeKind::Group => "k-group",
        NodeKind::Text => "k-text",
        NodeKind::Image => "k-image",
        NodeKind::Vector => "k-vector",
        NodeKind::Button => "k-button",
        NodeKind::Input => "k-input",
        NodeKind::Textarea => "k-textarea",
        NodeKind::Checkbox => "k-checkbox",
        NodeKind::Select => "k-select",
        NodeKind::ComponentInstance => "k-instance",
        NodeKind::CodeComponent => "k-code",
    }
}

fn class_token(prefix: char, id: Uuid) -> String {
    format!("{prefix}{}", id.simple())
}

/// Most specific preview failure: a missing asset or font is reported before a
/// generic measurement error because it can be fixed by the user directly.
fn specific_error(document: &Document, project_path: &Path, page_id: Uuid) -> Option<CoreError> {
    let mut pending = vec![
        document
            .pages
            .iter()
            .find(|page| page.id == page_id)?
            .root_id,
    ];
    let nodes: HashMap<_, _> = document.nodes.iter().map(|node| (node.id, node)).collect();
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = nodes.get(&id)?;
        if let Some(style) = node.style.known() {
            if let Some(asset_id) = style.font_asset_id {
                if let Err(error) = font_bytes(document, project_path, asset_id) {
                    return Some(error);
                }
            }
        }
        if let Some(asset_id) = node.content.known().and_then(|content| content.asset_id) {
            if let Err(error) = image_bytes(document, project_path, asset_id) {
                return Some(error);
            }
        }
        pending.extend(node.children.iter().copied());
    }
    None
}

fn asset_path(document: &Document, asset_id: Uuid) -> Result<&str> {
    document
        .assets
        .iter()
        .find(|asset| asset.id == asset_id)
        .map(|asset| asset.path.as_str())
        .ok_or_else(|| CoreError::new("MISSING_ASSET", format!("Missing asset {asset_id}")))
}

fn asset_bytes(project_path: &Path, path: &str) -> Result<Vec<u8>> {
    super::super::storage::asset_bytes(project_path, path)
}

fn image_bytes(document: &Document, project_path: &Path, asset_id: Uuid) -> Result<Vec<u8>> {
    asset_bytes(project_path, asset_path(document, asset_id)?)
}

fn font_bytes(document: &Document, project_path: &Path, asset_id: Uuid) -> Result<Vec<u8>> {
    super::super::storage::font_asset(project_path, asset_path(document, asset_id)?)
}

fn mime_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("ttc") => "font/collection",
        _ => "application/octet-stream",
    }
}

fn data_uri(document: &Document, project_path: &Path, asset_id: Uuid) -> Result<String> {
    let path = asset_path(document, asset_id)?;
    let bytes = asset_bytes(project_path, path)?;
    use base64::Engine as _;
    Ok(format!(
        "data:{};base64,{}",
        mime_type(path),
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

struct Styles<'a> {
    document: &'a Document,
    project_path: &'a Path,
    fonts: HashMap<Option<Uuid>, Option<String>>,
}

impl<'a> Styles<'a> {
    fn font_family(&mut self, asset_id: Option<Uuid>) -> Result<Option<String>> {
        if let Some(name) = self.fonts.get(&asset_id) {
            return Ok(name.clone());
        }
        let name = match asset_id {
            None => None,
            Some(asset_id) => {
                let bytes = font_bytes(self.document, self.project_path, asset_id)?;
                let face = ttf_parser::Face::parse(&bytes, 0)
                    .map_err(|_| CoreError::invalid("Invalid font asset"))?;
                face.names()
                    .into_iter()
                    .find(|name| name.name_id == ttf_parser::name_id::FAMILY)
                    .and_then(|name| name.to_string())
                    .map(|name| css_identifier(&name))
            }
        };
        self.fonts.insert(asset_id, name.clone());
        Ok(name)
    }

    fn style(&mut self, node: &Node, style: &NodeStyle, bounds: &NodeBounds) -> Result<String> {
        let mut css = String::with_capacity(256);
        css.push_str(&format!(
            "left:{}px;top:{}px;width:{}px;height:{}px;",
            number(bounds.x),
            number(bounds.y),
            number(bounds.w.max(0.0)),
            number(bounds.h.max(0.0))
        ));
        if node.hidden {
            css.push_str("display:none;");
        }
        match (node.kind, style.overflow) {
            (NodeKind::Frame, Some(Overflow::Hidden | Overflow::Clip | Overflow::Scroll)) => {
                css.push_str("overflow:hidden;")
            }
            (NodeKind::Frame, Some(Overflow::Visible)) | (NodeKind::Frame, None) => {}
            _ => {}
        }
        if let Some(fill) = &style.fill {
            css.push_str(&format!("background-color:{};", xml_escape(fill)));
        }
        if let Some(opacity) = style.opacity {
            css.push_str(&format!("opacity:{};", number(opacity)));
        }
        let mut transforms = Vec::new();
        if let Some(rotate) = style.rotate.filter(|rotate| *rotate != 0.0) {
            transforms.push(format!("rotate({}deg)", number(rotate)));
        }
        if !transforms.is_empty() {
            css.push_str(&format!("transform:{};", transforms.join(" ")));
        }
        if let Some(stroke) = &style.stroke {
            css.push_str(&format!(
                "outline:{}px solid {};outline-offset:-{}px;",
                number(stroke.width),
                xml_escape(&stroke.color),
                number(stroke.width / 2.0)
            ));
        }
        if !style.shadows.is_empty() {
            let shadows: Vec<_> = style
                .shadows
                .iter()
                .map(|shadow| {
                    format!(
                        "{} {} {} {} {}",
                        number(shadow.x),
                        number(shadow.y),
                        number(shadow.blur),
                        number(shadow.spread),
                        xml_escape(&shadow.color)
                    )
                })
                .collect();
            css.push_str(&format!("box-shadow:{};", shadows.join(",")));
        }
        let radius_applies = matches!(
            node.kind,
            NodeKind::Frame
                | NodeKind::Image
                | NodeKind::Button
                | NodeKind::Input
                | NodeKind::Textarea
                | NodeKind::Checkbox
                | NodeKind::Select
                | NodeKind::ComponentInstance
                | NodeKind::CodeComponent
        );
        if radius_applies {
            if let Some(radius) = style.radius.filter(|radius| *radius > 0.0) {
                css.push_str(&format!("border-radius:{}px;", number(radius)));
            }
        }
        if matches!(node.kind, NodeKind::Text | NodeKind::Button) {
            if let Some(color) = &style.color {
                css.push_str(&format!("color:{};", xml_escape(color)));
            }
            let asset_family = self.font_family(style.font_asset_id)?;
            let family = match asset_family {
                Some(family) => Some(family),
                None => style
                    .font_family
                    .as_deref()
                    .map(|family| clean_css(family, "/style/fontFamily"))
                    .transpose()?,
            };
            let mut families = Vec::new();
            if let Some(family) = family {
                families.push(format!("\"{family}\""));
            } else if style.font_asset_id.is_none() {
                families.push("\"Helvetica Neue\"".into());
            }
            if let Some(fallback) = style.font_fallback.as_deref() {
                families.push(format!(
                    "\"{}\"",
                    clean_css(fallback, "/style/fontFallback")?
                ));
            }
            families.push("sans-serif".into());
            css.push_str(&format!("font-family:{};", families.join(",")));
            if let Some(size) = style.font_size {
                css.push_str(&format!("font-size:{}px;", number(size)));
            }
            if let Some(weight) = style.font_weight {
                css.push_str(&format!("font-weight:{weight};"));
            }
            if let Some(font_style) = style.font_style {
                css.push_str(match font_style {
                    FontStyle::Normal => "font-style:normal;",
                    FontStyle::Italic => "font-style:italic;",
                    FontStyle::Oblique => "font-style:oblique;",
                });
            }
            if let Some(line_height) = style.line_height {
                css.push_str(&format!("line-height:{}px;", number(line_height)));
            }
            if let Some(spacing) = style.letter_spacing {
                css.push_str(&format!("letter-spacing:{}px;", number(spacing)));
            }
            if let Some(align) = style.text_align {
                css.push_str(&format!("text-align:{};", text_align(align)));
            }
            css.push_str(match style.text_wrap {
                Some(TextWrap::None) => "white-space:pre;",
                Some(TextWrap::Word | TextWrap::WordOrGlyph) | None => "white-space:pre-wrap;",
            });
            // Without a line limit the label still must not spill outside its
            // frame: it is clipped at the box edge instead of overlapping
            // neighbours.
            css.push_str("overflow:hidden;");
            if let Some(lines) = style.max_lines {
                css.push_str(&format!(
                    "display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:{lines};"
                ));
            }
        }
        if matches!(node.kind, NodeKind::Image) {
            if let Some(fit) = style.fit {
                css.push_str(&format!("object-fit:{};", image_fit(fit)));
            }
        }
        Ok(css)
    }
}

fn vector_markup(vector: &super::domain::VectorData) -> String {
    let mut output = String::with_capacity(128);
    use std::fmt::Write as _;
    for path in &vector.paths {
        output.push_str("<path d=\"");
        for command in path {
            let _ = write!(
                output,
                "{} ",
                match command {
                    VectorCommand::MoveTo { x, y } => format!("M{} {}", number(*x), number(*y)),
                    VectorCommand::LineTo { x, y } => format!("L{} {}", number(*x), number(*y)),
                    VectorCommand::CurveTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x,
                        y,
                    } => format!(
                        "C{} {} {} {} {} {}",
                        number(*x1),
                        number(*y1),
                        number(*x2),
                        number(*y2),
                        number(*x),
                        number(*y)
                    ),
                    VectorCommand::Close {} => "Z".into(),
                }
            );
        }
        output.push_str("\"/>");
    }
    for shape in &vector.shapes {
        match shape {
            VectorShape::Rect { x, y, w, h, rx, ry } => {
                let _ = write!(
                    output,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
                    number(*x),
                    number(*y),
                    number(*w),
                    number(*h)
                );
                if let Some(rx) = rx {
                    let _ = write!(output, " rx=\"{}\"", number(*rx));
                }
                if let Some(ry) = ry {
                    let _ = write!(output, " ry=\"{}\"", number(*ry));
                }
                output.push_str("/>");
            }
            VectorShape::Circle { cx, cy, r } => {
                let _ = write!(
                    output,
                    "<circle cx=\"{}\" cy=\"{}\" r=\"{}\"/>",
                    number(*cx),
                    number(*cy),
                    number(*r)
                );
            }
            VectorShape::Ellipse { cx, cy, rx, ry } => {
                let _ = write!(
                    output,
                    "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\"/>",
                    number(*cx),
                    number(*cy),
                    number(*rx),
                    number(*ry)
                );
            }
            VectorShape::Line { x1, y1, x2, y2 } => {
                let _ = write!(
                    output,
                    "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
                    number(*x1),
                    number(*y1),
                    number(*x2),
                    number(*y2)
                );
            }
            VectorShape::Polyline { points } | VectorShape::Polygon { points } => {
                let tag = if matches!(shape, VectorShape::Polygon { .. }) {
                    "polygon"
                } else {
                    "polyline"
                };
                let _ = write!(output, "<{tag} points=\"");
                for point in points {
                    let _ = write!(output, "{} {},", number(point.x), number(point.y));
                }
                output.pop();
                let _ = write!(output, "\"/>");
            }
        }
    }
    output
}

fn svg_attributes(style: &NodeStyle) -> String {
    let mut attributes = String::new();
    if let Some(fill) = &style.fill {
        let _ = write!(attributes, " fill=\"{}\"", xml_escape(fill));
    }
    if let Some(stroke) = &style.stroke {
        let _ = write!(
            attributes,
            " stroke=\"{}\" stroke-width=\"{}\" fill=\"none\"",
            xml_escape(&stroke.color),
            number(stroke.width)
        );
    }
    attributes
}

struct Renderer<'a> {
    document: &'a Document,
    page_id: Uuid,
    bounds: HashMap<Uuid, NodeBounds>,
    nodes: HashMap<Uuid, &'a Node>,
    styles: Styles<'a>,
    images: HashMap<Uuid, String>,
    fonts: Vec<String>,
    seen_fonts: HashSet<String>,
}

impl<'a> Renderer<'a> {
    fn image_uri(&mut self, asset_id: Uuid) -> Result<&str> {
        if !self.images.contains_key(&asset_id) {
            let uri = data_uri(self.document, self.styles.project_path, asset_id)?;
            self.images.insert(asset_id, uri);
        }
        Ok(&self.images[&asset_id])
    }

    fn node(&mut self, node: &'a Node) -> Result<String> {
        let bounds =
            self.bounds.get(&node.id).copied().ok_or_else(|| {
                CoreError::invalid(format!("Missing layout for node {}", node.id))
            })?;
        let class = class_token('n', node.id);
        let data_id = node.id.to_string();
        let style = node
            .style
            .known()
            .cloned()
            .unwrap_or_else(NodeStyle::default);
        let css = self.styles.style(node, &style, &bounds)?;
        let mut html = format!(
            "<div class=\"node {class} {}\" data-id=\"{data_id}\" style=\"{}\">",
            node_kind(node),
            xml_escape(&css)
        );
        match node.kind {
            NodeKind::Text | NodeKind::Button => {
                if let Some(asset_id) = style.font_asset_id {
                    if let Some(name) = self.styles.font_family(Some(asset_id))? {
                        if self.seen_fonts.insert(name.clone()) {
                            self.fonts.push(name);
                        }
                    }
                }
                let text = node
                    .content
                    .known()
                    .and_then(|content| content.text.as_deref())
                    .unwrap_or("");
                html.push_str(&format!("<span class=\"text\">{}</span>", xml_text(text)));
            }
            NodeKind::Image => {
                if let Some(asset_id) = node.content.known().and_then(|content| content.asset_id) {
                    let uri = self.image_uri(asset_id)?.to_owned();
                    html.push_str(&format!("<img alt=\"{}\" src=\"{uri}\"/>", {
                        let alt = node
                            .a11y
                            .recognized()
                            .and_then(|a11y| a11y.alt.clone().or_else(|| a11y.label.clone()))
                            .unwrap_or_default();
                        xml_escape(&alt)
                    }));
                }
            }
            NodeKind::Vector => {
                if let Some(content) = node.content.known() {
                    if let Some(vector) = &content.vector {
                        let (w, h) = (bounds.w.max(0.0), bounds.h.max(0.0));
                        html.push_str(&format!(
                            "<svg width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" preserveAspectRatio=\"none\"{}>{}</svg>",
                            number(w),
                            number(h),
                            number(w),
                            number(h),
                            svg_attributes(&style),
                            vector_markup(vector)
                        ));
                    }
                }
            }
            NodeKind::Input | NodeKind::Textarea => {
                let placeholder = node
                    .content
                    .known()
                    .and_then(|content| content.placeholder.as_deref())
                    .unwrap_or("");
                html.push_str(&format!(
                    "<span class=\"placeholder\">{}</span>",
                    xml_text(placeholder)
                ));
            }
            NodeKind::Frame | NodeKind::Group | NodeKind::ComponentInstance => {
                for child in &node.children {
                    if let Some(child) = self.nodes.get(child) {
                        html.push_str(&self.node(child)?);
                    }
                }
            }
            NodeKind::Checkbox | NodeKind::Select | NodeKind::CodeComponent => {}
        }
        html.push_str("</div>");
        Ok(html)
    }

    fn page(&mut self) -> Result<String> {
        let page = self
            .document
            .pages
            .iter()
            .find(|page| page.id == self.page_id)
            .ok_or_else(|| CoreError::invalid("Unknown preview page"))?;
        let root = *self
            .nodes
            .get(&page.root_id)
            .ok_or_else(|| CoreError::invalid("Unknown page root"))?;
        let bounds = self.bounds[&root.id];
        let mut html = format!(
            "<div class=\"page\" style=\"width:{}px;height:{}px\">\n",
            number(bounds.w.max(0.0)),
            number(bounds.h.max(0.0))
        );
        // Groups paint without a background; their children are positioned in
        // page space, so they are emitted after the group box.
        html.push_str(&self.node(root)?);
        html.push_str("\n</div>");
        Ok(html)
    }
}

impl Document {
    /// Computed absolute bounds for a page, as JSON records.
    pub fn layout_json(&self, project_path: &Path, page_id: Uuid) -> Result<String> {
        let bounds = self.layout_bounds(project_path, page_id)?;
        let visibility: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|node| (node.id, (node.hidden, node.kind)))
            .collect();
        let records: Vec<Value> = bounds
            .iter()
            .map(|bounds| {
                let (hidden, kind) = visibility[&bounds.id];
                let node = self
                    .nodes
                    .iter()
                    .find(|node| node.id == bounds.id)
                    .ok_or_else(|| CoreError::invalid("Missing node bounds"))?;
                Ok(json!({
                    "id": bounds.id,
                    "x": bounds.x,
                    "y": bounds.y,
                    "w": bounds.w,
                    "h": bounds.h,
                    "hidden": hidden,
                    "kind": kind.token(),
                    "inFlow": bounds.in_flow,
                    "parentId": node.parent_id,
                    "rotation": node.style.known().and_then(|style| style.rotate).unwrap_or(0.0),
                }))
            })
            .collect::<Result<Vec<Value>>>()?;
        serde_json::to_string(&records).map_err(|e| CoreError::invalid(e.to_string()))
    }

    /// A self-contained HTML document of one page. The same bytes are produced
    /// for the same document and project assets.
    pub fn preview_html(&self, project_path: &Path, page_id: Uuid) -> Result<String> {
        let bounds = match self.layout_bounds(project_path, page_id) {
            Ok(bounds) => bounds,
            Err(error) => {
                return Err(specific_error(self, project_path, page_id).unwrap_or(error));
            }
        };
        let page = self
            .pages
            .iter()
            .find(|page| page.id == page_id)
            .ok_or_else(|| CoreError::invalid("Unknown page"))?;
        let mut faces = String::new();
        for node in &self.nodes {
            let Some(style) = node.style.known() else {
                continue;
            };
            let Some(asset_id) = style.font_asset_id else {
                continue;
            };
            let bytes = font_bytes(self, project_path, asset_id)?;
            let face = ttf_parser::Face::parse(&bytes, 0)
                .map_err(|_| CoreError::invalid("Invalid font asset"))?;
            let Some(name) = face
                .names()
                .into_iter()
                .find(|name| name.name_id == ttf_parser::name_id::FAMILY)
                .and_then(|name| name.to_string())
            else {
                continue;
            };
            use base64::Engine as _;
            faces.push_str(&format!(
                "@font-face{{font-family:\"{}\";src:url(data:{};base64,{}) format(\"{}\");}}\n",
                css_identifier(&name),
                if bytes.starts_with(b"OTTO") {
                    "font/otf"
                } else {
                    "font/ttf"
                },
                base64::engine::general_purpose::STANDARD.encode(&bytes),
                if bytes.starts_with(b"OTTO") {
                    "opentype"
                } else {
                    "truetype"
                }
            ));
        }
        let mut renderer = Renderer {
            document: self,
            page_id,
            bounds: bounds.iter().map(|bounds| (bounds.id, *bounds)).collect(),
            nodes: self.nodes.iter().map(|node| (node.id, node)).collect(),
            styles: Styles {
                document: self,
                project_path,
                fonts: HashMap::new(),
            },
            images: HashMap::new(),
            fonts: Vec::new(),
            seen_fonts: HashSet::new(),
        };
        let content = renderer.page()?;
        let root = bounds
            .iter()
            .find(|bounds| bounds.id == page.root_id)
            .copied()
            .ok_or_else(|| CoreError::invalid("Unknown page root"))?;
        let mut html = String::with_capacity(content.len() + 2048);
        html.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'; frame-src 'none'\"><title>");
        html.push_str(&xml_text(&page.name));
        html.push_str("</title><style>\n");
        faces.push_str("html,body{margin:0;padding:0;background:#ffffff;}\n.page{position:relative;overflow:hidden;}\n.node{position:absolute;box-sizing:border-box;margin:0;padding:0;border:0;}\n.text,.placeholder{display:block;width:100%;height:100%;white-space:inherit;}\n.node.k-button .text,.node.k-input .placeholder,.node.k-textarea .placeholder,.node.k-select .placeholder{display:flex;align-items:center;justify-content:center;padding:0 12px;text-align:center;box-sizing:border-box;}\n.node.k-input .placeholder,.node.k-textarea .placeholder,.node.k-select .placeholder{justify-content:flex-start;text-align:left;}\n.node.k-textarea .placeholder{align-items:flex-start;padding-top:10px;}\n.node img{display:block;width:100%;height:100%;}\n.node svg{display:block;}\n");
        html.push_str(&faces);
        html.push_str("</style></head><body><div id=\"canvas\" style=\"position:relative;width:");
        html.push_str(&number(root.w.max(0.0)));
        html.push_str("px;height:");
        html.push_str(&number(root.h.max(0.0)));
        html.push_str("px\">");
        html.push_str(&content);
        html.push_str("</div></body></html>");
        Ok(html)
    }
}

impl crate::ProjectSession {
    /// Computed bounds of every node on `page_id`, or of the first page.
    pub fn layout_json(&self, page_id: Option<&str>) -> Result<String> {
        let document = self.state()?.model()?;
        let page_id = match page_id {
            Some(value) => {
                Uuid::parse_str(value).map_err(|_| CoreError::invalid("Invalid page ID"))?
            }
            None => document
                .pages
                .first()
                .map(|page| page.id)
                .ok_or_else(|| CoreError::invalid("Project has no pages"))?,
        };
        document.layout_json(&self.path, page_id)
    }

    /// Self-contained preview of `page_id`, or of the first page.
    pub fn preview_html(&self, page_id: Option<&str>) -> Result<String> {
        let document = self.state()?.model()?;
        let page_id = match page_id {
            Some(value) => {
                Uuid::parse_str(value).map_err(|_| CoreError::invalid("Invalid page ID"))?
            }
            None => document
                .pages
                .first()
                .map(|page| page.id)
                .ok_or_else(|| CoreError::invalid("Project has no pages"))?,
        };
        let html = document.preview_html(&self.path, page_id)?;
        // WKWebView and export consume this string; scripts would bypass the
        // restricted inspection channel.
        if html.to_ascii_lowercase().contains("<script") {
            return Err(CoreError::invalid("Preview unexpectedly contains a script"));
        }
        Ok(html)
    }

    /// Byte offsets where wrapped lines of a text node start at `width`.
    pub fn line_breaks_json(&self, node_id: &str, width: f64) -> Result<String> {
        let node_id =
            Uuid::parse_str(node_id).map_err(|_| CoreError::invalid("Invalid node ID"))?;
        let document = self.state()?.model()?;
        let breaks = document.line_breaks(&self.path, node_id, width)?;
        serde_json::to_string(&breaks).map_err(|e| CoreError::invalid(e.to_string()))
    }
}
