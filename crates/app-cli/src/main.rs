mod session_index;

use anyhow::Context;
use clap::{Parser, Subcommand};
use session_index::list_sessions;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use torchnexus_core::config::AppConfig;
use torchnexus_core::filter::CaptureFilter;
use torchnexus_core::shutdown::{ShutdownSender, ShutdownSignal};
use torchnexus_proxy_socks5::server::serve_socks5_listener;
use torchnexus_proxy_tcp::context::{TcpForwardRuntimeConfig, TcpRuntimeContext};
use torchnexus_proxy_tcp::server::serve_tcp_forward_listener;
use torchnexus_runtime::AgentRuntime;
use torchnexus_storage_support::metadata::PackageStatus;
use torchnexus_storage_support::recorder::FileRecorder;
use torchnexus_uploader::client::RuntimeUploader;
use torchnexus_uploader::queue::UploadQueue;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "torchnexus-agent")]
#[command(about = "TorchNexus proxy-side TCP payload collection agent")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    CheckConfig {
        #[arg(long)]
        config: PathBuf,
    },
    ListSessions {
        #[arg(long)]
        config: PathBuf,
    },
}

struct RunningRuntime {
    shutdown: ShutdownSender,
    tasks: Vec<JoinHandle<anyhow::Result<()>>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run { config } => run(config).await,
        Commands::CheckConfig { config } => check_config(config).await,
        Commands::ListSessions { config } => list(config).await,
    }
}

async fn load_validated_config(path: PathBuf) -> anyhow::Result<AppConfig> {
    let config = AppConfig::load_from_path(&path)
        .with_context(|| format!("failed to load config {}", path.display()))?;
    config.validate()?;
    Ok(config)
}

async fn start_runtime(config: &AppConfig) -> anyhow::Result<RunningRuntime> {
    let filter = CaptureFilter::new(config.capture.targets.clone())?;
    let capture_root = PathBuf::from(&config.capture.save_dir);
    let recorder = FileRecorder::new(
        capture_root.clone(),
        config.capture.save_uncaptured_sessions,
        config.storage.flush_each_chunk,
    );
    let uploader = RuntimeUploader::from_config(&config.upload);
    let upload_queue = Arc::new(UploadQueue::new(
        capture_root,
        config.upload.clone(),
        uploader,
    ));
    let runtime = Arc::new(TcpRuntimeContext {
        filter,
        recorder,
        upload_queue,
    });

    let (shutdown_tx, shutdown) = ShutdownSignal::new();
    let mut tasks = Vec::new();
    let mut tcp_listeners = Vec::new();

    if config.upload.enabled {
        let runtime = Arc::clone(&runtime);
        let shutdown = shutdown.clone();
        let interval = std::time::Duration::from_secs(config.upload.upload_interval_seconds);
        tasks.push(tokio::spawn(async move {
            run_upload_scheduler(runtime, interval, shutdown).await
        }));
    }

    for item in &config.listen.tcp {
        let bind: SocketAddr = item
            .bind
            .parse()
            .with_context(|| format!("invalid tcp bind {}", item.bind))?;
        let remote: SocketAddr = item
            .remote
            .parse()
            .with_context(|| format!("invalid tcp remote {}", item.remote))?;
        let forward = TcpForwardRuntimeConfig {
            name: item.name.clone(),
            bind,
            remote,
        };
        let listener = TcpListener::bind(bind)
            .await
            .with_context(|| format!("failed to bind {bind}"))?;
        tcp_listeners.push((listener, forward));
    }

    let socks5_listener =
        if config.listen.socks5.enabled {
            let bind: SocketAddr =
                config.listen.socks5.bind.parse().with_context(|| {
                    format!("invalid socks5 bind {}", config.listen.socks5.bind)
                })?;
            Some((
                bind,
                TcpListener::bind(bind)
                    .await
                    .with_context(|| format!("failed to bind {bind}"))?,
            ))
        } else {
            None
        };

    if let Some((bind, listener)) = socks5_listener {
        let runtime = Arc::clone(&runtime);
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            serve_socks5_listener(runtime, listener, bind, shutdown).await
        }));
    }

    for (listener, forward) in tcp_listeners {
        let runtime = Arc::clone(&runtime);
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            serve_tcp_forward_listener(listener, runtime, forward, shutdown).await
        }));
    }

    Ok(RunningRuntime {
        shutdown: shutdown_tx,
        tasks,
    })
}

