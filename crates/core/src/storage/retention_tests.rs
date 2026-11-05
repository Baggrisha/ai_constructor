use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::{create_project, open_project, ProjectSession};

fn save(session: &ProjectSession, name: &str) {
    session.rename(name).unwrap();
    session.save().unwrap();
}

fn retained(root: &Path) -> PathBuf {
    root.join(".aic-local/retained")
}

fn completed(root: &Path) -> Vec<(u64, PathBuf)> {
    let mut result: Vec<_> = fs::read_dir(retained(root))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(path.join("completed.json")).ok()?;
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            Some((value["generation"].as_u64().unwrap(), path))
        })
        .collect();
    result.sort_by_key(|(generation, _)| *generation);
    result
}

fn tree(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut result: Vec<_> = walkdir::WalkDir::new(path)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let relative = entry
                .path()
                .strip_prefix(path)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let bytes = if entry.file_type().is_file() {
                fs::read(entry.path()).unwrap()
            } else {
                Vec::new()
            };
            (relative, bytes)
        })
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

#[test]
fn unlimited_is_default_and_opening_or_changing_policy_does_not_prune() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    for name in ["one", "two", "three"] {
        save(&session, name);
    }
    assert_eq!(completed(&path).len(), 3);
    let before = tree(&retained(&path));
    session.set_retention_limit(Some(0));
    assert_eq!(tree(&retained(&path)), before);
    drop(session);
    let reopened = open_project(&path).unwrap();
    assert_eq!(tree(&retained(&path)), before);
    save(&reopened, "four");
    assert_eq!(completed(&path).len(), 4);
}

#[test]
fn finite_limit_keeps_newest_completed_saves_per_project() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    for name in ["one", "two", "three"] {
        save(&session, name);
    }
    session.set_retention_limit(Some(2));
    save(&session, "four");
    assert_eq!(
        completed(&path).iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![3, 4]
    );
    save(&session, "five");
    assert_eq!(
        completed(&path).iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        vec![4, 5]
    );
    session.set_retention_limit(Some(0));
    save(&session, "six");
    assert!(completed(&path).is_empty());
}

#[test]
fn a_save_with_multiple_files_counts_as_one_version() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    save(&session, "one");
    let snapshot: Value = serde_json::from_str(&session.snapshot_json().unwrap()).unwrap();
    let root = &snapshot["document"]["pages"][0]["rootId"];
    let transaction = json!({
        "transactionId": Uuid::new_v4(),
        "baseRevision": snapshot["document"]["revision"],
        "baseFingerprint": snapshot["fingerprint"],
        "origin": "user", "scopeNodeIds": [root],
        "operations": [{"type": "setProperty", "nodeId": root, "path": "/name", "value": "New frame"}]
    });
    session.set_retention_limit(Some(1));
    session.apply_transaction(&transaction.to_string()).unwrap();
    session.save().unwrap();
    let records = completed(&path);
    assert_eq!(records.len(), 1);
    let journal: Value =
        serde_json::from_slice(&fs::read(records[0].1.join("journal.json")).unwrap()).unwrap();
    assert!(journal["entries"].as_array().unwrap().len() >= 2);
}

