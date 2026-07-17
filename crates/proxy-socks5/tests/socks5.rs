use socks5_proto::{
    handshake::{
        Method as HandshakeMethod, Request as HandshakeRequest, Response as HandshakeResponse,
    },
    Address, Command, Reply, Request, Response,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};
use torchnexus_core::config::{BasicAuthConfig, CaptureTarget, RetryConfig, UploadConfig};
use torchnexus_core::filter::CaptureFilter;
use torchnexus_core::shutdown::{ShutdownSender, ShutdownSignal};
use torchnexus_proxy_socks5::server::{run_socks5_server, serve_socks5_listener};
use torchnexus_proxy_tcp::context::TcpRuntimeContext;
use torchnexus_storage_support::recorder::FileRecorder;
use torchnexus_uploader::client::AlwaysSucceedUploader;
use torchnexus_uploader::queue::UploadQueue;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> std::io::Result<Self> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "torchnexus-proxy-socks5-test-{unique}-{}-{temp_id}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
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
        let mut buf = [0_u8; 4];
        socket.read_exact(&mut buf).await.unwrap();
        socket.write_all(&buf).await.unwrap();
        let _ = release_rx.await;
    });
    (addr, release_tx)
}

async fn unused_local_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    addr
}

fn upload_config(root: &TempDir) -> UploadConfig {
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

async fn start_server(
    runtime: Arc<TcpRuntimeContext<AlwaysSucceedUploader>>,
) -> (
    SocketAddr,
    ShutdownSender,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = listener.local_addr().unwrap();

    let (shutdown_tx, shutdown) = ShutdownSignal::new();
    let server = tokio::spawn(serve_socks5_listener(runtime, listener, bind, shutdown));
    (bind, shutdown_tx, server)
}

async fn complete_no_auth_greeting(client: &mut TcpStream) {
    HandshakeRequest::new(vec![HandshakeMethod::NONE])
        .write_to(client)
        .await
        .unwrap();
    let response = HandshakeResponse::read_from(client).await.unwrap();
    assert_eq!(response.method, HandshakeMethod::NONE);
}

async fn run_raw_request_failure_case(request_bytes: &[u8]) -> Reply {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;
    client.write_all(request_bytes).await.unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();

    response.reply
}

fn first_bundle_path(root: &TempDir) -> PathBuf {
    std::fs::read_dir(root.path().join("captures").join("pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

async fn round_trip_handshake_request(request: HandshakeRequest) -> HandshakeRequest {
    let (mut writer, mut reader) = tokio::io::duplex(64);
    let expected = request.clone();
    tokio::spawn(async move {
        request.write_to(&mut writer).await.unwrap();
    });
    let actual = HandshakeRequest::read_from(&mut reader).await.unwrap();
    assert_eq!(actual.methods, expected.methods);
    actual
}

async fn round_trip_request(request: Request) -> Request {
    let (mut writer, mut reader) = tokio::io::duplex(512);
    let expected = request.clone();
    tokio::spawn(async move {
        request.write_to(&mut writer).await.unwrap();
    });
    let actual = Request::read_from(&mut reader).await.unwrap();
    assert_eq!(actual.command, expected.command);
    assert_eq!(actual.address, expected.address);
    actual
}

#[tokio::test]
async fn handshake_request_round_trips_no_auth_greeting() {
    let request = HandshakeRequest::new(vec![HandshakeMethod::NONE, HandshakeMethod::PASSWORD]);
    round_trip_handshake_request(request).await;
}

#[tokio::test]
async fn request_round_trips_ipv4_connect() {
    let request = Request::new(
        Command::Connect,
        Address::SocketAddress(SocketAddr::from(([127, 0, 0, 1], 9000))),
    );
    round_trip_request(request).await;
}

#[tokio::test]
async fn request_round_trips_domain_connect() {
    let request = Request::new(
        Command::Connect,
        Address::DomainAddress(b"example.com".to_vec(), 443),
    );
    round_trip_request(request).await;
}

#[tokio::test]
async fn request_round_trips_ipv6_connect() {
    let request = Request::new(
        Command::Connect,
        Address::SocketAddress("[::1]:9000".parse().unwrap()),
    );
    round_trip_request(request).await;
}

#[tokio::test]
async fn run_socks5_server_returns_bind_error_when_port_occupied() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = occupied.local_addr().unwrap();
    let (_, shutdown) = ShutdownSignal::new();

    let result = run_socks5_server(runtime, bind, shutdown).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn socks5_connect_ipv4_forwards_and_enqueues_tlc() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;

    let ip = match remote.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => panic!("echo server should bind ipv4"),
    };
    Request::new(
        Command::Connect,
        Address::SocketAddress(SocketAddr::from((ip, remote.port()))),
    )
    .write_to(&mut client)
    .await
    .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::Succeeded);

    client.write_all(b"ping").await.unwrap();
    let mut echo = [0_u8; 4];
    client.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"ping");
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();

    let queued: Vec<_> = std::fs::read_dir(temp.path().join("captures").join("pending"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].extension().and_then(|value| value.to_str()),
        Some("tlc")
    );
}

