use crate::context::{TcpForwardRuntimeConfig, TcpRuntimeContext};
use anyhow::Context;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use torchnexus_core::session::EntryType;
use torchnexus_storage_support::{direction::Direction, recorder::FileBundleRecorder};
use torchnexus_uploader::client::PackageUploader;

pub struct ConnectedStreamMetadata {
    pub target_addr: std::net::SocketAddr,
    pub target_host: Option<String>,
    pub entry_type: EntryType,
}

pub async fn pipe_bidirectional(
    client: TcpStream,
    remote: TcpStream,
    session: Option<&mut FileBundleRecorder>,
) -> anyhow::Result<()> {
    pipe_bidirectional_with_initial_client_data(client, remote, session, &[]).await
}

async fn pipe_bidirectional_with_initial_client_data(
    client: TcpStream,
    mut remote: TcpStream,
    mut session: Option<&mut FileBundleRecorder>,
    initial_client_data: &[u8],
) -> anyhow::Result<()> {
    if !initial_client_data.is_empty() {
        if let Some(session) = session.as_deref_mut() {
            session.write_chunk(Direction::ClientToServer, initial_client_data)?;
        }
        remote.write_all(initial_client_data).await?;
    }

    let (mut client_read, mut client_write) = client.into_split();
    let (mut remote_read, mut remote_write) = remote.into_split();
    let mut client_buf = vec![0_u8; 16 * 1024];
    let mut remote_buf = vec![0_u8; 16 * 1024];
    let mut client_open = true;
    let mut remote_open = true;

    while client_open || remote_open {
        tokio::select! {
            read = client_read.read(&mut client_buf), if client_open => {
                let n = read?;
                if n == 0 {
                    client_open = false;
                    remote_write.shutdown().await?;
                } else {
                    if let Some(session) = session.as_deref_mut() {
                        session.write_chunk(Direction::ClientToServer, &client_buf[..n])?;
                    }
                    remote_write.write_all(&client_buf[..n]).await?;
                }
            }
            read = remote_read.read(&mut remote_buf), if remote_open => {
                let n = read?;
                if n == 0 {
                    remote_open = false;
                    client_write.shutdown().await?;
                } else {
                    if let Some(session) = session.as_deref_mut() {
                        session.write_chunk(Direction::ServerToClient, &remote_buf[..n])?;
                    }
                    client_write.write_all(&remote_buf[..n]).await?;
                }
            }
        }
    }

    Ok(())
}

async fn finalize_session<U>(
    runtime: &TcpRuntimeContext<U>,
    session: &mut FileBundleRecorder,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    let closed = session.close()?;
    runtime
        .upload_queue
        .enqueue_bundle(&closed.bundle_path)
        .await?;
    Ok(())
}

fn session_result(
    result: anyhow::Result<()>,
    finalize_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (result, finalize_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(finalize_error)) => Err(finalize_error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(finalize_error)) => Err(error.context(format!(
            "session finalization also failed: {finalize_error:#}"
        ))),
    }
}

pub async fn handle_connected_streams<U>(
    client: TcpStream,
    client_addr: std::net::SocketAddr,
    remote: TcpStream,
    runtime: Arc<TcpRuntimeContext<U>>,
    target_addr: std::net::SocketAddr,
    target_host: Option<String>,
    entry_type: EntryType,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    handle_connected_streams_with_initial_client_data(
        client,
        client_addr,
        remote,
        runtime,
        ConnectedStreamMetadata {
            target_addr,
            target_host,
            entry_type,
        },
        Vec::new(),
    )
    .await
}

pub async fn handle_connected_streams_with_initial_client_data<U>(
    client: TcpStream,
    client_addr: std::net::SocketAddr,
    remote: TcpStream,
    runtime: Arc<TcpRuntimeContext<U>>,
    metadata: ConnectedStreamMetadata,
    initial_client_data: Vec<u8>,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    runtime.ensure_capture_dirs().await?;

    let capture_enabled = runtime.filter.should_capture(&metadata.target_addr);
    tracing::info!(
        client = %client_addr,
        target = %metadata.target_addr,
        host = ?metadata.target_host,
        entry_type = ?metadata.entry_type,
        capture_enabled,
        "已建立代理连接"
    );
    let mut session = runtime.recorder.start_bundle(capture_enabled)?;

    let result = pipe_bidirectional_with_initial_client_data(
        client,
        remote,
        session.as_mut(),
        &initial_client_data,
    )
    .await;

    if let Some(session) = session.as_mut() {
        let finalize_result = finalize_session(runtime.as_ref(), session).await;
        tracing::info!(target = %metadata.target_addr, "代理连接已结束，采集数据已入队");
        return session_result(result, finalize_result);
    }

    result
}

