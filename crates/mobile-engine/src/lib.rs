#[cfg(any(target_os = "android", target_os = "ios"))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Mutex;
#[cfg(any(target_os = "android", target_os = "ios"))]
use std::thread::JoinHandle;

use torchnexus_core::config::AppConfig;
use torchnexus_runtime::AgentRuntime;

#[cfg(target_os = "android")]
fn init_platform_logging() {
    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("TorchNexus")
            .with_max_level(log::LevelFilter::Info),
    );
}

#[cfg(not(target_os = "android"))]
fn init_platform_logging() {}

#[derive(Debug, thiserror::Error)]
pub enum MobileEngineError {
    #[error("mobile engine is already running")]
    AlreadyRunning,
    #[error("mobile engine is not running")]
    NotRunning,
    #[error("invalid configuration: {detail}")]
    Configuration { detail: String },
    #[error("failed to start runtime: {detail}")]
    Runtime { detail: String },
    #[error("TUN forwarding is only available on Android and iOS")]
    UnsupportedPlatform,
}

pub struct MobileEngine {
    state: Mutex<Option<RunningEngine>>,
}

struct RunningEngine {
    runtime: tokio::runtime::Runtime,
    agent: AgentRuntime,
    tunnel: Tunnel,
}

struct Tunnel {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    join: JoinHandle<()>,
    #[cfg(any(target_os = "android", target_os = "ios"))]
    shutdown: tun2proxy::CancellationToken,
    #[cfg(any(target_os = "android", target_os = "ios"))]
    tun_fd: Option<OwnedFd>,
}

impl MobileEngine {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
        }
    }

    /// Starts the Rust capture agent and sends TUN traffic through its private SOCKS5 listener.
    pub fn start(
        &self,
        tun_fd: i32,
        close_tun_fd: bool,
        config_yaml: String,
        mtu: u16,
        packet_information: bool,
    ) -> Result<u16, MobileEngineError> {
        init_platform_logging();
        tracing::info!(mtu, packet_information, "正在启动移动端采集引擎");
        if !cfg!(any(target_os = "android", target_os = "ios")) {
            return Err(MobileEngineError::UnsupportedPlatform);
        }
        let mut state = self
            .state
            .lock()
            .expect("mobile engine state lock poisoned");
        if state.is_some() {
            return Err(MobileEngineError::AlreadyRunning);
        }

        let config = AppConfig::from_yaml_str(&config_yaml).map_err(|error| {
            MobileEngineError::Configuration {
                detail: error.to_string(),
            }
        })?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| MobileEngineError::Runtime {
                detail: error.to_string(),
            })?;
        let agent = runtime
            .block_on(AgentRuntime::start_with_private_socks5(
                config,
                Some(
                    "127.0.0.1:0"
                        .parse()
                        .expect("valid loopback socket address"),
                ),
            ))
            .map_err(|error| MobileEngineError::Runtime {
                detail: error.to_string(),
            })?;
        let port = agent
            .private_socks5_bind_addr()
            .expect("mobile engine always enables its private SOCKS5 listener")
            .port();

        let tunnel = start_tunnel(tun_fd, close_tun_fd, port, mtu, packet_information)?;
        *state = Some(RunningEngine {
            runtime,
            agent,
            tunnel,
        });
        tracing::info!(socks5_port = port, "移动端采集引擎已启动");
        Ok(port)
    }

    pub fn stop(&self) -> Result<(), MobileEngineError> {
        let running = self
            .state
            .lock()
            .expect("mobile engine state lock poisoned")
            .take()
            .ok_or(MobileEngineError::NotRunning)?;
        tracing::info!("正在停止移动端采集引擎");
        // stop_tunnel cancels and closes the TUN fd but reaps its thread in the
        // background, so the Android VPN disconnect begins immediately without
        // blocking listener shutdown or a later restart.
        stop_tunnel(running.tunnel);
        let agent_stop_result = running
            .runtime
            .block_on(running.agent.stop())
            .map_err(|error| MobileEngineError::Runtime {
                detail: error.to_string(),
            });
        if agent_stop_result.is_ok() {
            tracing::info!("移动端采集引擎已停止");
        }
        agent_stop_result
    }
}

uniffi::include_scaffolding!("torchnexus_mobile_engine");

#[cfg(any(target_os = "android", target_os = "ios"))]
fn start_tunnel(
    tun_fd: i32,
    close_tun_fd: bool,
    port: u16,
    mtu: u16,
    packet_information: bool,
) -> Result<Tunnel, MobileEngineError> {
    // tun2proxy borrows this raw descriptor. When ownership was transferred to us,
    // keep it here so stop_tunnel can close it before waiting for the forwarding
    // thread to exit.
    let owned_tun_fd = close_tun_fd.then(|| unsafe { OwnedFd::from_raw_fd(tun_fd) });
    let mut args = tun2proxy::Args::default();
    args.proxy = tun2proxy::ArgProxy::try_from(format!("socks5://127.0.0.1:{port}").as_str())
        .map_err(|error| MobileEngineError::Runtime {
            detail: error.to_string(),
        })?;
    args.tun_fd = Some(
        owned_tun_fd
            .as_ref()
            .map(AsRawFd::as_raw_fd)
            .unwrap_or(tun_fd),
    );
    // The engine owns the fd rather than tun2proxy: dropping it first on shutdown
    // wakes a task blocked in a TUN read and avoids an Android service-destroy ANR.
    args.close_fd_on_drop = Some(false);
    args.setup = false;

    let shutdown = tun2proxy::CancellationToken::new();
    let tunnel_shutdown = shutdown.clone();
    let join = std::thread::spawn(move || {
        tracing::info!("TUN 流量转发线程已启动");
        // tun2proxy closes TCP streams with `tokio::task::block_in_place`. That API
        // panics on Tokio's current-thread runtime, so the tunnel must run on a
        // multi-thread runtime even though it is hosted by its own OS thread.
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::error!(%error, "创建 TUN 转发运行时失败");
                return;
            }
        };
        if let Err(error) = runtime.block_on(tun2proxy::general_run_async(
            args,
            mtu,
            packet_information,
            tunnel_shutdown,
        )) {
            tracing::error!(%error, "TUN 转发异常停止");
        } else {
            tracing::info!("TUN 流量转发线程已停止");
        }
    });
    Ok(Tunnel {
        join,
        shutdown,
        tun_fd: owned_tun_fd,
    })
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn start_tunnel(_: i32, _: bool, _: u16, _: u16, _: bool) -> Result<Tunnel, MobileEngineError> {
    Err(MobileEngineError::UnsupportedPlatform)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn stop_tunnel(tunnel: Tunnel) {
    tunnel.shutdown.cancel();
    drop(tunnel.tun_fd);
    // Closing the fd normally wakes tun2proxy immediately. Reap the OS thread in
    // the background so an edge case in its TUN read cannot block a VPN restart.
    std::thread::spawn(move || {
        let _ = tunnel.join.join();
    });
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn stop_tunnel(_: Tunnel) {
    unreachable!("a non-mobile build cannot start a tunnel")
}
