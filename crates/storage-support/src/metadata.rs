use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageStatus {
    Pending,
    Queued,
    Uploaded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleState {
    pub bundle_id: String,
    pub created_ms: u64,
    pub finalized_ms: Option<u64>,
    pub status: PackageStatus,
    pub file_size: u64,
    pub record_count: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleClosedInfo {
    pub bundle_id: String,
    pub bundle_path: PathBuf,
    pub finalized_ms: u64,
    pub file_size: u64,
    pub record_count: u64,
}