pub async fn handle_tcp_connection<U>(
    client: TcpStream,
    client_addr: std::net::SocketAddr,
    runtime: Arc<TcpRuntimeContext<U>>,
    forward: TcpForwardRuntimeConfig,
) -> anyhow::Result<()>
where
    U: PackageUploader,
{
    let remote = match TcpStream::connect(forward.remote)
        .await
        .with_context(|| format!("failed to connect remote {}", forward.remote))
    {
        Ok(remote) => remote,
        Err(error) => {
            tracing::warn!(client = %client_addr, remote = %forward.remote, error = %error, "连接目标服务器失败");
            runtime.ensure_capture_dirs().await?;

            let capture_enabled = runtime.filter.should_capture(&forward.remote);
            let _ = client_addr;
            let mut session = runtime.recorder.start_bundle(capture_enabled)?;

            if let Some(session) = session.as_mut() {
                let finalize_result = finalize_session(runtime.as_ref(), session).await;
                return session_result(Err(error), finalize_result);
            }

            return Err(error);
        }
    };

    handle_connected_streams(
        client,
        client_addr,
        remote,
        runtime,
        forward.remote,
        None,
        EntryType::TcpForward,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::net::TcpListener;
    use torchnexus_core::config::{BasicAuthConfig, CaptureTarget, RetryConfig, UploadConfig};
    use torchnexus_core::filter::CaptureFilter;
    use torchnexus_storage_support::{
        recorder::FileRecorder, test_support::SAMPLE_PROTOCOL_PACKET,
    };
    use torchnexus_uploader::client::AlwaysSucceedUploader;
    use torchnexus_uploader::queue::UploadQueue;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let test_id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "torchnexus-proxy-tcp-forward-{unique}-{}-{test_id}",
                std::process::id(),
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    async fn spawn_echo_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0_u8; 4096];
                    while let Ok(n) = socket.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if socket.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn unused_local_addr() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr
    }

    fn upload_config(_root: &TestDir) -> UploadConfig {
        UploadConfig {
            enabled: true,
            endpoint: "http://127.0.0.1/upload".to_string(),
            basic_auth: BasicAuthConfig {
                username: "agent".to_string(),
                password: "change-me".to_string(),
            },
            auto_package_on_disconnect: true,
            upload_interval_seconds: 60,
            retry: RetryConfig {
                max_attempts: 5,
                base_delay_seconds: 3,
            },
        }
    }

    fn runtime(
        root: &TestDir,
        remote: SocketAddr,
        save_uncaptured_sessions: bool,
    ) -> Arc<TcpRuntimeContext<AlwaysSucceedUploader>> {
        let capture_root = root.path().join("captures");
        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: remote.ip().to_string(),
            ports: Some(vec![remote.port()]),
        }])
        .unwrap();
        let recorder = FileRecorder::new(capture_root.clone(), save_uncaptured_sessions, true);
        let queue = Arc::new(UploadQueue::new(
            capture_root,
            upload_config(root),
            AlwaysSucceedUploader,
        ));
        Arc::new(TcpRuntimeContext {
            filter,
            recorder,
            upload_queue: queue,
        })
    }

    #[tokio::test]
    async fn forwards_bytes_and_finalizes_tlc_bundle() {
        let temp = TestDir::new();
        let remote = spawn_echo_server().await;
        let runtime = runtime(&temp, remote, false);

        let accept = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = accept.local_addr().unwrap();
        let server_task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            async move {
                let (client, client_addr) = accept.accept().await.unwrap();
                handle_tcp_connection(
                    client,
                    client_addr,
                    runtime,
                    TcpForwardRuntimeConfig {
                        name: "game-server-9000".to_string(),
                        bind,
                        remote,
                    },
                )
                .await
            }
        });

        let mut caller = TcpStream::connect(bind).await.unwrap();
        caller.write_all(SAMPLE_PROTOCOL_PACKET).await.unwrap();
        let mut buf = vec![0_u8; SAMPLE_PROTOCOL_PACKET.len()];
        caller.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, SAMPLE_PROTOCOL_PACKET);
        drop(caller);

        server_task.await.unwrap().unwrap();

        let pending_dir = temp.path().join("captures").join("pending");
        let bundles: Vec<_> = std::fs::read_dir(&pending_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(bundles.len(), 1);
        assert_eq!(
            bundles[0].extension().and_then(|value| value.to_str()),
            Some("tlc")
        );
    }

    #[tokio::test]
    async fn non_matching_target_skips_session_when_uncaptured_sessions_disabled() {
        let temp = TestDir::new();
        let remote = spawn_echo_server().await;

        let filter = CaptureFilter::new(vec![CaptureTarget {
            ip: "127.0.0.2".to_string(),
            ports: Some(vec![remote.port()]),
        }])
        .unwrap();
        let recorder = FileRecorder::new(temp.path().join("captures"), false, true);
        let queue = Arc::new(UploadQueue::new(
            temp.path().join("captures"),
            upload_config(&temp),
            AlwaysSucceedUploader,
        ));
        let runtime = Arc::new(TcpRuntimeContext {
            filter,
            recorder,
            upload_queue: queue,
        });

        let accept = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = accept.local_addr().unwrap();
        let server_task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            async move {
                let (client, client_addr) = accept.accept().await.unwrap();
                handle_tcp_connection(
                    client,
                    client_addr,
                    runtime,
                    TcpForwardRuntimeConfig {
                        name: "game-server-9000".to_string(),
                        bind,
                        remote,
                    },
                )
                .await
            }
        });

        let mut caller = TcpStream::connect(bind).await.unwrap();
        caller.write_all(b"skip").await.unwrap();
        let mut buf = [0_u8; 4];
        caller.read_exact(&mut buf).await.unwrap();
        drop(caller);

        server_task.await.unwrap().unwrap();

        assert!(temp.path().join("captures").exists());
        assert!(temp.path().join("captures").join("pending").exists());
        assert_eq!(
            std::fs::read_dir(temp.path().join("captures").join("pending"))
                .unwrap()
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn connect_failure_finalizes_tlc_bundle() {
        let temp = TestDir::new();
        let remote = unused_local_addr().await;
        let runtime = runtime(&temp, remote, false);

        let accept = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = accept.local_addr().unwrap();
        let server_task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            async move {
                let (client, client_addr) = accept.accept().await.unwrap();
                handle_tcp_connection(
                    client,
                    client_addr,
                    runtime,
                    TcpForwardRuntimeConfig {
                        name: "game-server-9000".to_string(),
                        bind,
                        remote,
                    },
                )
                .await
            }
        });

        let caller = TcpStream::connect(bind).await.unwrap();

        let error = server_task
            .await
            .unwrap()
            .expect_err("connect failure should surface");
        drop(caller);

        assert!(
            error
                .to_string()
                .contains(&format!("failed to connect remote {remote}")),
            "unexpected error: {error:#}"
        );

        let pending_dir = temp.path().join("captures").join("pending");
        let bundles: Vec<_> = std::fs::read_dir(&pending_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(bundles.len(), 1);
        assert_eq!(
            bundles[0].extension().and_then(|value| value.to_str()),
            Some("tlc")
        );

        let state_dir = temp.path().join("captures").join("state");
        let states: Vec<_> = std::fs::read_dir(&state_dir).unwrap().collect();
        assert_eq!(states.len(), 1);
    }

    #[tokio::test]
    async fn connected_flow_finalizes_tlc_bundle_for_socks5() {
        let temp = TestDir::new();
        let remote = spawn_echo_server().await;
        let runtime = runtime(&temp, remote, false);

        let accept = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = accept.local_addr().unwrap();
        let remote_stream = TcpStream::connect(remote).await.unwrap();

        let task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            async move {
                let (client, client_addr) = accept.accept().await.unwrap();
                handle_connected_streams(
                    client,
                    client_addr,
                    remote_stream,
                    runtime,
                    remote,
                    Some("example.com".to_string()),
                    EntryType::Socks5,
                )
                .await
            }
        });

        let mut caller = TcpStream::connect(bind).await.unwrap();
        caller.write_all(SAMPLE_PROTOCOL_PACKET).await.unwrap();
        let mut buf = vec![0_u8; SAMPLE_PROTOCOL_PACKET.len()];
        caller.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, SAMPLE_PROTOCOL_PACKET);
        drop(caller);

        task.await.unwrap().unwrap();

        let pending_dir = temp.path().join("captures").join("pending");
        let bundles: Vec<_> = std::fs::read_dir(&pending_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(bundles.len(), 1);
        assert_eq!(
            bundles[0].extension().and_then(|value| value.to_str()),
            Some("tlc")
        );
    }
}
