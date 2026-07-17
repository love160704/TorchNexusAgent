use crate::client::{PackageUploader, UploadBundle, UploadFailure};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use tokio::task;
use torchnexus_core::config::UploadConfig;
use torchnexus_storage_support::{
    metadata::{BundleState, PackageStatus},
    recorder::write_bundle_state,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInfo {
    pub bundle_id: String,
    pub package_path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadSummary {
    pub scanned: u64,
    pub uploaded: u64,
    pub failed: u64,
    pub moved_to_failed: u64,
}

#[derive(Debug, Clone)]
pub struct UploadQueue<U> {
    capture_root: PathBuf,
    config: UploadConfig,
    uploader: U,
}

impl<U> UploadQueue<U> {
    pub fn new(capture_root: PathBuf, config: UploadConfig, uploader: U) -> Self {
        Self {
            capture_root,
            config,
            uploader,
        }
    }

    pub fn config(&self) -> &UploadConfig {
        &self.config
    }
}

impl<U> UploadQueue<U>
where
    U: PackageUploader,
{
    pub async fn enqueue_bundle(&self, bundle_path: &Path) -> anyhow::Result<PackageInfo> {
        let info = package_info(bundle_path.to_path_buf()).await?;
        self.update_bundle_state(&info.bundle_id, |state| {
            state.file_size = info.size_bytes;
            state.sha256 = Some(info.sha256.clone());
            state.status = PackageStatus::Queued;
        })
        .await?;
        Ok(info)
    }

    pub async fn upload_pending_once(&self) -> anyhow::Result<UploadSummary> {
        let pending_dir = self.capture_root.join("pending");
        let uploading_dir = self.capture_root.join("uploading");
        let uploaded_dir = self.capture_root.join("uploaded");
        let failed_dir = self.capture_root.join("failed");

        for dir in [&pending_dir, &uploading_dir, &uploaded_dir, &failed_dir] {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("failed to create capture dir {}", dir.display()))?;
        }
        task::spawn_blocking({
            let pending_dir = pending_dir.clone();
            move || migrate_legacy_packages(&pending_dir)
        })
        .await
        .with_context(|| "legacy package migration task failed")??;

        let mut summary = UploadSummary::default();
        let mut entries = tokio::fs::read_dir(&pending_dir)
            .await
            .with_context(|| format!("failed to read pending dir {}", pending_dir.display()))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("failed to scan pending dir {}", pending_dir.display()))?
        {
            let path = entry.path();
            if !is_queued_package(&path) {
                continue;
            }

            summary.scanned += 1;
            tracing::info!(package = %path.display(), "开始上传采集包");
            let uploading_path = uploading_dir.join(path.file_name().expect("file name"));
            move_file(&path, &uploading_path).await?;
            move_sidecar_if_exists(&path, &uploading_path).await?;

            let info = package_info(uploading_path.clone()).await?;
            self.update_bundle_metadata(&info).await?;

            match self
                .uploader
                .upload_bundle(UploadBundle {
                    bundle_path: &info.package_path,
                    sha256: &info.sha256,
                })
                .await
            {
                Ok(_) => {
                    let uploaded_path =
                        uploaded_dir.join(uploading_path.file_name().expect("file name"));
                    move_file(&uploading_path, &uploaded_path).await?;
                    remove_if_exists(attempts_path(&uploading_path)).await?;
                    self.update_bundle_status(
                        bundle_id_from_path(&uploaded_path),
                        PackageStatus::Uploaded,
                    )
                    .await?;
                    summary.uploaded += 1;
                    tracing::info!(bundle_id = %info.bundle_id, "采集包上传成功");
                }
                Err(error) => {
                    summary.failed += 1;
                    tracing::warn!(bundle_id = %info.bundle_id, error = %error, "采集包上传失败");
                    if error.downcast_ref::<UploadFailure>().is_some_and(|failure| !failure.retryable) {
                        let failed_path = failed_dir.join(uploading_path.file_name().expect("file name"));
                        move_file(&uploading_path, &failed_path).await?;
                        remove_if_exists(attempts_path(&uploading_path)).await?;
                        write_failure(&failed_path, error.downcast_ref::<UploadFailure>(), &error).await?;
                        self.update_bundle_status(bundle_id_from_path(&failed_path), PackageStatus::Failed).await?;
                        summary.moved_to_failed += 1;
                        continue;
                    }
                    let attempts_path = attempts_path(&uploading_path);
                    let attempts = increment_attempts(&attempts_path).await?;
                    if attempts > self.config.retry.max_attempts {
                        let failed_path =
                            failed_dir.join(uploading_path.file_name().expect("file name"));
                        move_file(&uploading_path, &failed_path).await?;
                        remove_if_exists(attempts_path).await?;
                        write_failure(&failed_path, error.downcast_ref::<UploadFailure>(), &error).await?;
                        self.update_bundle_status(
                            bundle_id_from_path(&failed_path),
                            PackageStatus::Failed,
                        )
                        .await?;
                        summary.moved_to_failed += 1;
                    } else {
                        let pending_path =
                            pending_dir.join(uploading_path.file_name().expect("file name"));
                        move_file(&uploading_path, &pending_path).await?;
                        move_sidecar_if_exists(&uploading_path, &pending_path).await?;
                    }
                }
            }
        }

        Ok(summary)
    }

    async fn update_bundle_status(
        &self,
        bundle_id: &str,
        status: PackageStatus,
    ) -> anyhow::Result<()> {
        self.update_bundle_state(bundle_id, |state| {
            state.status = status;
        })
        .await
    }

    async fn update_bundle_metadata(&self, info: &PackageInfo) -> anyhow::Result<()> {
        self.update_bundle_state(&info.bundle_id, |state| {
            state.file_size = info.size_bytes;
            state.sha256 = Some(info.sha256.clone());
        })
        .await
    }

    async fn update_bundle_state<F>(&self, bundle_id: &str, update: F) -> anyhow::Result<()>
    where
        F: FnOnce(&mut BundleState),
    {
        let state_path = self
            .capture_root
            .join("state")
            .join(format!("{bundle_id}.json"));
        let state_json = tokio::fs::read_to_string(&state_path)
            .await
            .with_context(|| format!("failed to read bundle state {}", state_path.display()))?;
        let mut state: BundleState = serde_json::from_str(&state_json)
            .with_context(|| format!("failed to parse bundle state {}", state_path.display()))?;
        update(&mut state);
        write_bundle_state(&state_path, &state)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct AttemptsSidecar {
    attempts: u32,
}

fn is_queued_package(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    name.ends_with(".tlc.zst")
}

fn bundle_id_from_path(package_path: &Path) -> &str {
    package_path.file_name().and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".tlc.zst").or_else(|| name.strip_suffix(".tlc")))
        .expect("queued package has a valid file name")
}

