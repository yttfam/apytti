use std::sync::Arc;

use clap::Parser;
use tracing::info;

use apytti::config::{Cli, Command, RunArgs};
use apytti::handler::ServerState;
use apytti::persist::PersistedConfig;
use apytti::{install, setup};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config_path = cli
        .config
        .clone()
        .or_else(PersistedConfig::default_path)
        .ok_or_else(|| anyhow::anyhow!("could not determine config path"))?;

    match cli.command.unwrap_or(Command::Run(RunArgs::default())) {
        Command::Run(args) => run_server(args, config_path).await,
        Command::Setup => setup::run(&config_path),
        Command::Install(args) => install::install(&args),
        Command::Uninstall => install::uninstall(),
    }
}

async fn run_server(args: RunArgs, config_path: std::path::PathBuf) -> anyhow::Result<()> {
    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| log_level.into()),
        )
        .init();

    let config = PersistedConfig::load(&config_path)?;

    if config.active.is_none() && config.backends.is_empty() {
        eprintln!(
            "warning: no backend configured. Run `apytti setup` first.\n\
             Server will start but /api/ask will return errors until at least one backend is enabled.",
        );
    }

    let state = Arc::new(ServerState { config });
    let app = apytti::build_router(state);

    let bind_addr = args.bind_addr().to_owned();
    let port = args.port;

    let listener = tokio::net::TcpListener::bind((&*bind_addr, port))
        .await
        .unwrap_or_else(|e| {
            eprintln!("fatal: cannot bind to {bind_addr}:{port}: {e}");
            std::process::exit(1);
        });

    info!("apytti listening on {bind_addr}:{port} (config: {})", config_path.display());
    axum::serve(listener, app).await?;

    Ok(())
}
