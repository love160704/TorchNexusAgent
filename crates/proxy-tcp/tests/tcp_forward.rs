use std::net::SocketAddr;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};
use torchnexus_core::config::{BasicAuthConfig, CaptureTarget, RetryConfig, UploadConfig};
use torchnexus_core::filter::CaptureFilter;
use torchnexus_core::shutdown::ShutdownSignal;
use torchnexus_proxy_tcp::context::{TcpForwardRuntimeConfig, TcpRuntimeContext};
use torchnexus_proxy_tcp::server::run_tcp_forward_server;
use torchnexus_storage_support::{recorder::FileRecorder, test_support::SAMPLE_PROTOCOL_PACKET};
use torchnexus_uploader::client::AlwaysSucceedUploader;
use torchnexus_uploader::queue::UploadQueue;

mod tempfile {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new() -> std::io::Result<Self> {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let temp_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "torchnexus-proxy-tcp-test-{unique}-{}-{temp_id}",
                std::process::id(),
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
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
                loop {
                    let Ok(n) = socket.read(&mut buf).await else {
                        break;
                    };
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

async fn spawn_hold_open_server() -> (SocketAddr, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0_u8; SAMPLE_PROTOCOL_PACKET.len()];
        socket.read_exact(&mut buf).await.unwrap();
        socket.write_all(&buf).await.unwrap();
        let _ = release_rx.await;
    });
    (addr, release_tx)
}

fn upload_config(_root: &TempDir) -> UploadConfig {
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

fn runtime(root: &TempDir, remote: SocketAddr) -> Arc<TcpRuntimeContext<AlwaysSucceedUploader>> {
    let capture_root = root.path().join("captures");
    let filter = CaptureFilter::new(vec![CaptureTarget {
        ip: remote.ip().to_string(),
        ports: Some(vec![remote.port()]),
    }])
    .unwrap();
    let recorder = FileRecorder::new(capture_root.clone(), false, true);
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
async fn listener_accepts_forwards_and_stops_after_shutdown() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = probe.local_addr().unwrap();
    drop(probe);

    let (shutdown_tx, shutdown) = ShutdownSignal::new();
    let server = tokio::spawn(run_tcp_forward_server(
        runtime,
        TcpForwardRuntimeConfig {
            name: "game-server-9000".to_string(),
            bind,
            remote,
        },
        shutdown,
    ));

    let mut client = TcpStream::connect(bind).await.unwrap();
    client.write_all(SAMPLE_PROTOCOL_PACKET).await.unwrap();
    let mut buf = vec![0_u8; SAMPLE_PROTOCOL_PACKET.len()];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, SAMPLE_PROTOCOL_PACKET);
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();

    let connect_after_shutdown = TcpStream::connect(bind).await;
    assert!(connect_after_shutdown.is_err());
}

#[tokio::test]
async fn listener_bind_failure_returns_error() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = occupied.local_addr().unwrap();
    let (_, shutdown) = ShutdownSignal::new();

    let result = run_tcp_forward_server(
        runtime,
        TcpForwardRuntimeConfig {
            name: "game-server-9000".to_string(),
            bind,
            remote,
        },
        shutdown,
    )
    .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn shutdown_waits_for_active_connection_to_finish() {
    let temp = TempDir::new().unwrap();
    let (remote, release_remote) = spawn_hold_open_server().await;
    let runtime = runtime(&temp, remote);
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = probe.local_addr().unwrap();
    drop(probe);

    let (shutdown_tx, shutdown) = ShutdownSignal::new();
    let mut server = tokio::spawn(run_tcp_forward_server(
        runtime,
        TcpForwardRuntimeConfig {
            name: "game-server-9000".to_string(),
            bind,
            remote,
        },
        shutdown,
    ));

    let mut client = TcpStream::connect(bind).await.unwrap();
    client.write_all(SAMPLE_PROTOCOL_PACKET).await.unwrap();
    let mut buf = vec![0_u8; SAMPLE_PROTOCOL_PACKET.len()];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, SAMPLE_PROTOCOL_PACKET);

    shutdown_tx.cancel();
    assert!(
        timeout(Duration::from_millis(200), &mut server)
            .await
            .is_err(),
        "listener should wait for active connections to finish"
    );

    drop(client);
    let _ = release_remote.send(());
    server.await.unwrap().unwrap();

    let pending_dir = temp.path().join("captures").join("pending");
    let queued: Vec<_> = std::fs::read_dir(&pending_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].extension().and_then(|value| value.to_str()),
        Some("tlc")
    );
}
