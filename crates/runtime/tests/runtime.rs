use std::net::SocketAddr;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use socks5_proto::handshake::{Method, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use torchnexus_core::config::AppConfig;
use torchnexus_runtime::AgentRuntime;

fn config(bind: SocketAddr, capture_dir: &std::path::Path) -> AppConfig {
    AppConfig::from_yaml_str(&format!(
        r#"listen:
  socks5:
    enabled: true
    bind: "{bind}"
    auth:
      username: "lan-user"
      password: "lan-password"
  http:
    enabled: true
    bind: "127.0.0.1:0"
    auth:
      username: "lan-user"
      password: "lan-password"
  tcp: []
capture:
  targets:
    - ip: "127.0.0.1"
      ports: [65535]
  save_dir: "{}"
  save_uncaptured_sessions: false
upload:
  enabled: false
  endpoint: "https://example.invalid/upload"
  basic_auth:
    username: "agent"
    password: "secret"
  auto_package_on_disconnect: true
  upload_interval_seconds: 60
  retry:
    max_attempts: 1
    base_delay_seconds: 1
storage:
  flush_each_chunk: true
log:
  level: "info"
"#,
        capture_dir.display().to_string().replace('\\', "/"),
    ))
    .unwrap()
}

async fn read_http_head(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return String::from_utf8(bytes).unwrap();
        }
    }
}

fn proxy_authorization() -> String {
    format!("Basic {}", STANDARD.encode("lan-user:lan-password"))
}

#[tokio::test]
async fn runtime_requires_socks5_password_and_releases_its_listener_on_stop() {
    let temp = tempfile::tempdir().unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = probe.local_addr().unwrap();
    drop(probe);

    let runtime = AgentRuntime::start(config(bind, temp.path()))
        .await
        .unwrap();
    let mut client = TcpStream::connect(bind).await.unwrap();
    Request::new(vec![Method::NONE, Method::PASSWORD])
        .write_to(&mut client)
        .await
        .unwrap();
    let method = Response::read_from(&mut client).await.unwrap();
    assert_eq!(method.method, Method::PASSWORD);

    client
        .write_all(&[
            0x01, 8, b'l', b'a', b'n', b'-', b'u', b's', b'e', b'r', 12, b'l', b'a', b'n', b'-',
            b'p', b'a', b's', b's', b'w', b'o', b'r', b'd',
        ])
        .await
        .unwrap();
    let mut auth_response = [0_u8; 2];
    client.read_exact(&mut auth_response).await.unwrap();
    assert_eq!(auth_response, [0x01, 0x00]);

    drop(client);
    runtime.stop().await.unwrap();
    TcpListener::bind(bind)
        .await
        .expect("runtime stop should release its SOCKS5 listener");
}

#[tokio::test]
async fn runtime_starts_an_unauthenticated_private_listener_alongside_lan_listener() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::start_with_private_socks5(
        config("127.0.0.1:0".parse().unwrap(), temp.path()),
        Some("127.0.0.1:0".parse().unwrap()),
    )
    .await
    .unwrap();

    let lan_bind = runtime.socks5_bind_addr().unwrap();
    let private_bind = runtime.private_socks5_bind_addr().unwrap();
    assert_ne!(lan_bind, private_bind);

    let mut private_client = TcpStream::connect(private_bind).await.unwrap();
    Request::new(vec![Method::NONE])
        .write_to(&mut private_client)
        .await
        .unwrap();
    let method = Response::read_from(&mut private_client).await.unwrap();
    assert_eq!(method.method, Method::NONE);

    drop(private_client);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn runtime_stop_releases_listener_when_a_client_never_finishes_handshake() {
    let temp = tempfile::tempdir().unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bind = probe.local_addr().unwrap();
    drop(probe);

    let runtime = AgentRuntime::start(config(bind, temp.path()))
        .await
        .unwrap();
    let _client = TcpStream::connect(bind).await.unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(1), runtime.stop())
        .await
        .expect("runtime stop should not wait forever for an active client")
        .unwrap();
    TcpListener::bind(bind)
        .await
        .expect("forced stop should release the SOCKS5 listener");
}

