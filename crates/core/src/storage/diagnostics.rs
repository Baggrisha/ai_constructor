use std::{collections::BTreeSet, fs};

use serde_json::json;

use crate::{CoreError, ProjectSession, Result};

pub(crate) fn capture_external(root: &std::path::Path, state: &crate::State) -> Result<()> {
    let _lock = super::lock(root)?;
    let observed = super::scan(root)?;
    let marker = super::file_bytes(root, "project.json")?;
    super::conflicts::archive_failed(root, state, &observed, marker.as_deref())
}

impl ProjectSession {
    pub fn diagnostics_json(&self) -> Result<String> {
        let state = self.state()?;
        let _lock = super::lock(&self.path)?;
        let observed = super::scan(&self.path)?;
        let current_manifest = super::file_bytes(&self.path, "project.json")?;
        let manifest_changed = current_manifest.as_deref() != Some(state.manifest_bytes.as_slice());
        let base: super::Manifest = serde_json::from_slice(&state.manifest_bytes)
            .map_err(|e| CoreError::invalid(format!("Invalid saved manifest: {e}")))?;
        let baseline_available =
            super::conflicts::baseline_available(&self.path, &state.manifest_bytes, &base)?;
        let blocked_generated = super::conflicts::blocked_generated(&self.path, &base)?;
        let (disk_token, external_conflicts) = super::conflicts::external_diagnostics(
            &self.path,
            &state,
            &observed,
            current_manifest.as_deref(),
        )?;
        let recovery_conflicts = super::recovery_choice::diagnostics(
            &self.path,
            &observed,
            current_manifest.as_deref(),
            &disk_token,
        )?;
        let mut files = Vec::new();
        let paths: BTreeSet<_> = state.manifest.files.keys().chain(observed.keys()).collect();
        for path in paths {
            let expected = state.manifest.files.get(path);
            let actual = observed.get(path);
            if expected == actual {
                continue;
            }
            let kind = match (expected, actual) {
                (None, Some(_)) => "added",
                (Some(_), None) => "missing",
                _ => "modified",
            };
            files.push(
                json!({"path": path, "kind": kind, "expectedHash": expected, "actualHash": actual}),
            );
        }
        let mut conflicts = Vec::new();
        let root = self.path.join("conflicts");
        if let Ok(metadata) = fs::symlink_metadata(&root) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CoreError::invalid(
                    "Conflict directory is not a real directory",
                ));
            }
            let mut entries = fs::read_dir(&root)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let relative = format!("conflicts/{id}/metadata.json");
                let metadata = match super::file_bytes(&self.path, &relative)? {
                    Some(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).ok(),
                    None => None,
                };
                let reason = metadata
                    .as_ref()
                    .and_then(|value| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Conflict metadata unavailable");
                conflicts.push(json!({"id": id, "reason": reason}));
            }
        }
        for external in external_conflicts.into_iter().chain(recovery_conflicts) {
            let id = external.get("id").and_then(serde_json::Value::as_str);
            if let Some(index) = conflicts
                .iter()
                .position(|entry| entry.get("id").and_then(serde_json::Value::as_str) == id)
            {
                conflicts[index] = external;
            } else {
                conflicts.push(external);
            }
        }
        let read_only = state.read_only_reason.is_some() || manifest_changed || !files.is_empty();
        serde_json::to_string(&json!({
            "schemaVersion": 1,
            "baseManifestHash": super::hash(&state.manifest_bytes),
            "localGeneration": state.local_generation,
            "readOnly": read_only,
            "readOnlyReason": state.read_only_reason,
            "manifestChanged": manifest_changed,
            "files": files,
            "conflicts": conflicts,
            "observedDiskToken": disk_token,
            "blockedGenerated": blocked_generated,
            "baselineAvailable": baseline_available,
        }))
        .map_err(|e| CoreError::invalid(e.to_string()))
    }
}
