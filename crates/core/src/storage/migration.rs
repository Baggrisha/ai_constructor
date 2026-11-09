use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use serde_json::Value;
use uuid::Uuid;

use super::{control, hash, json, Manifest};
use crate::{model::Document, CoreError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Version {
    document: u64,
    manifest: u32,
}

const CURRENT: Version = Version {
    document: 2,
    manifest: 2,
};

struct Image {
    manifest: Manifest,
    files: BTreeMap<String, Vec<u8>>,
}

struct Migration {
    from: Version,
    to: Version,
    validate_source: fn(&Image) -> Result<()>,
    apply: fn(&mut Image) -> Result<()>,
    validate_target: fn(&Image) -> Result<()>,
}

// v2 only adds optional properties to the node model. The manifest version moves
// with the document so both files identify the same schema.
const MIGRATIONS: &[Migration] = &[Migration {
    from: Version {
        document: 1,
        manifest: 1,
    },
    to: Version {
        document: 2,
        manifest: 2,
    },
    validate_source: validate_schema_one_source,
    apply: apply_schema_one_to_two,
    validate_target: |_| Ok(()),
}];

fn version(document: &[u8], manifest: &Manifest) -> Result<Version> {
    let value: Value = serde_json::from_slice(document)
        .map_err(|e| CoreError::invalid(format!("Invalid document: {e}")))?;
    Ok(Version {
        document: value
            .get("schemaVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| CoreError::invalid("Missing document schemaVersion"))?,
        manifest: manifest.schema_version,
    })
}

fn support_base() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("AIC_APPLICATION_SUPPORT") {
        return Ok(value.into());
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| CoreError::io("Cannot determine Application Support directory"))?;
    #[cfg(target_os = "macos")]
    {
        Ok(PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".local/share")))
    }
}

