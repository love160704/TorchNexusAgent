use crate::context::{TcpForwardRuntimeConfig, TcpRuntimeContext};
use crate::forward::handle_tcp_connection;
use anyhow::Context;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use torchnexus_core::shutdown::ShutdownSignal;
use torchnexus_uploader::client::PackageUploader;

fn is_expected_disconnect_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| {
                matches!(
                    io_error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                )
            })
    })
}

pub async fn serve_tcp_forward_listener<U>(
    listener: TcpListener,
    runtime: Arc<TcpRuntimeContext<U>>,
    forward: TcpForwardRuntimeConfig,
    shutdown: ShutdownSignal,
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
                let forward = forward.clone();
                tasks.spawn(async move {
                    if let Err(err) = handle_tcp_connection(client, client_addr, runtime, forward).await {
                        if is_expected_disconnect_error(&err) {
                            tracing::debug!(
                                error = %err,
                                client = %client_addr,
                                "tcp forward connection closed by peer"
                            );
                        } else {
                            tracing::warn!(
                                error = ?err,
                                client = %client_addr,
                                "tcp forward connection failed"
                            );
                        }
                    }
                });
            }
            _ = shutdown.cancelled() => {
                tracing::info!(bind = %forward.bind, name = %forward.name, "tcp forward listener stopping");
                break;
            }
        }
    }

    while let Some(result) = tasks.join_next().await {
        result.with_context(|| {
            format!(
                "tcp forward connection task join failed for {}",
                forward.name
            )
        })?;
    }

    Ok(())
}

pub async fn run_tcp_forward_server<U>(
    runtime: Arc<TcpRuntimeContext<U>>,
    forward: TcpForwardRuntimeConfig,
    shutdown: ShutdownSignal,
) -> anyhow::Result<()>
where
    U: PackageUploader + Send + Sync + 'static,
{
    let listener = TcpListener::bind(forward.bind)
        .await
        .with_context(|| format!("failed to bind {}", forward.bind))?;

    serve_tcp_forward_listener(listener, runtime, forward, shutdown).await
}

#[cfg(test)]
mod tests {
    use super::is_expected_disconnect_error;
    use anyhow::Context as _;

    #[test]
    fn classifies_connection_reset_as_expected_disconnect() {
        let error = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        ));

        assert!(is_expected_disconnect_error(&error));
    }

    #[test]
    fn classifies_contextualized_broken_pipe_as_expected_disconnect() {
        let error = Err::<(), _>(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "pipe closed",
        ))
        .context("forwarding response")
        .unwrap_err();

        assert!(is_expected_disconnect_error(&error));
    }

    #[test]
    fn leaves_other_io_errors_as_unexpected() {
        let error = anyhow::Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "permission denied",
        ));

        assert!(!is_expected_disconnect_error(&error));
    }
}