#[test]
fn active_legacy_and_unknown_entries_are_never_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    for name in ["one", "two", "three"] {
        save(&session, name);
    }
    let records = completed(&path);
    let active = records[0].1.clone();
    let tx = path
        .join(".aic-local/transactions")
        .join(active.file_name().unwrap());
    fs::create_dir(&tx).unwrap(); // An unfinished, unpublished transaction.
    fs::write(tx.join("foreign"), b"active").unwrap();
    let legacy = records[1].1.clone();
    fs::remove_file(legacy.join("completed.json")).unwrap(); // A pre-receipt retained record.
    let changed = records[2].1.clone();
    fs::write(changed.join("foreign"), b"unknown").unwrap();
    let unknown = retained(&path).join(Uuid::new_v4().to_string());
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("private"), b"do not delete").unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("data"), b"outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, retained(&path).join("foreign-link")).unwrap();
    let active_before = tree(&active);
    let legacy_before = tree(&legacy);
    let changed_before = tree(&changed);
    session.set_retention_limit(Some(0));
    save(&session, "four");
    assert_eq!(tree(&active), active_before);
    assert_eq!(tree(&legacy), legacy_before);
    assert_eq!(tree(&changed), changed_before);
    assert_eq!(fs::read(tx.join("foreign")).unwrap(), b"active");
    assert_eq!(fs::read(unknown.join("private")).unwrap(), b"do not delete");
    assert_eq!(fs::read(outside.join("data")).unwrap(), b"outside");
}

#[test]
fn conflict_archive_blocks_cleanup_without_modifying_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    save(&session, "one");
    let original = completed(&path)[0].1.clone();
    let conflict = path.join("conflicts/foreign");
    fs::create_dir_all(&conflict).unwrap();
    fs::write(conflict.join("journal"), b"not ours").unwrap();
    let before = tree(&conflict);
    session.set_retention_limit(Some(0));
    save(&session, "two");
    assert!(original.exists());
    assert_eq!(tree(&conflict), before);
}

#[test]
fn late_external_write_makes_record_ineligible() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    let mut external = fs::OpenOptions::new()
        .write(true)
        .open(path.join("project.json"))
        .unwrap();
    save(&session, "one");
    let record = completed(&path)[0].1.clone();
    external.set_len(0).unwrap();
    external.write_all(b"late external update").unwrap();
    external.sync_all().unwrap();
    session.set_retention_limit(Some(0));
    save(&session, "two");
    assert!(tree(&record)
        .iter()
        .any(|(_, data)| data == b"late external update"));
}

#[test]
fn unknown_file_in_an_interrupted_tombstone_is_not_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("project");
    let session = create_project(&path, "Example").unwrap();
    save(&session, "one");
    session.set_retention_limit(Some(0));
    super::retention::INTERRUPT.with(|p| p.set(Some(0)));
    save(&session, "two");
    let tombstone = fs::read_dir(retained(&path))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".pruning-")
        })
        .unwrap();
    fs::write(tombstone.join("foreign"), b"keep").unwrap();
    let before = tree(&tombstone);
    save(&session, "three");
    assert_eq!(tree(&tombstone), before);
}

#[cfg(unix)]
#[test]
fn killed_during_retirement_recovers_only_after_another_save() {
    const CHILD: &str = "AIC_RETENTION_CRASH_PROJECT";
    if let Some(path) = std::env::var_os(CHILD) {
        let session = open_project(path).unwrap();
        session.set_retention_limit(Some(0));
        let point = std::env::var("AIC_RETENTION_CRASH_POINT")
            .unwrap()
            .parse()
            .unwrap();
        super::retention::KILL.with(|p| p.set(Some(point)));
        save(&session, "child");
        panic!("retention kill point was not reached");
    }
    use std::os::unix::process::ExitStatusExt;
    for point in [0, 2] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("project");
        let session = create_project(&path, "Example").unwrap();
        save(&session, "before");
        drop(session);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::retention_tests::killed_during_retirement_recovers_only_after_another_save"])
            .env(CHILD, &path)
            .env("AIC_RETENTION_CRASH_POINT", point.to_string())
            .status().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let before = tree(&retained(&path));
        let reopened = open_project(&path).unwrap();
        assert_eq!(tree(&retained(&path)), before);
        let snapshot: Value = serde_json::from_str(&reopened.snapshot_json().unwrap()).unwrap();
        assert_eq!(snapshot["name"], "child");
        reopened.set_retention_limit(Some(0));
        save(&reopened, "after");
        assert!(fs::read_dir(retained(&path)).unwrap().next().is_none());
    }
}