// Resolve the existing ancestor before creating anything, so a misconfigured
// support directory inside the project cannot create even a partial backup there.
fn resolved_destination(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(CoreError::invalid(
            "Application Support path must be absolute",
        ));
    }
    let mut cursor = path;
    let mut missing = Vec::new();
    while !cursor.exists() {
        missing.push(
            cursor
                .file_name()
                .ok_or_else(|| CoreError::invalid("Invalid backup path"))?
                .to_owned(),
        );
        cursor = cursor
            .parent()
            .ok_or_else(|| CoreError::invalid("Invalid backup path"))?;
    }
    let mut resolved = fs::canonicalize(cursor)?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn private_dirs(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        cursor = cursor
            .parent()
            .ok_or_else(|| CoreError::invalid("Invalid backup path"))?;
    }
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    for created in missing.iter().rev() {
        super::sync_dir(created)?;
        super::sync_dir(
            created
                .parent()
                .ok_or_else(|| CoreError::invalid("Invalid backup path"))?,
        )?;
    }
    Ok(())
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::invalid("Invalid backup path"))?;
    private_dirs(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    for chunk in bytes.chunks(64 * 1024) {
        control::check()?;
        file.write_all(chunk)?;
    }
    file.sync_all()?;
    drop(file);
    if super::read_checked(fs::File::open(path)?)? != bytes {
        return Err(CoreError::io("Migration backup verification failed"));
    }
    super::sync_dir(parent)?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Backup records the exact migration inputs"
)]
fn backup(
    root: &Path,
    support: &Path,
    project_id: Uuid,
    before: Version,
    after: Version,
    marker: &[u8],
    original: &BTreeMap<String, Vec<u8>>,
    touched: &BTreeSet<String>,
) -> Result<()> {
    let project = fs::canonicalize(root)?;
    let base = resolved_destination(support)?;
    if base.starts_with(&project) {
        return Err(CoreError::invalid(
            "Migration backup must be outside the project",
        ));
    }
    private_dirs(&base)?;
    let base = fs::canonicalize(&base)?;
    if base.starts_with(&project) {
        return Err(CoreError::invalid(
            "Migration backup must be outside the project",
        ));
    }
    let mut dir = base;
    for component in ["AIConstructor", "Backups", &project_id.to_string()] {
        dir.push(component);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(CoreError::invalid("Unsafe migration backup directory"))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                private_dirs(&dir)?;
                super::sync_dir(
                    dir.parent()
                        .ok_or_else(|| CoreError::invalid("Invalid backup path"))?,
                )?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let id = Uuid::new_v4().to_string();
    let stage = dir.join(format!(".preparing-{id}"));
    private_dirs(&stage)?;
    private_write(&stage.join("files/project.json"), marker)?;
    let mut entries = BTreeMap::new();
    entries.insert("project.json".to_owned(), Some(hash(marker)));
    for path in touched {
        control::check()?;
        super::safe_path(root, path)?;
        let old = original.get(path);
        if let Some(bytes) = old {
            private_write(&stage.join("files").join(path), bytes)?;
        }
        entries.insert(path.clone(), old.map(|bytes| hash(bytes)));
    }
    let metadata = serde_json::json!({
        "backupVersion": 1, "projectId": project_id,
        "from": {"document": before.document, "manifest": before.manifest},
        "to": {"document": after.document, "manifest": after.manifest},
        "files": entries,
    });
    private_write(&stage.join("backup.json"), &json(&metadata)?)?;
    super::sync_dir(&stage)?;
    control::check()?;
    super::atomic::move_no_replace(&stage, &dir.join(id))?;
    super::sync_dir(&dir)?;
    Ok(())
}

// Compare against the shape of schema v1 separately from the current model:
// the typed model already refuses v1 documents, so a v1 snapshot must be checked
// structurally or it could be "migrated" without ever being read.
fn validate_schema_one_source(image: &Image) -> Result<()> {
    let document = image
        .files
        .get("document.json")
        .ok_or_else(|| CoreError::invalid("Schema v1 project has no document.json"))?;
    let value: Value = serde_json::from_slice(document)
        .map_err(|e| CoreError::invalid(format!("Invalid schema v1 document: {e}")))?;
    let missing = |field: &str| CoreError::invalid(format!("Schema v1 document lacks {field}"));
    let project_id = value
        .get("projectId")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| missing("projectId"))?;
    if project_id != image.manifest.project_id
        || value.get("revision").and_then(Value::as_u64) != Some(image.manifest.revision)
    {
        return Err(CoreError::invalid(
            "Schema v1 document does not match its manifest",
        ));
    }
    for field in [
        "pages",
        "nodes",
        "tokens",
        "variables",
        "actions",
        "animations",
    ] {
        if !value.get(field).is_some_and(Value::is_array) {
            return Err(missing(field));
        }
    }
    let nodes = value["nodes"].as_array().expect("checked nodes array");
    for node in nodes {
        let layout = node
            .get("layout")
            .and_then(Value::as_object)
            .ok_or_else(|| missing("node layout"))?;
        if ["x", "y", "w", "h"]
            .iter()
            .any(|key| !layout.get(*key).is_some_and(Value::is_number))
        {
            return Err(CoreError::invalid("Schema v1 node layout is invalid"));
        }
        let style = node
            .get("style")
            .ok_or_else(|| missing("node style"))?
            .as_object()
            .ok_or_else(|| missing("node style"))?;
        for unsupported in [
            "stroke",
            "shadows",
            "radius",
            "rotate",
            "textAlign",
            "maxLines",
            "overflow",
            "fit",
        ] {
            if style.contains_key(unsupported) {
                return Err(CoreError::unsupported(format!(
                    "Schema v1 style already contains {unsupported}"
                )));
            }
        }
        for unsupported in ["minW", "minH", "maxW", "maxH"] {
            if layout.contains_key(unsupported) {
                return Err(CoreError::unsupported(format!(
                    "Schema v1 layout already contains {unsupported}"
                )));
            }
        }
    }
    Ok(())
}

fn apply_schema_one_to_two(image: &mut Image) -> Result<()> {
    let document = image
        .files
        .get_mut("document.json")
        .ok_or_else(|| CoreError::invalid("Schema v1 project has no document.json"))?;
    let mut value: Value = serde_json::from_slice(document)
        .map_err(|e| CoreError::invalid(format!("Invalid schema v1 document: {e}")))?;
    value["schemaVersion"] = serde_json::json!(2);
    *document = json(&value)?;
    image.manifest.schema_version = CURRENT.manifest;
    image.manifest.snapshot_hash = super::fingerprint(document, &image.manifest.files);
    Ok(())
}

