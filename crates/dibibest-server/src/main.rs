use dibibest_server::config::Config;
use dibibest_server::{bind, shutdown_signal};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let server = match bind(config).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(addr = %server.local_addr(), "listening");
    match server.run(shutdown_signal()).await {
        Ok(()) => {
            tracing::info!("stopped");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("server error: {e}");
            ExitCode::FAILURE
        }
    }
}
