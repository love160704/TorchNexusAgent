use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use torchnexus_core::filter::CaptureFilter;
use torchnexus_storage_support::recorder::FileRecorder;
use torchnexus_uploader::client::PackageUploader;
use torchnexus_uploader::queue::UploadQueue;

pub struct TcpRuntimeContext<U>
where
    U: PackageUploader,
{
    pub filter: CaptureFilter,
    pub recorder: FileRecorder,
    pub upload_queue: Arc<UploadQueue<U>>,
}

impl<U> TcpRuntimeContext<U>
where
    U: PackageUploader,
{
    pub async fn ensure_capture_dirs(&self) -> anyhow::Result<()> {
        for dir in [
            self.recorder.root_dir().join("current"),
            self.recorder.root_dir().join("pending"),
            self.recorder.root_dir().join("uploading"),
            self.recorder.root_dir().join("uploaded"),
            self.recorder.root_dir().join("failed"),
            self.recorder.root_dir().join("state"),
        ] {
            tokio::fs::create_dir_all(&dir)
                .await
                .with_context(|| format!("failed to create capture dir {}", dir.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TcpForwardRuntimeConfig {
    pub name: String,
    pub bind: SocketAddr,
    pub remote: SocketAddr,
}
