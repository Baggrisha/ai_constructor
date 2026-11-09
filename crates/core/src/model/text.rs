use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, LazyLock},
};

use cosmic_text::{
    fontdb::{self, Database, Query, Source},
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, Weight, Wrap,
};
use unicode_linebreak::{break_property, BreakClass};
use uuid::Uuid;

use super::{
    domain::{FontStyle, NodeStyle, TextWrap},
    CoreError, Document, Node, Result,
};

#[derive(Hash, PartialEq, Eq)]
struct FontKey {
    asset: Option<Uuid>,
    family: Option<String>,
    fallback: Option<String>,
}

struct FontSet {
    system: FontSystem,
    family: String,
}

pub(super) struct TextEngine<'a> {
    document: &'a Document,
    project_path: &'a Path,
    fonts: HashMap<FontKey, FontSet>,
}

fn system_fonts() -> &'static Database {
    static FONTS: LazyLock<Database> = LazyLock::new(|| {
        let mut db = Database::new();
        db.load_system_fonts();
        db
    });
    &FONTS
}

fn family_name(db: &Database, requested: &str) -> Option<String> {
    db.faces()
        .flat_map(|face| &face.families)
        .find(|(name, _)| name.eq_ignore_ascii_case(requested))
        .map(|(name, _)| name.clone())
}

fn default_family(db: &Database) -> Option<String> {
    [
        "Arial",
        "Helvetica",
        "DejaVu Sans",
        "Liberation Sans",
        "Noto Sans",
    ]
    .iter()
    .find_map(|name| family_name(db, name))
}

fn only_families(db: &mut Database, names: &[&str]) {
    let remove: Vec<_> = db
        .faces()
        .filter(|face| {
            !face
                .families
                .iter()
                .any(|(family, _)| names.contains(&family.as_str()))
        })
        .map(|face| face.id)
        .collect();
    for id in remove {
        db.remove_face(id);
    }
}

fn missing_font(name: &str) -> CoreError {
    CoreError::new("MISSING_FONT", format!("Font is unavailable: {name}"))
}

fn load_fonts(document: &Document, project_path: &Path, key: &FontKey) -> Result<FontSet> {
    let (mut db, primary) = if let Some(asset_id) = key.asset {
        let asset = document
            .assets
            .iter()
            .find(|asset| asset.id == asset_id)
            .ok_or_else(|| {
                CoreError::new("MISSING_ASSET", format!("Missing font asset {asset_id}"))
            })?;
        let bytes = crate::storage::font_asset(project_path, &asset.path)?;
        let source = Source::Binary(Arc::new(bytes));
        let mut asset_db = Database::new();
        asset_db.load_font_source(source.clone());
        let primary = if let Some(requested) = &key.family {
            family_name(&asset_db, requested).ok_or_else(|| missing_font(requested))?
        } else {
            let names: HashSet<_> = asset_db
                .faces()
                .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
                .collect();
            if names.len() != 1 {
                return Err(CoreError::invalid(
                    "Font asset must contain one family, or fontFamily must select one",
                ));
            }
            names.into_iter().next().expect("one family")
        };
        if let Some(fallback) = &key.fallback {
            let mut system = system_fonts().clone();
            let fallback = family_name(&system, fallback).ok_or_else(|| missing_font(fallback))?;
            if fallback.eq_ignore_ascii_case(&primary) {
                return Err(CoreError::invalid(
                    "Font fallback must differ from the asset family",
                ));
            }
            only_families(&mut system, &[&fallback]);
            system.load_font_source(source);
            only_families(&mut system, &[&primary, &fallback]);
            (system, primary)
        } else {
            only_families(&mut asset_db, &[&primary]);
            (asset_db, primary)
        }
    } else {
        let mut system = system_fonts().clone();
        let primary = match &key.family {
            Some(name) if name == "sans-serif" => {
                default_family(&system).ok_or_else(|| missing_font(name))?
            }
            Some(name) => family_name(&system, name).ok_or_else(|| missing_font(name))?,
            None => default_family(&system).ok_or_else(|| missing_font("system sans-serif"))?,
        };
        let fallback = key
            .fallback
            .as_ref()
            .map(|name| family_name(&system, name).ok_or_else(|| missing_font(name)))
            .transpose()?;
        if let Some(fallback) = &fallback {
            only_families(&mut system, &[&primary, fallback]);
        } else {
            only_families(&mut system, &[&primary]);
        }
        (system, primary)
    };
    db.set_sans_serif_family(&primary);
    Ok(FontSet {
        system: FontSystem::new_with_locale_and_db("en-US".to_owned(), db),
        family: primary,
    })
}
#[derive(Clone, Copy)]
struct FaceMetrics {
    ascent: f32,
    descent: f32,
    gap: f32,
}

