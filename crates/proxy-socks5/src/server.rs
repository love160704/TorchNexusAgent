use crate::handler::{handle_socks5_connection, handle_socks5_connection_with_auth};
use anyhow::Context;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use torchnexus_core::config::Socks5AuthConfig;
use torchnexus_core::shutdown::ShutdownSignal;
use torchnexus_proxy_tcp::context::TcpRuntimeContext;
use torchnexus_uploader::client::PackageUploader;

pub async fn serve_socks5_listener_with_handler<H, Fut>(
    listener: TcpListener,
    bind: SocketAddr,
    shutdown: ShutdownSignal,
    handler: H,
) -> anyhow::Result<()>
where
    H: Fn(TcpStream, SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let handler = Arc::new(handler);
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (client, client_addr) = accept?;
                let handler = Arc::clone(&handler);
                tasks.spawn(async move {
                    if let Err(err) = handler(client, client_addr).await {
                        tracing::warn!(error = ?err, remote = %client_addr, "socks5 connection failed");
                    }
                });
            }
            _ = shutdown.cancelled() => {
                tracing::info!(bind = %bind, "socks5 listener stopping");
                break;
            }
        }
    }

    while let Some(result) = tasks.join_next().await {
        result.with_context(|| format!("socks5 connection task join failed for {bind}"))?;
    }

    Ok(())
}

pub async fn serve_socks5_listener<U>(
    runtime: Arc<TcpRuntimeContext<U>>,
    listener: TcpListener,
    bind: SocketAddr,
    shutdown: ShutdownSignal,
) -> anyhow::Result<()>
where
    U: PackageUploader + Send + Sync + 'static,
{
    serve_socks5_listener_with_handler(listener, bind, shutdown, move |client, client_addr| {
        let runtime = Arc::clone(&runtime);
        async move { handle_socks5_connection(client, client_addr, runtime).await }
    })
    .await
}

pub async fn serve_authenticated_socks5_listener<U>(
    runtime: Arc<TcpRuntimeContext<U>>,
    listener: TcpListener,
    bind: SocketAddr,
    shutdown: ShutdownSignal,
    auth: Option<Socks5AuthConfig>,
) -> anyhow::Result<()>
where
    U: PackageUploader + Send + Sync + 'static,
{
    serve_socks5_listener_with_handler(listener, bind, shutdown, move |client, client_addr| {
        let runtime = Arc::clone(&runtime);
        let auth = auth.clone();
        async move { handle_socks5_connection_with_auth(client, client_addr, runtime, auth).await }
    })
    .await
}

pub async fn run_socks5_server<U>(
    runtime: Arc<TcpRuntimeContext<U>>,
    bind: SocketAddr,
    shutdown: ShutdownSignal,
) -> anyhow::Result<()>
where
    U: PackageUploader + Send + Sync + 'static,
{
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;
    serve_socks5_listener(runtime, listener, bind, shutdown).await
}
