use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Cursor, Read},
    path::Path,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use image::{ImageFormat, ImageReader};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{model::Document, CoreError, ProjectSession, Result};

const MAX_ASSET_BYTES: u64 = 50 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_FONT_FACES: u32 = 64;
const MAX_FONT_TABLES: u16 = 128;

fn image_error(error: image::ImageError) -> CoreError {
    if matches!(error, image::ImageError::Limits(_)) {
        CoreError::import_limit(format!("Image exceeds decoder resource limits: {error}"))
    } else {
        CoreError::invalid(format!("Invalid image: {error}"))
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    // FIFO opens must not block before the regular-file check (including a path swap).
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(CoreError::invalid("Asset must be a regular file"));
    }
    if metadata.len() > MAX_ASSET_BYTES {
        return Err(CoreError::import_limit("Asset exceeds 50 MiB"));
    }
    let mut bytes = Vec::new();
    let mut input = file.take(MAX_ASSET_BYTES + 1);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        super::control::check()?;
        let count = input.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if bytes.len() as u64 > MAX_ASSET_BYTES {
        return Err(CoreError::import_limit("Asset exceeds 50 MiB"));
    }
    if bytes.is_empty() {
        return Err(CoreError::invalid("Asset is empty"));
    }
    Ok(bytes)
}

fn font_extension(bytes: &[u8]) -> Result<Option<&'static str>> {
    let extension = match bytes.get(..4) {
        Some(b"\0\x01\0\0" | b"true") => "ttf",
        Some(b"OTTO") => "otf",
        Some(b"ttcf") => "ttc",
        _ => return Ok(None),
    };
    let (faces, header_len) = if extension == "ttc" {
        let faces = ttf_parser::fonts_in_collection(bytes)
            .ok_or_else(|| CoreError::invalid("Truncated font collection header"))?;
        // Bound the attacker-controlled count before iteration or fontdb allocation.
        if faces > MAX_FONT_FACES {
            return Err(CoreError::import_limit("Font collection exceeds 64 faces"));
        }
        if faces == 0 {
            return Err(CoreError::invalid("Font collection must contain a face"));
        }
        let extra = match bytes.get(4..8) {
            Some(b"\0\x01\0\0") => 0,
            Some(b"\0\x02\0\0") => 12,
            _ => return Err(CoreError::invalid("Invalid font collection version")),
        };
        let header_len = usize::try_from(faces)
            .ok()
            .and_then(|count| count.checked_mul(4))
            .and_then(|offsets| offsets.checked_add(12 + extra))
            .ok_or_else(|| CoreError::invalid("Invalid font collection offsets"))?;
        if bytes.len() < header_len {
            return Err(CoreError::invalid("Truncated font collection offsets"));
        }
        (faces, header_len)
    } else {
        (1, 0)
    };
    for index in 0..faces {
        super::control::check()?;
        if header_len != 0 {
            let pos = 12 + index as usize * 4;
            let offset =
                u32::from_be_bytes(bytes[pos..pos + 4].try_into().expect("checked TTC header"));
            if usize::try_from(offset).ok().is_none_or(|offset| {
                offset < header_len || offset.checked_add(12).is_none_or(|end| end > bytes.len())
            }) {
                return Err(CoreError::invalid("Invalid font face offset"));
            }
        }
        let raw = ttf_parser::RawFace::parse(bytes, index)
            .map_err(|_| CoreError::invalid("Invalid font face directory"))?;
        if raw.table_records.len() > MAX_FONT_TABLES {
            return Err(CoreError::import_limit("Font face exceeds 128 tables"));
        }
        for table in raw.table_records {
            let end = usize::try_from(table.offset)
                .ok()
                .zip(usize::try_from(table.length).ok())
                .and_then(|(offset, length)| offset.checked_add(length));
            if end.is_none_or(|end| end > bytes.len()) {
                return Err(CoreError::invalid("Font table exceeds asset bounds"));
            }
        }
        ttf_parser::Face::parse(bytes, index)
            .map_err(|_| CoreError::invalid("Invalid font asset"))?;
    }
    Ok(Some(extension))
}

pub(crate) fn font_asset(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = super::safe_path(root, relative)?;
    let bytes = read_bounded(&path).map_err(|error| {
        if !path.exists() {
            CoreError::new("MISSING_ASSET", format!("Missing font asset {relative}"))
        } else {
            error
        }
    })?;
    let expected = relative
        .strip_prefix("assets/")
        .and_then(|name| name.split_once('.'))
        .map(|(digest, _)| digest)
        .ok_or_else(|| CoreError::invalid("Invalid font asset path"))?;
    if super::hash(&bytes) != expected {
        return Err(CoreError::conflict(format!(
            "Font asset changed: {relative}"
        )));
    }
    if font_extension(&bytes)?.is_none() {
        return Err(CoreError::invalid(format!(
            "Asset is not a font: {relative}"
        )));
    }
    Ok(bytes)
}

