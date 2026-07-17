use crate::direction::Direction;
use crate::metadata::{BundleClosedInfo, BundleState, PackageStatus};
use anyhow::{anyhow, bail, Context};
use chrono::Utc;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

pub struct FileRecorder {
    root_dir: PathBuf,
    save_uncaptured_sessions: bool,
    recorder: *mut torchnexus_agent_storage_recorder,
}

unsafe impl Send for FileRecorder {}
unsafe impl Sync for FileRecorder {}

impl FileRecorder {
    pub fn new(save_dir: PathBuf, save_uncaptured_sessions: bool, flush_each_chunk: bool) -> Self {
        let save_dir_text = save_dir.to_string_lossy().into_owned();
        let root_dir = save_dir;
        let root_dir_c =
            CString::new(save_dir_text).expect("capture root should not contain interior nul");
        let mut recorder = std::ptr::null_mut();
        let status = unsafe {
            torchnexus_agent_storage_recorder_new(
                root_dir_c.as_ptr(),
                save_uncaptured_sessions,
                flush_each_chunk,
                &mut recorder,
            )
        };
        if status != 0 {
            panic!(
                "failed to create storage recorder: {}",
                last_error_message()
            );
        }
        if recorder.is_null() {
            panic!("storage recorder returned a null handle");
        }

        Self {
            root_dir,
            save_uncaptured_sessions,
            recorder,
        }
    }

    pub fn start_bundle(
        &self,
        capture_enabled: bool,
    ) -> anyhow::Result<Option<FileBundleRecorder>> {
        if !capture_enabled && !self.save_uncaptured_sessions {
            return Ok(None);
        }

        let mut bundle = std::ptr::null_mut();
        ffi_status(unsafe {
            torchnexus_agent_storage_recorder_start_bundle(
                self.recorder,
                capture_enabled,
                &mut bundle,
            )
        })?;
        if bundle.is_null() {
            bail!("storage bundle handle was null");
        }

        Ok(Some(FileBundleRecorder {
            root_dir: self.root_dir.clone(),
            created_ms: now_ms(),
            bundle: Some(bundle),
        }))
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }
}

impl Drop for FileRecorder {
    fn drop(&mut self) {
        if !self.recorder.is_null() {
            unsafe { torchnexus_agent_storage_recorder_free(self.recorder) };
            self.recorder = std::ptr::null_mut();
        }
    }
}

pub struct FileBundleRecorder {
    root_dir: PathBuf,
    created_ms: u64,
    bundle: Option<*mut torchnexus_agent_storage_bundle>,
}

unsafe impl Send for FileBundleRecorder {}

impl FileBundleRecorder {
    pub fn write_chunk(&mut self, direction: Direction, data: &[u8]) -> anyhow::Result<()> {
        let bundle = self
            .bundle
            .ok_or_else(|| anyhow!("bundle already closed"))?;
        ffi_status(unsafe {
            torchnexus_agent_storage_bundle_write_chunk(
                bundle,
                direction as u8,
                data.as_ptr(),
                data.len(),
            )
        })
    }

    pub fn close(&mut self) -> anyhow::Result<BundleClosedInfo> {
        let bundle = self
            .bundle
            .take()
            .ok_or_else(|| anyhow!("bundle already closed"))?;
        let mut closed = std::ptr::null_mut();
        let close_result =
            ffi_status(unsafe { torchnexus_agent_storage_bundle_close(bundle, &mut closed) });
        close_result?;

        if closed.is_null() {
            unsafe { torchnexus_agent_storage_bundle_free(bundle) };
            bail!("closed bundle handle was null");
        }

        let info = closed_bundle_info(closed, &self.root_dir, self.created_ms);
        unsafe { torchnexus_agent_storage_closed_bundle_free(closed) };
        unsafe { torchnexus_agent_storage_bundle_free(bundle) };
        info
    }
}

impl Drop for FileBundleRecorder {
    fn drop(&mut self) {
        if let Some(bundle) = self.bundle.take() {
            unsafe { torchnexus_agent_storage_bundle_free(bundle) };
        }
    }
}

pub fn write_bundle_state(path: &Path, state: &BundleState) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(state)?;
    fs::write(path, json)
        .with_context(|| format!("failed to write bundle state {}", path.display()))
}