fn attempts_path(package_path: &Path) -> PathBuf {
    package_path.with_file_name(format!(
        "{}.attempts.json",
        bundle_id_from_path(package_path)
    ))
}

async fn increment_attempts(path: &Path) -> anyhow::Result<u32> {
    let attempts = match tokio::fs::read_to_string(path).await {
        Ok(text) => {
            serde_json::from_str::<AttemptsSidecar>(&text)
                .with_context(|| format!("failed to parse attempts sidecar {}", path.display()))?
                .attempts
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read attempts sidecar {}", path.display()));
        }
    } + 1;

    let text = serde_json::to_string(&AttemptsSidecar { attempts })
        .with_context(|| "failed to serialize attempts sidecar")?;
    tokio::fs::write(path, text)
        .await
        .with_context(|| format!("failed to write attempts sidecar {}", path.display()))?;
    Ok(attempts)
}

async fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    remove_if_exists(to.to_path_buf()).await?;
    tokio::fs::rename(from, to).await.with_context(|| {
        format!(
            "failed to move package from {} to {}",
            from.display(),
            to.display()
        )
    })
}

async fn move_sidecar_if_exists(from_bundle: &Path, to_bundle: &Path) -> anyhow::Result<()> {
    let from_sidecar = attempts_path(from_bundle);
    let to_sidecar = attempts_path(to_bundle);
    if !tokio::fs::try_exists(&from_sidecar)
        .await
        .with_context(|| format!("failed to inspect {}", from_sidecar.display()))?
    {
        return Ok(());
    }

    remove_if_exists(to_sidecar.clone()).await?;
    tokio::fs::rename(&from_sidecar, &to_sidecar)
        .await
        .with_context(|| {
            format!(
                "failed to move attempts sidecar from {} to {}",
                from_sidecar.display(),
                to_sidecar.display()
            )
        })
}

async fn remove_if_exists(path: PathBuf) -> anyhow::Result<()> {
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove file {}", path.display()))
        }
    }
}

async fn package_info(package_path: PathBuf) -> anyhow::Result<PackageInfo> {
    task::spawn_blocking(move || package_info_blocking(package_path))
        .await
        .with_context(|| "package info task failed")?
}