fn read_asset(path: &Path) -> Result<(Vec<u8>, &'static str)> {
    let bytes = read_bounded(path)?;
    if let Some(extension) = font_extension(&bytes)? {
        return Ok((bytes, extension));
    }
    let extension = match image::guess_format(&bytes) {
        Ok(
            format @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP),
        ) => {
            let (width, height) = ImageReader::with_format(Cursor::new(&bytes), format)
                .into_dimensions()
                .map_err(image_error)?;
            if width == 0 || height == 0 {
                return Err(CoreError::invalid("Image dimensions must be positive"));
            }
            if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
                return Err(CoreError::import_limit(
                    "Image dimensions exceed the supported limit of 64 Mi pixels",
                ));
            }
            super::control::check()?;
            image::load_from_memory_with_format(&bytes, format).map_err(image_error)?;
            super::control::check()?;
            match format {
                ImageFormat::Png => "png",
                ImageFormat::Jpeg => "jpg",
                ImageFormat::Gif => "gif",
                ImageFormat::WebP => "webp",
                _ => unreachable!(),
            }
        }
        _ => {
            let source = std::str::from_utf8(&bytes).map_err(|_| {
                CoreError::invalid(
                    "Unsupported asset type; use PNG, JPEG, GIF, WebP, SVG, TTF, OTF or TTC",
                )
            })?;
            let xml = roxmltree::Document::parse(source)
                .map_err(|e| CoreError::invalid(format!("Invalid SVG: {e}")))?;
            let root = xml.root_element();
            if root.tag_name().name() != "svg"
                || root.tag_name().namespace() != Some("http://www.w3.org/2000/svg")
            {
                return Err(CoreError::invalid("Asset is not an SVG document"));
            }
            // SVG is persisted for later display: reject executable or external references.
            for node in xml.descendants().filter(|node| node.is_element()) {
                let name = node.tag_name().name();
                if matches!(name, "script" | "foreignObject") {
                    return Err(CoreError::invalid(
                        "SVG contains unsupported active content",
                    ));
                }
                for attribute in node.attributes() {
                    let name = attribute.name();
                    let value = attribute.value().trim();
                    if name.to_ascii_lowercase().starts_with("on")
                        || (name == "href" && !value.starts_with('#'))
                        || value.to_ascii_lowercase().contains("url(")
                    {
                        return Err(CoreError::invalid(
                            "SVG contains an active or external reference",
                        ));
                    }
                }
            }
            "svg"
        }
    };
    Ok((bytes, extension))
}

type ObservedFilesAndMarker = (BTreeMap<String, String>, Option<Vec<u8>>);