fn closed_bundle_info(
    closed: *mut torchnexus_agent_storage_closed_bundle,
    root_dir: &Path,
    created_ms: u64,
) -> anyhow::Result<BundleClosedInfo> {
    let bundle_id = unsafe { c_string(torchnexus_agent_storage_closed_bundle_id(closed)) }?;
    let bundle_path =
        PathBuf::from(unsafe { c_string(torchnexus_agent_storage_closed_bundle_path(closed)) }?);
    let record_count = unsafe { torchnexus_agent_storage_closed_bundle_record_count(closed) };
    let final_path = finalize_bundle_path(root_dir, &bundle_id, &bundle_path)?;
    let file_size = fs::metadata(&final_path)
        .with_context(|| format!("failed to stat bundle {}", final_path.display()))?
        .len();
    let finalized_ms = now_ms();

    let state_dir = root_dir.join("state");
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
    let state = BundleState {
        bundle_id: bundle_id.clone(),
        created_ms,
        finalized_ms: Some(finalized_ms),
        status: PackageStatus::Queued,
        file_size,
        record_count,
        sha256: None,
    };
    write_bundle_state(&state_dir.join(format!("{bundle_id}.json")), &state)?;

    Ok(BundleClosedInfo {
        bundle_id,
        bundle_path: final_path,
        finalized_ms,
        file_size,
        record_count,
    })
}

fn finalize_bundle_path(
    root_dir: &Path,
    bundle_id: &str,
    bundle_path: &Path,
) -> anyhow::Result<PathBuf> {
    let pending_dir = root_dir.join("pending");
    fs::create_dir_all(&pending_dir)
        .with_context(|| format!("failed to create pending dir {}", pending_dir.display()))?;

    let final_path = pending_dir.join(format!("{bundle_id}.tlc.zst"));
    let temporary_path = pending_dir.join(format!("{bundle_id}.tlc.zst.tmp"));
    let source = fs::File::open(bundle_path)
        .with_context(|| format!("failed to open bundle {}", bundle_path.display()))?;
    let destination = fs::File::create(&temporary_path)
        .with_context(|| format!("failed to create compressed bundle {}", temporary_path.display()))?;
    zstd::stream::copy_encode(source, destination, 0)
        .with_context(|| format!("failed to compress bundle {}", bundle_path.display()))?;
    fs::rename(&temporary_path, &final_path).with_context(|| {
        format!(
            "failed to finalize compressed bundle {} -> {}",
            temporary_path.display(),
            final_path.display()
        )
    })?;
    if bundle_path != final_path {
        fs::remove_file(bundle_path).with_context(|| {
            format!("failed to remove uncompressed bundle {}", bundle_path.display())
        })?;
    }

    Ok(final_path)
}

fn now_ms() -> u64 {
    Utc::now().timestamp_millis() as u64
}

fn ffi_status(status: c_int) -> anyhow::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        bail!("storage ffi call failed: {}", last_error_message());
    }
}

fn last_error_message() -> String {
    unsafe { c_string(torchnexus_agent_storage_last_error_message()) }
        .unwrap_or_else(|_| "unknown storage ffi error".to_string())
}

unsafe fn c_string(ptr: *const c_char) -> anyhow::Result<String> {
    if ptr.is_null() {
        bail!("ffi returned null string");
    }
    Ok(CStr::from_ptr(ptr)
        .to_str()
        .with_context(|| "ffi string was not utf-8")?
        .to_string())
}

#[repr(C)]
struct torchnexus_agent_storage_recorder {
    _private: [u8; 0],
}

#[repr(C)]
struct torchnexus_agent_storage_bundle {
    _private: [u8; 0],
}

