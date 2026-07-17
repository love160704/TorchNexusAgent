use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::task;
use torchnexus_storage_support::metadata::{BundleState, PackageStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSummary {
    pub bundle_id: String,
    pub created_ms: u64,
    pub status: PackageStatus,
    pub file_size: u64,
}

pub async fn list_sessions(save_dir: impl AsRef<Path>) -> anyhow::Result<Vec<BundleSummary>> {
    let state_dir = save_dir.as_ref().join("state");
    if !tokio::fs::try_exists(&state_dir)
        .await
        .with_context(|| format!("failed to inspect {}", state_dir.display()))?
    {
        return Ok(Vec::new());
    }

    let state_paths = task::spawn_blocking({
        let state_dir = state_dir.clone();
        move || collect_state_paths(&state_dir)
    })
    .await
    .with_context(|| "bundle index walk task failed")??;

    let mut summaries = Vec::new();
    for state_path in state_paths {
        let state_json = tokio::fs::read_to_string(&state_path)
            .await
            .with_context(|| format!("failed to read {}", state_path.display()))?;
        let state: BundleState = serde_json::from_str(&state_json)
            .with_context(|| format!("failed to parse {}", state_path.display()))?;
        summaries.push(BundleSummary {
            bundle_id: state.bundle_id,
            created_ms: state.created_ms,
            status: state.status,
            file_size: state.file_size,
        });
    }

    summaries.sort_by(|a, b| (a.created_ms, &a.bundle_id).cmp(&(b.created_ms, &b.bundle_id)));
    Ok(summaries)
}

fn collect_state_paths(state_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut state_paths = Vec::new();
    for entry in fs::read_dir(state_dir)
        .with_context(|| format!("failed to read {}", state_dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to scan {}", state_dir.display()))?;
        let path = entry.path();
        if entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?
            .is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("json")
        {
            state_paths.push(path);
        }
    }
    Ok(state_paths)
}

#[cfg(test)]
mod tests {
    use super::list_sessions;
    use std::fs;

    #[tokio::test]
    async fn missing_save_dir_returns_empty_vec() {
        let temp_dir = tempfile::tempdir().unwrap();
        let sessions = list_sessions(temp_dir.path().join("missing"))
            .await
            .unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn ignores_nested_state_json_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let state_dir = temp_dir.path().join("captures").join("state");
        let nested_dir = state_dir.join("nested");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::write(
            state_dir.join("top.json"),
            r#"{"bundle_id":"top","created_ms":1,"finalized_ms":2,"status":"queued","file_size":3,"record_count":4,"sha256":null}"#,
        )
        .unwrap();
        fs::write(
            nested_dir.join("nested.json"),
            r#"{"bundle_id":"nested","created_ms":0,"finalized_ms":1,"status":"pending","file_size":2,"record_count":3,"sha256":null}"#,
        )
        .unwrap();

        let sessions = list_sessions(temp_dir.path().join("captures"))
            .await
            .unwrap();

        assert_eq!(
            sessions
                .iter()
                .map(|bundle| bundle.bundle_id.as_str())
                .collect::<Vec<_>>(),
            vec!["top"]
        );
    }
}
