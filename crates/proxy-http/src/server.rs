use crate::handler::handle_http_proxy_connection;
use anyhow::Context;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use torchnexus_core::config::HttpProxyAuthConfig;
use torchnexus_core::shutdown::ShutdownSignal;
use torchnexus_proxy_tcp::context::TcpRuntimeContext;
use torchnexus_uploader::client::PackageUploader;

pub async fn serve_http_proxy_listener<U>(
    runtime: Arc<TcpRuntimeContext<U>>,
    listener: TcpListener,
    bind: SocketAddr,
    shutdown: ShutdownSignal,
    auth: Option<HttpProxyAuthConfig>,
) -> anyhow::Result<()>
where
    U: PackageUploader + Send + Sync + 'static,
{
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (client, client_addr) = accept?;
                let runtime = Arc::clone(&runtime);
                let auth = auth.clone();
                tasks.spawn(async move {
                    if let Err(error) = handle_http_proxy_connection(client, client_addr, runtime, auth.as_ref()).await {
                        tracing::warn!(?error, remote = %client_addr, "http proxy connection failed");
                    }
                });
            }
            _ = shutdown.cancelled() => {
                tracing::info!(%bind, "http proxy listener stopping");
                break;
            }
        }
    }

    while let Some(result) = tasks.join_next().await {
        result.with_context(|| format!("http proxy connection task join failed for {bind}"))?;
    }
    Ok(())
}