fn package_info_blocking(package_path: PathBuf) -> anyhow::Result<PackageInfo> {
    let bundle_id = bundle_id_from_path(&package_path).to_string();
    let file = File::open(&package_path)
        .with_context(|| format!("failed to open package {}", package_path.display()))?;
    let mut file = zstd::stream::read::Decoder::new(file)
        .with_context(|| format!("failed to decompress package {}", package_path.display()))?;
    let mut hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];

    loop {
        let bytes_read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read package {}", package_path.display()))?;
        if bytes_read == 0 {
            break;
        }
        size_bytes += bytes_read as u64;
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(PackageInfo {
        bundle_id,
        package_path,
        sha256: hex::encode(hasher.finalize()),
        size_bytes,
    })
}

fn migrate_legacy_packages(pending_dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(pending_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("tlc") {
            continue;
        }
        let compressed = path.with_extension("tlc.zst");
        let temporary = path.with_extension("tlc.zst.tmp");
        zstd::stream::copy_encode(File::open(&path)?, File::create(&temporary)?, 0)?;
        std::fs::rename(&temporary, &compressed)?;
        std::fs::remove_file(path)?;
    }
    Ok(())
}

async fn write_failure(path: &Path, failure: Option<&UploadFailure>, error: &anyhow::Error) -> anyhow::Result<()> {
    #[derive(Serialize)]
    struct FailureRecord<'a> { status: Option<u16>, title: Option<&'a str>, detail: String }
    let record = FailureRecord {
        status: failure.and_then(|value| value.status),
        title: failure.and_then(|value| value.title.as_deref()),
        detail: failure.map(|value| value.detail.clone()).unwrap_or_else(|| error.to_string()),
    };
    let error_path = path.with_file_name(format!("{}.error.json", bundle_id_from_path(path)));
    tokio::fs::write(error_path, serde_json::to_vec_pretty(&record)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{AlwaysFailUploader, AlwaysSucceedUploader, PackageUploader, UploadBundle};
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    use torchnexus_core::config::{BasicAuthConfig, RetryConfig, UploadConfig};

    fn upload_config(max_attempts: u32) -> UploadConfig {
        UploadConfig {
            enabled: true,
            endpoint: "http://127.0.0.1/upload".to_string(),
            basic_auth: BasicAuthConfig {
                username: "agent".to_string(),
                password: "secret".to_string(),
            },
            auto_package_on_disconnect: true,
            upload_interval_seconds: 60,
            retry: RetryConfig {
                max_attempts,
                base_delay_seconds: 1,
            },
        }
    }

    fn bundle_name() -> &'static str {
        "00000000000000000000000000000001.tlc"
    }

    fn attempts_sidecar_name() -> &'static str {
        "00000000000000000000000000000001.attempts.json"
    }

    fn write_bundle_state_file(capture_root: &Path, bundle_id: &str) {
        let state_dir = capture_root.join("state");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        fs::write(
            state_dir.join(format!("{bundle_id}.json")),
            format!(
                r#"{{
  "bundle_id": "{bundle_id}",
  "created_ms": 1,
  "finalized_ms": 2,
  "status": "queued",
  "file_size": 3,
  "record_count": 4,
  "sha256": null
}}"#
            ),
        )
        .expect("bundle state should be written");
    }

    fn write_pending_bundle(capture_root: &Path) -> PathBuf {
        let pending_dir = capture_root.join("pending");
        fs::create_dir_all(&pending_dir).expect("pending dir should be created");
        let bundle_path = pending_dir.join(bundle_name());
        fs::write(&bundle_path, b"TLC1 test bundle bytes").expect("bundle should be written");
        write_bundle_state_file(
            capture_root,
            bundle_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap(),
        );
        bundle_path
    }

    #[tokio::test]
    async fn successful_upload_moves_tlc_to_uploaded() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let capture_root = temp_dir.path().join("captures");
        let original_path = write_pending_bundle(&capture_root);
        let uploaded_path = capture_root.join("uploaded").join(bundle_name());

        let queue = UploadQueue::new(
            capture_root.clone(),
            upload_config(5),
            AlwaysSucceedUploader,
        );
        let summary = queue
            .upload_pending_once()
            .await
            .expect("upload should succeed");

        assert_eq!(summary.uploaded, 1);
        assert!(!original_path.exists());
        assert!(uploaded_path.exists());
        let state_json = fs::read_to_string(
            capture_root
                .join("state")
                .join("00000000000000000000000000000001.json"),
        )
        .expect("state should be readable");
        let state: BundleState = serde_json::from_str(&state_json).expect("state should parse");
        assert_eq!(state.status, PackageStatus::Uploaded);
    }

    #[tokio::test]
    async fn failed_upload_keeps_tlc_before_max_attempts() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let capture_root = temp_dir.path().join("captures");
        let original_path = write_pending_bundle(&capture_root);
        let sidecar_path = capture_root.join("pending").join(attempts_sidecar_name());

        let queue = UploadQueue::new(capture_root, upload_config(5), AlwaysFailUploader);
        let summary = queue
            .upload_pending_once()
            .await
            .expect("failed upload should be summarized");

        assert_eq!(summary.failed, 1);
        assert!(original_path.exists());
        assert!(sidecar_path.exists());
        assert_eq!(
            fs::read_to_string(sidecar_path).expect("sidecar should be readable"),
            r#"{"attempts":1}"#
        );
    }

    #[tokio::test]
    async fn failed_upload_moves_tlc_after_max_attempts() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let capture_root = temp_dir.path().join("captures");
        let original_path = write_pending_bundle(&capture_root);
        let sidecar_path = capture_root.join("pending").join(attempts_sidecar_name());
        let failed_path = capture_root.join("failed").join(bundle_name());

        let queue = UploadQueue::new(capture_root.clone(), upload_config(1), AlwaysFailUploader);
        let first_summary = queue
            .upload_pending_once()
            .await
            .expect("failed upload should be summarized");

        assert_eq!(first_summary.failed, 1);
        assert_eq!(first_summary.moved_to_failed, 0);
        assert!(original_path.exists());
        assert!(sidecar_path.exists());

        let second_summary = queue
            .upload_pending_once()
            .await
            .expect("terminal failed upload should be summarized");

        assert_eq!(second_summary.moved_to_failed, 1);
        assert!(!original_path.exists());
        assert!(failed_path.exists());
        assert!(!sidecar_path.exists());
        let state_json = fs::read_to_string(
            capture_root
                .join("state")
                .join("00000000000000000000000000000001.json"),
        )
        .expect("state should be readable");
        let state: BundleState = serde_json::from_str(&state_json).expect("state should parse");
        assert_eq!(state.status, PackageStatus::Failed);
    }

    #[derive(Clone, Default)]
    struct CountingUploader {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl PackageUploader for CountingUploader {
        async fn upload_bundle(&self, _bundle: UploadBundle<'_>) -> anyhow::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("enqueue should not upload");
        }
    }

    #[tokio::test]
    async fn enqueue_bundle_updates_sha_and_leaves_tlc_in_pending() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let capture_root = temp_dir.path().join("captures");
        let bundle_path = write_pending_bundle(&capture_root);
        let uploader = CountingUploader::default();
        let calls = Arc::clone(&uploader.calls);

        let queue = UploadQueue::new(capture_root.clone(), upload_config(5), uploader);
        let info = queue
            .enqueue_bundle(&bundle_path)
            .await
            .expect("bundle should be enqueued");

        assert_eq!(info.package_path, bundle_path);
        assert_eq!(
            info.size_bytes,
            fs::metadata(&info.package_path)
                .expect("bundle metadata should exist")
                .len()
        );
        assert_eq!(info.sha256.len(), 64);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let state_json = fs::read_to_string(
            capture_root
                .join("state")
                .join("00000000000000000000000000000001.json"),
        )
        .expect("state should be readable");
        assert!(state_json.contains(r#""sha256":"#));
        assert!(info.package_path.exists());
    }

    #[tokio::test]
    async fn upload_pending_once_ignores_non_matching_files_and_sidecars() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let capture_root = temp_dir.path().join("captures");
        let pending_dir = capture_root.join("pending");
        let matching_bundle = write_pending_bundle(&capture_root);
        fs::write(pending_dir.join("notes.txt"), b"ignore").expect("non-package should be written");
        fs::write(pending_dir.join("bundle.tlc.tmp"), b"ignore")
            .expect("temp file should be written");
        fs::write(
            pending_dir.join(attempts_sidecar_name()),
            r#"{"attempts":4}"#,
        )
        .expect("sidecar should be written");

        let queue = UploadQueue::new(
            capture_root.clone(),
            upload_config(5),
            AlwaysSucceedUploader,
        );
        let summary = queue
            .upload_pending_once()
            .await
            .expect("upload should succeed");

        assert_eq!(summary.scanned, 1);
        assert_eq!(summary.uploaded, 1);
        assert!(!matching_bundle.exists());
        assert!(pending_dir.join("notes.txt").exists());
        assert!(pending_dir.join("bundle.tlc.tmp").exists());
    }
}