async fn run_upload_scheduler(
    runtime: Arc<TcpRuntimeContext<RuntimeUploader>>,
    interval: std::time::Duration,
    shutdown: ShutdownSignal,
) -> anyhow::Result<()> {
    let interval_ms = interval.as_millis();
    let mut interval = tokio::time::interval(interval);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!(interval_ms, "upload scheduler stopping");
                return Ok(());
            }
            _ = interval.tick() => {
                if let Err(err) = runtime.upload_queue.upload_pending_once().await {
                    tracing::warn!(error = ?err, "upload scheduler tick failed");
                }
            }
        }
    }
}

async fn run(path: PathBuf) -> anyhow::Result<()> {
    let config = load_validated_config(path).await?;
    init_tracing(&config);

    let runtime = AgentRuntime::start(config).await?;
    println!("proxy runtime initialized");
    println!("configured proxy listeners started");

    tokio::signal::ctrl_c().await?;
    runtime.stop().await?;

    Ok(())
}

async fn check_config(path: PathBuf) -> anyhow::Result<()> {
    load_validated_config(path).await?;
    println!("config ok");
    Ok(())
}

async fn list(path: PathBuf) -> anyhow::Result<()> {
    let config = load_validated_config(path).await?;
    let bundles = list_sessions(&config.capture.save_dir).await?;
    for bundle in bundles {
        println!(
            "{}\t{}\t{}\t{}",
            bundle.bundle_id,
            chrono::DateTime::from_timestamp_millis(bundle.created_ms as i64)
                .expect("bundle timestamp should be valid")
                .to_rfc3339(),
            package_status(bundle.status),
            bundle.file_size
        );
    }
    Ok(())
}

fn package_status(status: PackageStatus) -> &'static str {
    match status {
        PackageStatus::Pending => "pending",
        PackageStatus::Queued => "queued",
        PackageStatus::Uploaded => "uploaded",
        PackageStatus::Failed => "failed",
    }
}

fn init_tracing(config: &AppConfig) {
    static TRACING: OnceLock<()> = OnceLock::new();

    TRACING.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new(&config.log.level))
            .unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
    });
}

#[cfg(test)]
mod tests {
    use super::run_upload_scheduler;
    use anyhow::Context;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use torchnexus_core::config::{BasicAuthConfig, CaptureTarget, RetryConfig, UploadConfig};
    use torchnexus_core::filter::CaptureFilter;
    use torchnexus_core::shutdown::ShutdownSignal;
    use torchnexus_proxy_tcp::context::TcpRuntimeContext;
    use torchnexus_storage_support::recorder::FileRecorder;
    use torchnexus_uploader::client::RuntimeUploader;
    use torchnexus_uploader::queue::UploadQueue;

    fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
        headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then_some(value.trim())
        })
    }

    fn decode_chunked_body(mut bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let size_end = bytes
                .windows(2)
                .position(|window| window == b"\r\n")
                .context("chunked body missing size line terminator")?;
            let size = std::str::from_utf8(&bytes[..size_end])
                .context("chunk size should be utf-8")?
                .trim();
            let size = usize::from_str_radix(size, 16).context("chunk size should be hex")?;
            bytes = &bytes[size_end + 2..];
            if size == 0 {
                break;
            }
            if bytes.len() < size + 2 {
                anyhow::bail!("chunked body ended before declared chunk size");
            }
            body.extend_from_slice(&bytes[..size]);
            bytes = &bytes[size + 2..];
        }
        Ok(body)
    }

    async fn spawn_capture_server() -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0_u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let header_end = header_end + 4;
                    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                    if let Some(content_length) = header_value(&headers, "content-length") {
                        let content_length = content_length.parse::<usize>().unwrap();
                        if bytes.len() >= header_end + content_length {
                            break;
                        }
                    } else if bytes[header_end..]
                        .windows(5)
                        .any(|window| window == b"0\r\n\r\n")
                    {
                        break;
                    }
                }
            }
            let header_end = bytes
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
            let body = if let Some(content_length) = header_value(&headers, "content-length") {
                let content_length = content_length.parse::<usize>().unwrap();
                bytes[header_end..header_end + content_length].to_vec()
            } else {
                decode_chunked_body(&bytes[header_end..]).unwrap()
            };
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
            let mut request = headers.into_bytes();
            request.extend_from_slice(&body);
            request
        });
        (format!("http://{addr}/upload"), task)
    }

    fn upload_config(endpoint: String) -> UploadConfig {
        UploadConfig {
            enabled: true,
            endpoint,
            basic_auth: BasicAuthConfig {
                username: "agent".to_string(),
                password: "secret".to_string(),
            },
            auto_package_on_disconnect: true,
            upload_interval_seconds: 60,
            retry: RetryConfig {
                max_attempts: 5,
                base_delay_seconds: 1,
            },
        }
    }

    fn write_pending_bundle(temp: &TempDir) -> std::path::PathBuf {
        let capture_root = temp.path().join("captures");
        let pending_dir = capture_root.join("pending");
        let state_dir = capture_root.join("state");
        std::fs::create_dir_all(&pending_dir).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        let bundle_path = pending_dir.join("00000000000000000000000000000001.tlc");
        std::fs::write(&bundle_path, b"TLC1 scheduler upload").unwrap();
        std::fs::write(
            state_dir.join("00000000000000000000000000000001.json"),
            r#"{
  "bundle_id": "00000000000000000000000000000001",
  "created_ms": 1,
  "finalized_ms": 2,
  "status": "queued",
  "file_size": 0,
  "record_count": 1,
  "sha256": null
}"#,
        )
        .unwrap();
        capture_root
    }

    #[tokio::test]
    async fn upload_scheduler_posts_pending_tlc_with_real_http_uploader() {
        let temp = TempDir::new().unwrap();
        let capture_root = write_pending_bundle(&temp);
        let (endpoint, request_task) = spawn_capture_server().await;
        let config = upload_config(endpoint);
        let runtime = Arc::new(TcpRuntimeContext {
            filter: CaptureFilter::new(vec![CaptureTarget {
                ip: "127.0.0.1".to_string(),
                ports: None,
            }])
            .unwrap(),
            recorder: FileRecorder::new(capture_root.clone(), false, true),
            upload_queue: Arc::new(UploadQueue::new(
                capture_root.clone(),
                config.clone(),
                RuntimeUploader::from_config(&config),
            )),
        });
        let (shutdown_tx, shutdown) = ShutdownSignal::new();

        let scheduler = tokio::spawn(run_upload_scheduler(
            Arc::clone(&runtime),
            std::time::Duration::from_millis(20),
            shutdown,
        ));

        let request = tokio::time::timeout(std::time::Duration::from_secs(5), request_task)
            .await
            .expect("upload should be triggered")
            .unwrap();
        shutdown_tx.cancel();
        scheduler.await.unwrap().unwrap();

        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.contains("POST /upload HTTP/1.1"));
        assert!(request_text.contains("name=\"sha256\""));
        assert!(request_text
            .contains("name=\"data\"; filename=\"00000000000000000000000000000001.tlc\""));
        assert!(request
            .windows(b"TLC1 scheduler upload".len())
            .any(|w| w == b"TLC1 scheduler upload"));
        assert!(capture_root
            .join("uploaded")
            .join("00000000000000000000000000000001.tlc")
            .exists());
    }
}
