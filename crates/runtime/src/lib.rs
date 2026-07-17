//! Cross-platform lifecycle for the TorchNexus capture agent.

use anyhow::Context;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use torchnexus_core::config::AppConfig;
use torchnexus_core::filter::CaptureFilter;
use torchnexus_core::shutdown::{ShutdownSender, ShutdownSignal};
use torchnexus_proxy_http::server::serve_http_proxy_listener;
use torchnexus_proxy_socks5::server::serve_authenticated_socks5_listener;
use torchnexus_proxy_tcp::context::{TcpForwardRuntimeConfig, TcpRuntimeContext};
use torchnexus_proxy_tcp::server::serve_tcp_forward_listener;
use torchnexus_storage_support::recorder::FileRecorder;
use torchnexus_uploader::client::RuntimeUploader;
use torchnexus_uploader::queue::UploadQueue;

/// A running capture agent, suitable for a CLI, desktop application, or mobile host.
pub struct AgentRuntime {
    shutdown: ShutdownSender,
    tasks: Vec<JoinHandle<anyhow::Result<()>>>,
    http_bind_addr: Option<SocketAddr>,
    socks5_bind_addr: Option<SocketAddr>,
    private_socks5_bind_addr: Option<SocketAddr>,
}

const TASK_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

impl AgentRuntime {
    pub async fn start(config: AppConfig) -> anyhow::Result<Self> {
        Self::start_with_private_socks5(config, None).await
    }

    /// Starts the configured public SOCKS5 listener and, when requested, an additional
    /// loopback-only listener for a device-local TUN implementation.
    pub async fn start_with_private_socks5(
        config: AppConfig,
        private_socks5_bind: Option<SocketAddr>,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        tracing::info!(
            capture_dir = %config.capture.save_dir,
            upload_enabled = config.upload.enabled,
            tcp_forward_count = config.listen.tcp.len(),
            "正在初始化采集代理运行时"
        );

        let filter = CaptureFilter::new(config.capture.targets.clone())?;
        let capture_root = PathBuf::from(&config.capture.save_dir);
        let recorder = FileRecorder::new(
            capture_root.clone(),
            config.capture.save_uncaptured_sessions,
            config.storage.flush_each_chunk,
        );
        let uploader = RuntimeUploader::from_config(&config.upload);
        let upload_queue = Arc::new(UploadQueue::new(
            capture_root.clone(),
            config.upload.clone(),
            uploader,
        ));
        let runtime = Arc::new(TcpRuntimeContext {
            filter,
            recorder,
            upload_queue,
        });
        runtime.ensure_capture_dirs().await?;

        let (shutdown_tx, shutdown) = ShutdownSignal::new();
        let mut tasks = Vec::new();

        if config.upload.enabled {
            tracing::info!(
                interval_seconds = config.upload.upload_interval_seconds,
                "已启用定时上传任务"
            );
            let runtime = Arc::clone(&runtime);
            let shutdown = shutdown.clone();
            let interval = std::time::Duration::from_secs(config.upload.upload_interval_seconds);
            tasks.push(tokio::spawn(async move {
                run_upload_scheduler(runtime, interval, shutdown).await
            }));
        }

        let mut socks5_bind_addr = None;
        if config.listen.socks5.enabled {
            let bind: SocketAddr =
                config.listen.socks5.bind.parse().with_context(|| {
                    format!("invalid socks5 bind {}", config.listen.socks5.bind)
                })?;
            let listener = TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to bind {bind}"))?;
            socks5_bind_addr = Some(listener.local_addr()?);
            tracing::info!(bind = %socks5_bind_addr.expect("listener address is set"), authenticated = config.listen.socks5.auth.is_some(), "SOCKS5 代理已开始监听");
            let runtime = Arc::clone(&runtime);
            let shutdown = shutdown.clone();
            let auth = config.listen.socks5.auth.clone();
            tasks.push(tokio::spawn(async move {
                serve_authenticated_socks5_listener(runtime, listener, bind, shutdown, auth).await
            }));
        }

        let mut http_bind_addr = None;
        if config.listen.http.enabled {
            let bind: SocketAddr =
                config.listen.http.bind.parse().with_context(|| {
                    format!("invalid http proxy bind {}", config.listen.http.bind)
                })?;
            let listener = TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to bind HTTP proxy {bind}"))?;
            http_bind_addr = Some(listener.local_addr()?);
            tracing::info!(bind = %http_bind_addr.expect("listener address is set"), authenticated = config.listen.http.auth.is_some(), "HTTP 代理已开始监听");
            let runtime = Arc::clone(&runtime);
            let shutdown = shutdown.clone();
            let auth = config.listen.http.auth.clone();
            tasks.push(tokio::spawn(async move {
                serve_http_proxy_listener(runtime, listener, bind, shutdown, auth).await
            }));
        }

        let mut private_socks5_bind_addr = None;
        if let Some(bind) = private_socks5_bind {
            let listener = TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to bind private socks5 listener {bind}"))?;
            private_socks5_bind_addr = Some(listener.local_addr()?);
            tracing::info!(bind = %private_socks5_bind_addr.expect("listener address is set"), "移动端内部 SOCKS5 代理已开始监听");
            let runtime = Arc::clone(&runtime);
            let shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                serve_authenticated_socks5_listener(runtime, listener, bind, shutdown, None).await
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
            let listener = TcpListener::bind(bind)
                .await
                .with_context(|| format!("failed to bind {bind}"))?;
            tracing::info!(name = %item.name, bind = %bind, remote = %remote, "TCP 转发已开始监听");
            let forward = TcpForwardRuntimeConfig {
                name: item.name.clone(),
                bind,
                remote,
            };
            let runtime = Arc::clone(&runtime);
            let shutdown = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                serve_tcp_forward_listener(listener, runtime, forward, shutdown).await
            }));
        }

        tracing::info!("采集代理运行时初始化完成");
        Ok(Self {
            shutdown: shutdown_tx,
            tasks,
            http_bind_addr,
            socks5_bind_addr,
            private_socks5_bind_addr,
        })
    }

    /// The actual HTTP proxy listener address, including an OS-assigned port.
    pub fn http_bind_addr(&self) -> Option<SocketAddr> {
        self.http_bind_addr
    }

    /// The actual SOCKS5 listener address, including an OS-assigned port.
    pub fn socks5_bind_addr(&self) -> Option<SocketAddr> {
        self.socks5_bind_addr
    }

    /// The device-local SOCKS5 listener reserved for a TUN forwarding implementation.
    pub fn private_socks5_bind_addr(&self) -> Option<SocketAddr> {
        self.private_socks5_bind_addr
    }

    pub async fn stop(self) -> anyhow::Result<()> {
        tracing::info!(task_count = self.tasks.len(), "正在停止采集代理运行时");
        self.shutdown.cancel();
        for mut task in self.tasks {
            match timeout(TASK_SHUTDOWN_GRACE, &mut task).await {
                Ok(result) => result??,
                Err(_) => {
                    // A SOCKS5 client may keep a capture connection open forever.
                    // Listener shutdown must still release its bound port so mobile
                    // VPN can be restarted.
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        tracing::info!("采集代理运行时已停止");
        Ok(())
    }
}

async fn run_upload_scheduler(
    runtime: Arc<TcpRuntimeContext<RuntimeUploader>>,
    interval: std::time::Duration,
    shutdown: ShutdownSignal,
) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if let Err(err) = runtime.upload_queue.upload_pending_once().await {
                    tracing::warn!(error = ?err, "定时上传任务执行失败");
                }
            }
        }
    }
}