impl FaceMetrics {
    fn height(self) -> f32 {
        self.ascent + self.descent + self.gap
    }

    fn include(&mut self, other: Self) {
        self.ascent = self.ascent.max(other.ascent);
        self.descent = self.descent.max(other.descent);
        self.gap = self.gap.max(other.gap);
    }
}

fn face_metrics(
    db: &Database,
    id: fontdb::ID,
    size: f32,
    weight: Weight,
    style: Style,
) -> Result<FaceMetrics> {
    let info = db.face(id).ok_or_else(|| missing_font("selected face"))?;
    if info.style != style {
        return Err(missing_font(&format!(
            "{} ({style:?})",
            info.post_script_name
        )));
    }
    db.with_face_data(id, |bytes, index| {
        let mut face = ttf_parser::Face::parse(bytes, index).ok()?;
        let weight_axis = face
            .variation_axes()
            .into_iter()
            .find(|axis| axis.tag == ttf_parser::Tag::from_bytes(b"wght"));
        if let Some(axis) = weight_axis {
            if f32::from(weight.0) < axis.min_value || f32::from(weight.0) > axis.max_value {
                return None;
            }
            face.set_variation(axis.tag, f32::from(weight.0))?;
        } else if info.weight != weight {
            return None;
        }
        let scale = size / f32::from(face.units_per_em());
        let metrics = FaceMetrics {
            ascent: f32::from(face.ascender()) * scale,
            descent: -f32::from(face.descender()) * scale,
            gap: f32::from(face.line_gap()) * scale,
        };
        (metrics.height().is_finite() && metrics.height() > 0.0).then_some(metrics)
    })
    .flatten()
    .ok_or_else(|| missing_font(&format!("{} (weight {})", info.post_script_name, weight.0)))
}

impl<'a> TextEngine<'a> {
    pub(super) fn new(document: &'a Document, project_path: &'a Path) -> Self {
        Self {
            document,
            project_path,
            fonts: HashMap::new(),
        }
    }

    pub(super) fn measure(&mut self, node: &Node, width: Option<f64>) -> Result<(f64, f64)> {
        let style = node
            .style
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown text style"))?;
        let content = node
            .content
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown text content"))?;
        let bindings = node
            .bindings
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown text bindings"))?;
        if bindings.keys().any(|path| {
            matches!(
                path.as_str(),
                "/style/fontFamily"
                    | "/style/fontFallback"
                    | "/style/fontSize"
                    | "/style/fontWeight"
                    | "/style/lineHeight"
                    | "/style/letterSpacing"
                    | "/content/text"
            )
        }) {
            return Err(CoreError::unsupported(
                "Bound typography requires resolved values",
            ));
        }
        let key = FontKey {
            asset: style.font_asset_id,
            family: style.font_family.clone(),
            fallback: style.font_fallback.clone(),
        };
        let fonts = match self.fonts.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let fonts = load_fonts(self.document, self.project_path, entry.key())?;
                entry.insert(fonts)
            }
        };
        shape(fonts, style, content.text.as_deref().unwrap_or(""), width)
    }

    /// Byte offsets in the original text where a wrapped line starts.
    pub(super) fn line_breaks(&mut self, node: &Node, width: f64) -> Result<Vec<usize>> {
        let style = node
            .style
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown text style"))?;
        let content = node
            .content
            .known()
            .ok_or_else(|| CoreError::unsupported("Unknown text content"))?;
        let key = FontKey {
            asset: style.font_asset_id,
            family: style.font_family.clone(),
            fallback: style.font_fallback.clone(),
        };
        let fonts = match self.fonts.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let fonts = load_fonts(self.document, self.project_path, entry.key())?;
                entry.insert(fonts)
            }
        };
        shape_line_breaks(
            fonts,
            style,
            content.text.as_deref().unwrap_or(""),
            width as f32,
        )
    }
}