#[tokio::test]
async fn http_proxy_requires_authentication() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::start(config("127.0.0.1:0".parse().unwrap(), temp.path()))
        .await
        .unwrap();
    let mut client = TcpStream::connect(runtime.http_bind_addr().unwrap())
        .await
        .unwrap();
    client
        .write_all(b"GET http://example.invalid/ HTTP/1.1\r\nHost: example.invalid\r\n\r\n")
        .await
        .unwrap();

    let response = read_http_head(&mut client).await;
    assert!(response.starts_with("HTTP/1.1 407 Proxy Authentication Required\r\n"));
    assert!(response.contains("Proxy-Authenticate: Basic realm=\"TorchNexus\"\r\n"));

    drop(client);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn http_proxy_connect_opens_a_tcp_tunnel() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut data = [0_u8; 4];
        stream.read_exact(&mut data).await.unwrap();
        stream.write_all(&data).await.unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let mut runtime_config = config("127.0.0.1:0".parse().unwrap(), temp.path());
    runtime_config.capture.targets[0].ports = Some(vec![origin_addr.port()]);
    let runtime = AgentRuntime::start(runtime_config).await.unwrap();
    let mut client = TcpStream::connect(runtime.http_bind_addr().unwrap())
        .await
        .unwrap();
    client
        .write_all(
            format!(
                "CONNECT {origin_addr} HTTP/1.1\r\nHost: {origin_addr}\r\nProxy-Authorization: {}\r\n\r\nping",
                proxy_authorization()
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let response = read_http_head(&mut client).await;
    assert_eq!(response, "HTTP/1.1 200 Connection Established\r\n\r\n");
    let mut echoed = [0_u8; 4];
    client.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping");

    drop(client);
    origin_task.await.unwrap();
    runtime.stop().await.unwrap();
    let bundles = std::fs::read_dir(temp.path().join("pending"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(bundles.len(), 1);
    assert_eq!(
        bundles[0]
            .path()
            .extension()
            .and_then(|value| value.to_str()),
        Some("tlc")
    );
}

#[tokio::test]
async fn http_proxy_forwards_absolute_form_requests_to_the_origin() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let request = read_http_head(&mut stream).await;
        assert!(request.starts_with("GET /?x=1 HTTP/1.1\r\n"));
        assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
        assert!(!request.to_ascii_lowercase().contains("proxy-connection"));
        assert!(!request.contains("X-Remove:"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .unwrap();
    });
    let temp = tempfile::tempdir().unwrap();
    let mut runtime_config = config("127.0.0.1:0".parse().unwrap(), temp.path());
    runtime_config.capture.targets[0].ports = Some(vec![origin_addr.port()]);
    let runtime = AgentRuntime::start(runtime_config).await.unwrap();
    let mut client = TcpStream::connect(runtime.http_bind_addr().unwrap())
        .await
        .unwrap();
    client
        .write_all(
            format!(
                "GET hTtP://{origin_addr}?x=1 HTTP/1.1\r\nHost: {origin_addr}\r\nProxy-Connection: keep-alive\r\nConnection: X-Remove\r\nX-Remove: secret\r\nProxy-Authorization: {}\r\n\r\n",
                proxy_authorization()
            )
            .as_bytes(),
        )
        .await
        .unwrap();

    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    assert!(response.ends_with(b"\r\n\r\nok"));

    drop(client);
    origin_task.await.unwrap();
    runtime.stop().await.unwrap();
    let bundles = std::fs::read_dir(temp.path().join("pending"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(bundles.len(), 1);
    assert_eq!(
        bundles[0]
            .path()
            .extension()
            .and_then(|value| value.to_str()),
        Some("tlc")
    );
}
