use tokio::signal;

/// Wait for SIGINT (Ctrl-C) or SIGTERM (Unix only).
///
/// Handler-install failures never panic: if Ctrl-C cannot be installed we log
/// the error and rely on SIGTERM (Unix); if SIGTERM cannot be installed we log
/// the error and rely on Ctrl-C. The function only resolves once a signal it
/// could actually install is received, so graceful shutdown still triggers via
/// whichever handler succeeded.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        match signal::ctrl_c().await {
            Ok(()) => {}
            Err(error) => {
                tracing::error!(
                    %error,
                    "failed to install Ctrl-C handler; relying on SIGTERM (Unix) only"
                );
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler; relying on Ctrl-C only");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}