// COSMIC's editor lines recognize CR/LF only. Adapt Unicode BK/CR/LF/NL
// hard separators to layout paragraphs without changing the document text.
fn layout_text(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|ch| ch != '\n' && hard_break(ch)) {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if hard_break(ch) {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }
    Cow::Owned(normalized)
}

fn hard_break(ch: char) -> bool {
    matches!(
        break_property(u32::from(ch)),
        BreakClass::Mandatory
            | BreakClass::CarriageReturn
            | BreakClass::LineFeed
            | BreakClass::NextLine
    )
}

fn style_font_face<'a>(fonts: &'a mut FontSet, style: &NodeStyle) -> Result<&'a mut FontSystem> {
    let weight = Weight(style.font_weight.unwrap_or(400));
    let font_style = match style.font_style.unwrap_or(FontStyle::Normal) {
        FontStyle::Normal => Style::Normal,
        FontStyle::Italic => Style::Italic,
        FontStyle::Oblique => Style::Oblique,
    };
    let face = fonts
        .system
        .db()
        .query(&Query {
            families: &[Family::Name(&fonts.family)],
            weight,
            style: font_style,
            ..Query::default()
        })
        .ok_or_else(|| missing_font(&fonts.family))?;
    let _ = face;
    Ok(&mut fonts.system)
}