fn prepare_relink(
    session: &ProjectSession,
    state: &crate::State,
    old_path: Option<&str>,
    replacement_path: &str,
    shared: bool,
    replacement: &[u8],
) -> Result<Option<ObservedFilesAndMarker>> {
    let _lock = super::lock(&session.path)?;
    if super::recover(&session.path)?.is_some() {
        return Err(CoreError::conflict(
            "Resolve the pending recovery before relinking",
        ));
    }
    if fs::read(session.path.join("project.json"))? != state.manifest_bytes {
        return Err(CoreError::conflict("Project manifest changed on disk"));
    }
    let observed = super::scan(&session.path)?;
    if observed == state.manifest.files {
        return Ok(None);
    }
    let old_path = old_path.ok_or_else(|| CoreError::conflict("Project files changed on disk"))?;
    let paths: BTreeSet<_> = observed.keys().chain(state.manifest.files.keys()).collect();
    if !state.manifest.files.contains_key(old_path)
        || paths
            .into_iter()
            .any(|path| path != old_path && observed.get(path) != state.manifest.files.get(path))
    {
        return Err(CoreError::conflict(
            "Other project files also changed; resolve those conflicts first",
        ));
    }
    let baseline: super::Manifest = serde_json::from_slice(&state.manifest_bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid saved manifest: {e}")))?;
    let original =
        super::conflicts::baseline_file(&session.path, &state.manifest_bytes, &baseline, old_path)?;
    if shared && replacement_path != old_path && original.is_none() {
        return Err(CoreError::conflict(
            "The missing asset is shared and its original bytes are unavailable; restore the original before relinking",
        ));
    }
    let external = super::file_bytes(&session.path, old_path)?;
    if external.as_deref().map(super::hash).as_ref() != observed.get(old_path) {
        return Err(CoreError::conflict(
            "Asset changed while preparing the repair",
        ));
    }
    let doc = super::file_bytes(&session.path, "document.json")?
        .ok_or_else(|| CoreError::invalid("Missing document"))?;
    if Some(&super::hash(&doc)) != state.manifest.files.get("document.json") {
        return Err(CoreError::conflict(
            "Document changed while preparing the repair",
        ));
    }
    super::control::begin_commit()?;
    let conflicts = super::safe_path(&session.path, "conflicts")?;
    fs::create_dir_all(&conflicts)?;
    super::sync_dir(&session.path)?;
    let dir = super::safe_path(&session.path, &format!("conflicts/{}", Uuid::new_v4()))?;
    fs::create_dir(&dir)?;
    super::durable_write(&dir.join("old-manifest"), &state.manifest_bytes)?;
    super::durable_write(&dir.join("old-document.json"), &doc)?;
    super::durable_write(&dir.join("replacement"), replacement)?;
    if let Some(bytes) = &external {
        super::durable_write(&dir.join("external"), bytes)?;
    }
    if let Some(bytes) = &original {
        super::durable_write(&dir.join("old-asset"), bytes)?;
    }
    super::durable_write(
        &dir.join("metadata.json"),
        &super::json(&json!({
            "reason": "External asset missing or modified; relink preserves the observed version",
            "path": old_path,
            "expectedHash": state.manifest.files.get(old_path),
            "actualHash": observed.get(old_path),
            "externalCopy": if external.is_some() { Some("external") } else { None },
            "originalAssetAvailable": original.is_some(),
            "replacementHash": super::hash(replacement)
        }))?,
    )?;
    super::sync_dir(&dir)?;
    super::sync_dir(&session.path.join("conflicts"))?;
    Ok(Some((observed, original)))
}

impl ProjectSession {
    pub fn import_asset(&self, source_path: impl AsRef<Path>) -> Result<String> {
        self.change_asset(None, source_path.as_ref())
    }

    pub fn relink_asset(&self, asset_id: &str, source_path: impl AsRef<Path>) -> Result<String> {
        let id = Uuid::parse_str(asset_id).map_err(|_| CoreError::invalid("Invalid asset ID"))?;
        self.change_asset(Some(id), source_path.as_ref())
    }

    fn change_asset(&self, asset_id: Option<Uuid>, source: &Path) -> Result<String> {
        let (bytes, extension) = read_asset(source)?;
        let path = format!("assets/{}.{}", super::hash(&bytes), extension);
        let dirty = self.state()?.dirty;
        if dirty {
            self.save()?;
        }
        let mut state = self.state()?;
        if state.dirty {
            return Err(CoreError::conflict(
                "Project changed while preparing the asset",
            ));
        }
        if let Some(reason) = &state.read_only_reason {
            if asset_id.is_none() || reason != super::EXTERNAL_CHANGE_REASON {
                return Err(CoreError::unsupported(reason));
            }
        }
        let before = state.model()?;
        before.validate()?;
        if !before.editable()
            || before.project_id != state.manifest.project_id
            || before.revision != state.manifest.revision
            || super::fingerprint(&super::json(&state.document)?, &state.manifest.files)
                != state.manifest.snapshot_hash
        {
            return Err(CoreError::conflict(
                "Project document is not a verified editable baseline",
            ));
        }
        let old_path = asset_id
            .map(|id| {
                before
                    .assets
                    .iter()
                    .find(|asset| asset.id == id)
                    .map(|asset| asset.path.clone())
                    .ok_or_else(|| {
                        CoreError::new(
                            "MISSING_ASSET",
                            format!("Asset {id} is not in the document"),
                        )
                    })
            })
            .transpose()?;
        let shared = old_path.as_deref().is_some_and(|old| {
            before
                .assets
                .iter()
                .filter(|asset| asset.path == old)
                .count()
                > 1
        });
        let (observed, original) =
            match prepare_relink(self, &state, old_path.as_deref(), &path, shared, &bytes)? {
                Some((observed, original)) => (Some(observed), original),
                None => (None, None),
            };
        let mut document =
            serde_json::to_value(&before).map_err(|e| CoreError::invalid(e.to_string()))?;
        let assets = document
            .get_mut("assets")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| CoreError::invalid("Document assets are invalid"))?;
        let id = match asset_id {
            Some(id) => {
                let id_string = id.to_string();
                for item in assets.iter_mut() {
                    if item["id"].as_str() == Some(id_string.as_str()) {
                        item["path"] = json!(path);
                    }
                }
                id
            }
            None => {
                let id = Uuid::new_v4();
                assets.push(json!({"id": id, "path": path}));
                id
            }
        };
        let revision = before
            .revision
            .checked_add(1)
            .ok_or_else(|| CoreError::invalid("Revision overflow"))?;
        document["revision"] = json!(revision);
        let next: Document = serde_json::from_value(document.clone())
            .map_err(|e| CoreError::invalid(e.to_string()))?;
        next.validate()?;
        let mut files = observed.as_ref().unwrap_or(&state.manifest.files).clone();
        let mut deletions = BTreeSet::new();
        if observed.is_some() {
            if let Some(old_path) = &old_path {
                if old_path != &path {
                    if let Some(original) = &original {
                        files.insert(old_path.clone(), super::hash(original));
                    } else {
                        files.remove(old_path);
                        deletions.insert(old_path.clone());
                    }
                }
            }
        }
        files.insert(path.clone(), super::hash(&bytes));
        next.validate_files(&files)?;
        let transaction_id = Uuid::new_v4();
        let mut history = if state.read_only_reason.is_some()
            && state.manifest.files.contains_key("history/state.json")
        {
            serde_json::from_slice(&super::tracked_bytes(
                &self.path,
                &state.manifest,
                "history/state.json",
            )?)
            .map_err(|e| CoreError::invalid(format!("Invalid history: {e}")))?
        } else {
            state.history.clone()
        };
        history.undo.push(before.clone());
        if observed.is_some() && old_path.as_deref() != Some(path.as_str()) && original.is_none() {
            // The displaced original asset is unavailable, so its document cannot safely be restored.
            history.file_restore_barrier = Some(history.undo.len());
        }
        history.redo.clear();
        history.transaction_ids.insert(transaction_id);
        let transaction = json!({"transactionId": transaction_id, "kind": if asset_id.is_some() {"relinkAsset"} else {"importAsset"}, "before": before, "after": next});
        let mut changes = BTreeMap::from([
            (path.clone(), bytes),
            ("document.json".into(), super::json(&document)?),
            ("history/state.json".into(), super::json(&history)?),
            (
                format!("history/transactions/{transaction_id}.json"),
                super::json(&transaction)?,
            ),
        ]);
        if let (Some(old_path), Some(original)) = (&old_path, original) {
            if old_path != &path {
                changes.insert(old_path.clone(), original);
            }
        }
        let mut manifest = state.manifest.clone();
        manifest.revision = revision;
        let next_generation = state.next_generation()?;
        let committed = if let Some(observed) = &observed {
            super::commit_observed(
                &self.path,
                &state.manifest_bytes,
                &manifest,
                &changes,
                &deletions,
                Some(observed),
            )?
        } else {
            super::commit(&self.path, &state.manifest_bytes, &manifest, &changes)?
        };
        self.after_commit(&state.manifest_bytes, &committed, state.manifest.project_id);
        state.local_generation = next_generation;
        state.manifest =
            serde_json::from_slice(&committed).map_err(|e| CoreError::invalid(e.to_string()))?;
        state.manifest_bytes = committed;
        state.document = document;
        state.history = history;
        state.read_only_reason = None;
        serde_json::to_string(&json!({"id": id, "path": path}))
            .map_err(|e| CoreError::invalid(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{font_asset, font_extension};

    fn collection(count: u32) -> Vec<u8> {
        let font = font_test_data::AHEM;
        let offset = 12 + count as usize * 4;
        let mut bytes = b"ttcf\0\x01\0\0".to_vec();
        bytes.extend_from_slice(&count.to_be_bytes());
        for _ in 0..count {
            bytes.extend_from_slice(&(offset as u32).to_be_bytes());
        }
        bytes.extend_from_slice(font);
        let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
        for table in 0..tables {
            let pos = offset + 12 + table * 16 + 8;
            let old = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap());
            bytes[pos..pos + 4].copy_from_slice(&(old + offset as u32).to_be_bytes());
        }
        bytes
    }

    #[test]
    fn font_collection_validates_every_face_and_table_within_supported_limits() {
        assert_eq!(font_extension(&collection(64)).unwrap(), Some("ttc"));
        let mut invalid = collection(2);
        invalid[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            font_extension(&invalid).unwrap_err().code,
            "INVALID_DOCUMENT"
        );
        let mut invalid = font_test_data::AHEM.to_vec();
        invalid[20..24].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            font_extension(&invalid).unwrap_err().code,
            "INVALID_DOCUMENT"
        );
    }

    #[test]
    fn layout_font_asset_loading_rejects_untrusted_collection_count_before_fontdb() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("assets")).unwrap();
        let malicious = b"ttcf\0\x01\0\0\xff\xff\xff\xff";
        let path = format!("assets/{}.ttc", crate::storage::hash(malicious));
        std::fs::write(root.path().join(&path), malicious).unwrap();
        assert_eq!(
            font_asset(root.path(), &path).unwrap_err().code,
            "IMPORT_LIMIT_EXCEEDED"
        );
    }
}