#[repr(C)]
struct torchnexus_agent_storage_closed_bundle {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn torchnexus_agent_storage_recorder_new(
        root_dir: *const c_char,
        save_uncaptured_sessions: bool,
        flush_each_chunk: bool,
        out_recorder: *mut *mut torchnexus_agent_storage_recorder,
    ) -> c_int;
    fn torchnexus_agent_storage_recorder_start_bundle(
        recorder: *mut torchnexus_agent_storage_recorder,
        capture_enabled: bool,
        out_bundle: *mut *mut torchnexus_agent_storage_bundle,
    ) -> c_int;
    fn torchnexus_agent_storage_bundle_write_chunk(
        bundle: *mut torchnexus_agent_storage_bundle,
        direction: u8,
        data: *const u8,
        len: usize,
    ) -> c_int;
    fn torchnexus_agent_storage_bundle_close(
        bundle: *mut torchnexus_agent_storage_bundle,
        out_closed: *mut *mut torchnexus_agent_storage_closed_bundle,
    ) -> c_int;
    fn torchnexus_agent_storage_closed_bundle_id(
        closed: *const torchnexus_agent_storage_closed_bundle,
    ) -> *const c_char;
    fn torchnexus_agent_storage_closed_bundle_path(
        closed: *const torchnexus_agent_storage_closed_bundle,
    ) -> *const c_char;
    fn torchnexus_agent_storage_closed_bundle_record_count(
        closed: *const torchnexus_agent_storage_closed_bundle,
    ) -> u64;
    fn torchnexus_agent_storage_last_error_message() -> *const c_char;
    fn torchnexus_agent_storage_recorder_free(recorder: *mut torchnexus_agent_storage_recorder);
    fn torchnexus_agent_storage_bundle_free(bundle: *mut torchnexus_agent_storage_bundle);
    fn torchnexus_agent_storage_closed_bundle_free(
        closed: *mut torchnexus_agent_storage_closed_bundle,
    );
}

#[cfg(test)]
mod tests {
    use super::{finalize_bundle_path, now_ms, write_bundle_state, BundleState, FileRecorder};
    use crate::direction::Direction;
    use crate::test_support::SAMPLE_PROTOCOL_PACKET;
    use crate::PackageStatus;

    #[test]
    fn bundle_state_writes_pretty_json() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bundle.json");
        let state = BundleState {
            bundle_id: "bundle-1".to_string(),
            created_ms: 1,
            finalized_ms: Some(2),
            status: PackageStatus::Queued,
            file_size: 3,
            record_count: 4,
            sha256: None,
        };

        write_bundle_state(&path, &state).unwrap();

        let json = std::fs::read_to_string(path).unwrap();
        assert!(json.contains("\"status\": \"queued\""));
    }

    #[test]
    fn finalize_bundle_path_moves_bundle_into_pending() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let current_dir = root.join("current");
        std::fs::create_dir_all(&current_dir).unwrap();
        let bundle_path = current_dir.join("bundle-1.tlc");
        std::fs::write(&bundle_path, b"hello").unwrap();

        let final_path = finalize_bundle_path(root, "bundle-1", &bundle_path).unwrap();

        assert_eq!(final_path, root.join("pending").join("bundle-1.tlc"));
        assert!(final_path.exists());
        assert!(!bundle_path.exists());
    }

    #[test]
    fn finalize_bundle_path_compresses_bundle_as_zstd_in_pending() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let current_dir = root.join("current");
        std::fs::create_dir_all(&current_dir).unwrap();
        let bundle_path = current_dir.join("bundle-1.tlc");
        std::fs::write(&bundle_path, b"TLC\\0compressed capture").unwrap();

        let final_path = finalize_bundle_path(root, "bundle-1", &bundle_path).unwrap();

        assert_eq!(final_path, root.join("pending").join("bundle-1.tlc.zst"));
        assert_eq!(
            zstd::stream::decode_all(std::fs::File::open(&final_path).unwrap()).unwrap(),
            b"TLC\\0compressed capture"
        );
        assert!(!bundle_path.exists());
    }

    #[test]
    fn now_ms_returns_positive_timestamp() {
        assert!(now_ms() > 0);
    }

    #[test]
    fn recorder_can_write_and_close_bundle_with_real_protocol_packet() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("captures");
        std::fs::create_dir_all(root.join("current")).unwrap();
        let recorder = FileRecorder::new(root.clone(), true, true);
        let mut bundle = recorder.start_bundle(true).unwrap().unwrap();

        bundle
            .write_chunk(Direction::ClientToServer, SAMPLE_PROTOCOL_PACKET)
            .unwrap();
        let closed = bundle.close().unwrap();

        assert_eq!(
            closed.bundle_path.parent(),
            Some(root.join("pending").as_path())
        );
        assert!(root
            .join("state")
            .join(format!("{}.json", closed.bundle_id))
            .exists());
        assert!(closed.file_size > 0);
        assert_eq!(closed.record_count, 1);
    }
}