fn shape_line_breaks(
    fonts: &mut FontSet,
    style: &NodeStyle,
    text: &str,
    width: f32,
) -> Result<Vec<usize>> {
    if !width.is_finite() || width < 0.0 {
        return Err(CoreError::invalid("Text width exceeds supported range"));
    }
    let family = fonts.family.clone();
    let system = style_font_face(fonts, style)?;
    let size = style.font_size.unwrap_or(16.0) as f32;
    let weight = Weight(style.font_weight.unwrap_or(400));
    let font_style = match style.font_style.unwrap_or(FontStyle::Normal) {
        FontStyle::Normal => Style::Normal,
        FontStyle::Italic => Style::Italic,
        FontStyle::Oblique => Style::Oblique,
    };
    let tracking = (style.letter_spacing.unwrap_or(0.0) / f64::from(size)) as f32;
    if !tracking.is_finite() {
        return Err(CoreError::invalid("Letter spacing exceeds supported range"));
    }
    let face = system
        .db()
        .query(&Query {
            families: &[Family::Name(&family)],
            weight,
            style: font_style,
            ..Query::default()
        })
        .ok_or_else(|| missing_font(&family))?;
    let base = face_metrics(system.db(), face, size, weight, font_style)?;
    let line_height = style
        .line_height
        .map_or(base.height(), |value| value as f32);
    let mut buffer = Buffer::new(system, Metrics::new(size, line_height));
    buffer.set_size(Some(width), None);
    buffer.set_wrap(match style.text_wrap.unwrap_or(TextWrap::WordOrGlyph) {
        TextWrap::None => Wrap::None,
        TextWrap::Word => Wrap::Word,
        TextWrap::WordOrGlyph => Wrap::WordOrGlyph,
    });
    let attrs = Attrs::new()
        .family(Family::Name(&family))
        .weight(weight)
        .style(font_style)
        .letter_spacing(tracking);
    let normalized = layout_text(text);
    buffer.set_text(&normalized, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(system, false);
    // Runs are indexed into the normalized text; map the byte offset back
    // through the same paragraph separator when hard breaks were substituted.
    let map_offset = |offset: usize| -> usize {
        match &normalized {
            Cow::Borrowed(_) => offset,
            Cow::Owned(_) => text
                .char_indices()
                .map(|(index, _)| index)
                .nth(normalized[..offset].chars().count())
                .unwrap_or(text.len()),
        }
    };
    let mut breaks = Vec::new();
    for run in buffer.layout_runs() {
        let Some(first) = run.glyphs.first() else {
            continue;
        };
        let offset = map_offset(first.start);
        if offset > 0 {
            breaks.push(offset);
        }
    }
    breaks.dedup();
    Ok(breaks)
}

fn shape(
    fonts: &mut FontSet,
    style: &NodeStyle,
    text: &str,
    width: Option<f64>,
) -> Result<(f64, f64)> {
    let size = style.font_size.unwrap_or(16.0) as f32;
    let weight = Weight(style.font_weight.unwrap_or(400));
    let font_style = match style.font_style.unwrap_or(FontStyle::Normal) {
        FontStyle::Normal => Style::Normal,
        FontStyle::Italic => Style::Italic,
        FontStyle::Oblique => Style::Oblique,
    };
    let family = Family::Name(&fonts.family);
    let face = fonts
        .system
        .db()
        .query(&Query {
            families: &[family],
            weight,
            style: font_style,
            ..Query::default()
        })
        .ok_or_else(|| missing_font(&fonts.family))?;
    let base_metrics = face_metrics(fonts.system.db(), face, size, weight, font_style)?;
    let line_height = style
        .line_height
        .map_or(base_metrics.height(), |value| value as f32);
    let tracking = (style.letter_spacing.unwrap_or(0.0) / f64::from(size)) as f32;
    if !tracking.is_finite() {
        return Err(CoreError::invalid("Letter spacing exceeds supported range"));
    }
    let width = width.map(|n| n as f32);
    if width.is_some_and(|n| !n.is_finite() || n < 0.0) {
        return Err(CoreError::invalid("Text width exceeds supported range"));
    }
    let mut buffer = Buffer::new(&mut fonts.system, Metrics::new(size, line_height));
    buffer.set_size(width, None);
    buffer.set_wrap(match style.text_wrap.unwrap_or(TextWrap::WordOrGlyph) {
        TextWrap::None => Wrap::None,
        TextWrap::Word => Wrap::Word,
        TextWrap::WordOrGlyph => Wrap::WordOrGlyph,
    });
    let attrs = Attrs::new()
        .family(family)
        .weight(weight)
        .style(font_style)
        .letter_spacing(tracking);
    let text = layout_text(text);
    buffer.set_text(&text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(&mut fonts.system, false);
    let mut max_width = 0.0_f32;
    let mut height = 0.0_f64;
    let mut metrics = HashMap::new();
    metrics.insert(face, base_metrics);
    for run in buffer.layout_runs() {
        max_width = max_width.max(run.line_w);
        let mut actual_metrics = base_metrics;
        for glyph in run.glyphs {
            if glyph.glyph_id == 0 {
                return Err(CoreError::new(
                    "MISSING_GLYPH",
                    format!("Font does not cover text on node using {}", fonts.family),
                ));
            }
            let glyph_metrics = match metrics.get(&glyph.font_id) {
                Some(metrics) => *metrics,
                None => {
                    let face =
                        face_metrics(fonts.system.db(), glyph.font_id, size, weight, font_style)?;
                    metrics.insert(glyph.font_id, face);
                    face
                }
            };
            actual_metrics.include(glyph_metrics);
        }
        height += f64::from(if style.line_height.is_some() {
            line_height
        } else {
            actual_metrics.height()
        });
    }
    if !max_width.is_finite() || !height.is_finite() {
        return Err(CoreError::invalid("Text layout overflow"));
    }
    Ok((f64::from(max_width), height))
}

#[cfg(test)]
mod tests {
    use super::{default_family, shape, Database, FontSet, FontSystem, NodeStyle};

    #[test]
    fn empty_font_database_has_no_implicit_fallback() {
        let db = Database::new();
        assert!(default_family(&db).is_none());
        let mut fonts = FontSet {
            system: FontSystem::new_with_locale_and_db("en-US".to_owned(), db),
            family: "missing".to_owned(),
        };
        assert_eq!(
            shape(&mut fonts, &NodeStyle::default(), "Hello", None)
                .unwrap_err()
                .code,
            "MISSING_FONT"
        );
    }
}