fn validate_image(
    image: &Image,
    expected: Version,
    original_id: Uuid,
    original_revision: u64,
    full: bool,
) -> Result<()> {
    let document = image
        .files
        .get("document.json")
        .ok_or_else(|| CoreError::invalid("Migration removed document.json"))?;
    if version(document, &image.manifest)? != expected || image.manifest.project_id != original_id {
        return Err(CoreError::invalid(
            "Migration produced an unexpected schema or project ID",
        ));
    }
    let value: Value = serde_json::from_slice(document)
        .map_err(|e| CoreError::invalid(format!("Invalid migrated document: {e}")))?;
    if value
        .get("projectId")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        != Some(original_id)
        || value.get("revision").and_then(Value::as_u64) != Some(image.manifest.revision)
        || image.manifest.revision < original_revision
    {
        return Err(CoreError::invalid(
            "Migration changed project identity or regressed revision",
        ));
    }
    if image.files.contains_key("project.json") {
        return Err(CoreError::invalid(
            "Migration cannot replace project.json directly",
        ));
    }
    if full {
        let model: Document = serde_json::from_value(value)
            .map_err(|e| CoreError::invalid(format!("Invalid migrated schema: {e}")))?;
        model.validate()?;
        let hashes = image
            .files
            .iter()
            .map(|(path, bytes)| (path.clone(), hash(bytes)))
            .collect();
        model.validate_files(&hashes)?;
    }
    Ok(())
}

// Caller holds the project lock continuously through validation, backup and journal publication.
fn migrate_with(
    root: &Path,
    marker: &[u8],
    manifest: &Manifest,
    document: &[u8],
    support: &Path,
    migrations: &[Migration],
) -> Result<Option<super::Loaded>> {
    migrate_to(
        root, marker, manifest, document, support, migrations, CURRENT,
    )
}

fn migrate_to(
    root: &Path,
    marker: &[u8],
    manifest: &Manifest,
    document: &[u8],
    support: &Path,
    migrations: &[Migration],
    target: Version,
) -> Result<Option<super::Loaded>> {
    let original_version = version(document, manifest)?;
    if original_version == target
        || !migrations
            .iter()
            .any(|migration| migration.from == original_version)
    {
        return Ok(None);
    }
    if super::manifest_has_unknown_fields(marker)? {
        return Err(CoreError::unsupported(
            "Cannot migrate a manifest with unknown fields",
        ));
    }
    let mut files = BTreeMap::new();
    for (path, expected) in &manifest.files {
        control::check()?;
        let bytes = super::file_bytes(root, path)?
            .ok_or_else(|| CoreError::conflict(format!("Missing {path}")))?;
        if hash(&bytes) != *expected {
            return Err(CoreError::conflict(format!("Changed {path}")));
        }
        files.insert(path.clone(), bytes);
    }
    if files.get("document.json").map(Vec::as_slice) != Some(document) {
        return Err(CoreError::conflict(
            "Document changed while preparing migration",
        ));
    }
    let original = files.clone();
    let mut image = Image {
        manifest: manifest.clone(),
        files,
    };
    let final_step = target == CURRENT;
    let mut at = original_version;
    loop {
        let mut candidates = migrations.iter().filter(|migration| migration.from == at);
        let step = candidates
            .next()
            .ok_or_else(|| CoreError::unsupported("No complete migration path for this schema"))?;
        if candidates.next().is_some()
            || step.to.document < at.document
            || step.to.manifest < at.manifest
            || step.to == at
            || step.to.document > target.document
            || step.to.manifest > target.manifest
        {
            return Err(CoreError::invalid(
                "Ambiguous or invalid migration version path",
            ));
        }
        validate_image(&image, at, manifest.project_id, manifest.revision, false)?;
        (step.validate_source)(&image)?;
        (step.apply)(&mut image)?;
        validate_image(
            &image,
            step.to,
            manifest.project_id,
            manifest.revision,
            final_step && step.to == target,
        )?;
        (step.validate_target)(&image)?;
        at = step.to;
        if at == target {
            break;
        }
    }
    let mut changes = BTreeMap::new();
    let mut deletions = BTreeSet::new();
    let mut touched = BTreeSet::new();
    for path in original.keys().chain(image.files.keys()) {
        let allowed = path == "document.json"
            || path == ".gitignore"
            || [
                "src/",
                "assets/",
                "chats/",
                "agent/",
                "history/",
                "checkpoints/",
            ]
            .iter()
            .any(|prefix| path.starts_with(prefix));
        if !allowed || path.contains('\\') {
            return Err(CoreError::invalid("Migration touched an untracked path"));
        }
        super::safe_path(root, path)?;
        if original.get(path) != image.files.get(path) {
            touched.insert(path.clone());
            if let Some(bytes) = image.files.get(path) {
                changes.insert(path.clone(), bytes.clone());
            } else {
                deletions.insert(path.clone());
            }
        }
    }
    if super::scan(root)? != manifest.files
        || super::file_bytes(root, "project.json")?.as_deref() != Some(marker)
    {
        return Err(CoreError::conflict(
            "Project changed while preparing migration",
        ));
    }
    backup(
        root,
        support,
        manifest.project_id,
        original_version,
        at,
        marker,
        &original,
        &touched,
    )?;
    let bytes = super::commit_locked(
        root,
        marker,
        &image.manifest,
        &changes,
        &deletions,
        None,
        None,
    )?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::invalid(format!("Invalid migrated manifest: {e}")))?;
    let document = image
        .files
        .remove("document.json")
        .expect("validated migrated document");
    Ok(Some(super::Loaded {
        manifest,
        manifest_bytes: bytes,
        document_bytes: document,
        read_only_reason: None,
    }))
}