#[tokio::test]
async fn socks5_connect_failure_returns_general_failure_reply() {
    let temp = TempDir::new().unwrap();
    let remote = unused_local_addr().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;

    Request::new(Command::Connect, Address::SocketAddress(remote))
        .write_to(&mut client)
        .await
        .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::GeneralFailure);
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn socks5_connect_domain_uses_system_dns_and_finalizes_tlc() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;

    Request::new(
        Command::Connect,
        Address::DomainAddress(b"localhost".to_vec(), remote.port()),
    )
    .write_to(&mut client)
    .await
    .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::Succeeded);

    client.write_all(b"pong").await.unwrap();
    let mut echo = [0_u8; 4];
    client.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"pong");
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();

    let bundle_path = first_bundle_path(&temp);
    assert_eq!(
        bundle_path.extension().and_then(|value| value.to_str()),
        Some("tlc")
    );
}

#[tokio::test]
async fn socks5_rejects_unsupported_auth_methods() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    HandshakeRequest::new(vec![HandshakeMethod::GSSAPI, HandshakeMethod::PASSWORD])
        .write_to(&mut client)
        .await
        .unwrap();

    let response = HandshakeResponse::read_from(&mut client).await.unwrap();
    assert_eq!(response.method, HandshakeMethod::UNACCEPTABLE);

    let mut extra = [0_u8; 1];
    let read = client.read(&mut extra).await.unwrap();
    assert_eq!(read, 0, "server should close rejected auth connection");
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn socks5_rejects_unsupported_command_with_failure_response() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;
    Request::new(
        Command::Bind,
        Address::SocketAddress(SocketAddr::from(([127, 0, 0, 1], 9000))),
    )
    .write_to(&mut client)
    .await
    .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::CommandNotSupported);
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn socks5_rejects_udp_associate_with_failure_response() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;
    Request::new(
        Command::Associate,
        Address::SocketAddress(SocketAddr::from(([127, 0, 0, 1], 9000))),
    )
    .write_to(&mut client)
    .await
    .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::CommandNotSupported);
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn socks5_rejects_unsupported_address_type_with_failure_response() {
    let temp = TempDir::new().unwrap();
    let remote = spawn_echo_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;
    Request::new(
        Command::Connect,
        Address::SocketAddress("[::1]:9000".parse().unwrap()),
    )
    .write_to(&mut client)
    .await
    .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::AddressTypeNotSupported);
    drop(client);

    shutdown_tx.cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn socks5_rejects_non_zero_reserved_byte_with_general_failure_reply() {
    let reply =
        run_raw_request_failure_case(&[0x05, 0x01, 0x01, 0x01, 127, 0, 0, 1, 0x23, 0x28]).await;
    assert_eq!(reply, Reply::GeneralFailure);
}

#[tokio::test]
async fn socks5_rejects_unknown_command_with_failure_response() {
    let reply =
        run_raw_request_failure_case(&[0x05, 0x09, 0x00, 0x01, 127, 0, 0, 1, 0x23, 0x28]).await;
    assert_eq!(reply, Reply::CommandNotSupported);
}

#[tokio::test]
async fn socks5_rejects_unknown_address_type_with_failure_response() {
    let reply = run_raw_request_failure_case(&[0x05, 0x01, 0x00, 0x09]).await;
    assert_eq!(reply, Reply::AddressTypeNotSupported);
}

#[tokio::test]
async fn socks5_rejects_invalid_domain_encoding_with_general_failure_reply() {
    let reply =
        run_raw_request_failure_case(&[0x05, 0x01, 0x00, 0x03, 0x02, 0xff, 0xfe, 0x01, 0xbb]).await;
    assert_eq!(reply, Reply::GeneralFailure);
}

#[tokio::test]
async fn socks5_shutdown_waits_for_active_connection_to_finish() {
    let temp = TempDir::new().unwrap();
    let (remote, release_remote) = spawn_hold_open_server().await;
    let runtime = runtime(&temp, remote);
    let (bind, shutdown_tx, mut server) = start_server(runtime).await;

    let mut client = TcpStream::connect(bind).await.unwrap();
    complete_no_auth_greeting(&mut client).await;

    Request::new(Command::Connect, Address::SocketAddress(remote))
        .write_to(&mut client)
        .await
        .unwrap();

    let response = Response::read_from(&mut client).await.unwrap();
    assert_eq!(response.reply, Reply::Succeeded);

    client.write_all(b"ping").await.unwrap();
    let mut echo = [0_u8; 4];
    client.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"ping");

    shutdown_tx.cancel();
    assert!(
        timeout(Duration::from_millis(200), &mut server)
            .await
            .is_err(),
        "listener should wait for active socks5 connections to finish"
    );

    drop(client);
    let _ = release_remote.send(());
    server.await.unwrap().unwrap();
}
