use tokio::signal;

/// Wait for SIGINT (Ctrl-C) or SIGTERM (Unix only).
///
/// Handler-install failures degrade **fail-safe** rather than hang. If a signal
/// handler cannot be installed, that branch resolves immediately (after logging)
/// instead of parking on [`std::future::pending`]. Otherwise `tokio::select!`
/// could be left waiting on two futures that will both never fire, which would
/// hang a headless deployment (Kubernetes/LB pod recycling via SIGTERM) whose
/// SIGTERM handler failed to install and where no human can send Ctrl-C — the
/// graceful drain would then never begin (audit GAP-LOW-15).
///
/// Resolving a failed branch is preferred over hanging: a process that cannot
/// install its primary shutdown signal is in a degraded state, and a supervisor
/// (systemd/k8s) will restart it cleanly.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        match signal::ctrl_c().await {
            Ok(()) => tracing::info!("received SIGINT (Ctrl-C)"),
            Err(error) => tracing::error!(
                %error,
                "failed to install Ctrl-C handler; beginning graceful shutdown"
            ),
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
                tracing::info!("received SIGTERM");
            }
            Err(error) => tracing::error!(
                %error,
                "failed to install SIGTERM handler; beginning graceful shutdown"
            ),
        }
        // An install failure falls through here (resolving the branch) instead of
        // parking on pending(), so tokio::select! below is never left waiting on
        // two futures that will both never fire (audit GAP-LOW-15).
    };

    #[cfg(not(unix))]
    let terminate = async {
        // No SIGTERM on non-Unix; Ctrl-C is the sole signal and always resolves
        // (Ok on signal, Err on install failure), so this branch is never the
        // sole deciding future and the pending park is harmless.
        std::future::pending::<()>().await;
    };

    tokio::select! {
        // Both branches are biased to resolve: a real signal OR an install
        // failure completes the select and lets graceful shutdown proceed.
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