pub(super) fn migrate_locked(
    root: &Path,
    marker: &[u8],
    manifest: &Manifest,
    document: &[u8],
) -> Result<Option<super::Loaded>> {
    let source = version(document, manifest)?;
    if !MIGRATIONS.iter().any(|migration| migration.from == source) {
        return Ok(None);
    }
    migrate_with(
        root,
        marker,
        manifest,
        document,
        &support_base()?,
        MIGRATIONS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // These synthetic records exercise the framework; they are not a supported
    // historical project format and are never registered in production.
    const SYNTHETIC: Version = Version {
        document: 0,
        manifest: 0,
    };

    fn source(image: &Image) -> Result<()> {
        let doc: Value = serde_json::from_slice(&image.files["document.json"]).unwrap();
        if doc["syntheticFixture"] != true {
            return Err(CoreError::invalid("Not the synthetic test format"));
        }
        Ok(())
    }

    fn convert(image: &mut Image) -> Result<()> {
        let mut doc: Value = serde_json::from_slice(&image.files["document.json"]).unwrap();
        doc["schemaVersion"] = json!(1);
        doc.as_object_mut().unwrap().remove("syntheticFixture");
        image
            .files
            .insert("document.json".into(), super::super::json(&doc)?);
        image
            .files
            .insert(".gitignore".into(), b"changed ignore\n".to_vec());
        image.files.remove("src/user/old.ts");
        image
            .files
            .insert("src/user/new.ts".into(), b"new file\n".to_vec());
        image.manifest.schema_version = 1;
        Ok(())
    }

    fn target(_image: &Image) -> Result<()> {
        Ok(())
    }

    fn invalid_target(image: &mut Image) -> Result<()> {
        convert(image)?;
        image
            .files
            .insert("document.json".into(), b"not a document".to_vec());
        Ok(())
    }

    const SYNTHETIC_TARGET: Version = Version {
        document: 1,
        manifest: 1,
    };
    const SYNTHETIC_MANIFEST_TARGET: Version = Version {
        document: 2,
        manifest: 1,
    };

    fn migrate_test(
        root: &Path,
        marker: &[u8],
        manifest: &Manifest,
        document: &[u8],
        support: &Path,
        migrations: &[Migration],
    ) -> Result<Option<super::super::Loaded>> {
        let target = if migrations
            .iter()
            .any(|migration| migration.to == SYNTHETIC_MANIFEST_TARGET)
        {
            SYNTHETIC_MANIFEST_TARGET
        } else {
            SYNTHETIC_TARGET
        };
        migrate_to(
            root, marker, manifest, document, support, migrations, target,
        )
    }

    fn migration(apply: fn(&mut Image) -> Result<()>) -> Migration {
        Migration {
            from: SYNTHETIC,
            to: SYNTHETIC_TARGET,
            validate_source: source,
            apply,
            validate_target: target,
        }
    }

    fn fixture(root: &Path) -> (Manifest, Vec<u8>, Vec<u8>) {
        drop(crate::create_project(root, "Synthetic").unwrap());
        let mut doc: Value =
            serde_json::from_slice(&fs::read(root.join("document.json")).unwrap()).unwrap();
        doc["schemaVersion"] = json!(0);
        doc["syntheticFixture"] = json!(true);
        let document = super::super::json(&doc).unwrap();
        fs::write(root.join("document.json"), &document).unwrap();
        fs::write(root.join("src/user/old.ts"), b"old file\n").unwrap();
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
        manifest.schema_version = 0;
        manifest.files = super::super::scan(root).unwrap();
        manifest.snapshot_hash = super::super::fingerprint(&document, &manifest.files);
        let marker = super::super::json(&manifest).unwrap();
        fs::write(root.join("project.json"), &marker).unwrap();
        (manifest, marker, document)
    }

    #[test]
    fn unknown_format_is_not_migrated_or_backed_up() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let (manifest, marker, doc) = fixture(&root);
        let support = temp.path().join("support");
        let _lock = super::super::lock(&root).unwrap();
        assert!(migrate_test(&root, &marker, &manifest, &doc, &support, &[])
            .unwrap()
            .is_none());
        assert!(!support.exists());
        assert_eq!(fs::read(root.join("project.json")).unwrap(), marker);
        assert_eq!(fs::read(root.join("document.json")).unwrap(), doc);
    }

    #[test]
    fn backup_contains_every_changed_or_deleted_original_and_marks_new_files_absent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let support = temp.path().join("support");
        let (manifest, marker, doc) = fixture(&root);
        let ignore = fs::read(root.join(".gitignore")).unwrap();
        let _lock = super::super::lock(&root).unwrap();
        let result = migrate_test(
            &root,
            &marker,
            &manifest,
            &doc,
            &support,
            &[migration(convert)],
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.manifest.schema_version, 1);
        let parent = support
            .join("AIConstructor/Backups")
            .join(manifest.project_id.to_string());
        let backups = fs::read_dir(parent)
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(backups.len(), 1);
        let backup = backups[0].path();
        assert_eq!(fs::read(backup.join("files/project.json")).unwrap(), marker);
        assert_eq!(fs::read(backup.join("files/document.json")).unwrap(), doc);
        assert_eq!(fs::read(backup.join("files/.gitignore")).unwrap(), ignore);
        assert_eq!(
            fs::read(backup.join("files/src/user/old.ts")).unwrap(),
            b"old file\n"
        );
        let metadata: Value =
            serde_json::from_slice(&fs::read(backup.join("backup.json")).unwrap()).unwrap();
        assert!(metadata["files"]
            .as_object()
            .unwrap()
            .contains_key("src/user/new.ts"));
        assert!(metadata["files"]["src/user/new.ts"].is_null());
        assert_eq!(metadata["files"]["document.json"], hash(&doc));
        assert!(!root.join("src/user/old.ts").exists());
        assert_eq!(
            fs::read(root.join("src/user/new.ts")).unwrap(),
            b"new file\n"
        );
    }

    #[test]
    fn manifest_only_migration_is_journalled_and_increments_generation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        drop(crate::create_project(&root, "Synthetic manifest").unwrap());
        let doc = fs::read(root.join("document.json")).unwrap();
        let mut manifest: Manifest =
            serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
        manifest.schema_version = 0;
        let marker = super::super::json(&manifest).unwrap();
        fs::write(root.join("project.json"), &marker).unwrap();
        let step = Migration {
            from: Version {
                document: 2,
                manifest: 0,
            },
            to: SYNTHETIC_MANIFEST_TARGET,
            validate_source: target,
            apply: |image| {
                image.manifest.schema_version = SYNTHETIC_MANIFEST_TARGET.manifest;
                Ok(())
            },
            validate_target: target,
        };
        let _lock = super::super::lock(&root).unwrap();
        let updated = migrate_test(
            &root,
            &marker,
            &manifest,
            &doc,
            &temp.path().join("support"),
            &[step],
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated.manifest.schema_version, 1);
        assert_eq!(
            updated.manifest.storage_generation,
            manifest.storage_generation + 1
        );
        assert_eq!(updated.document_bytes, doc);
        let disk: Manifest =
            serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
        assert_eq!(disk.schema_version, 1);
    }

    #[test]
    fn invalid_target_or_backup_inside_project_never_publishes() {
        for invalid_schema in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("project");
            let (manifest, marker, doc) = fixture(&root);
            let support = if invalid_schema {
                temp.path().join("support")
            } else {
                root.join("bad-support")
            };
            let step = migration(if invalid_schema {
                invalid_target
            } else {
                convert
            });
            let _lock = super::super::lock(&root).unwrap();
            assert!(migrate_test(&root, &marker, &manifest, &doc, &support, &[step]).is_err());
            assert!(!support.exists());
            assert_eq!(fs::read(root.join("project.json")).unwrap(), marker);
            assert_eq!(fs::read(root.join("document.json")).unwrap(), doc);
            assert_eq!(
                fs::read(root.join("src/user/old.ts")).unwrap(),
                b"old file\n"
            );
            assert!(!root.join("src/user/new.ts").exists());
        }
    }

    #[test]
    fn migration_rejects_unrepresentable_manifest_fields_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let support = temp.path().join("support");
        let (manifest, marker, doc) = fixture(&root);
        let mut value: Value = serde_json::from_slice(&marker).unwrap();
        value["futureSecret"] = json!({"preserve": true});
        let extended = super::super::json(&value).unwrap();
        fs::write(root.join("project.json"), &extended).unwrap();
        let _lock = super::super::lock(&root).unwrap();
        let error = migrate_test(
            &root,
            &extended,
            &manifest,
            &doc,
            &support,
            &[migration(convert)],
        )
        .unwrap_err();
        assert_eq!(error.code, "UNSUPPORTED_FEATURE");
        assert!(!support.exists());
        assert_eq!(fs::read(root.join("project.json")).unwrap(), extended);
        assert_eq!(fs::read(root.join("document.json")).unwrap(), doc);
    }

    #[test]
    fn interrupted_synthetic_migration_recovers_whole_old_or_new_snapshot() {
        for point in [0, 1, 4, 5] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("project");
            let support = temp.path().join("support");
            let (manifest, marker, doc) = fixture(&root);
            {
                let _lock = super::super::lock(&root).unwrap();
                super::super::INTERRUPT_AFTER.with(|hook| hook.set(Some(point)));
                assert!(migrate_test(
                    &root,
                    &marker,
                    &manifest,
                    &doc,
                    &support,
                    &[migration(convert)]
                )
                .is_err());
                assert!(super::super::recover(&root).unwrap().is_none());
            }
            let current: Manifest =
                serde_json::from_slice(&fs::read(root.join("project.json")).unwrap()).unwrap();
            let current_doc: Value =
                serde_json::from_slice(&fs::read(root.join("document.json")).unwrap()).unwrap();
            assert_eq!(
                current.schema_version as u64,
                current_doc["schemaVersion"].as_u64().unwrap()
            );
            assert_eq!(super::super::scan(&root).unwrap(), current.files);
            if current.schema_version == 0 {
                assert_eq!(
                    fs::read(root.join("src/user/old.ts")).unwrap(),
                    b"old file\n"
                );
                assert!(!root.join("src/user/new.ts").exists());
            } else {
                assert_eq!(current.schema_version, 1);
                assert!(!root.join("src/user/old.ts").exists());
                assert_eq!(
                    fs::read(root.join("src/user/new.ts")).unwrap(),
                    b"new file\n"
                );
            }
        }
    }
}
