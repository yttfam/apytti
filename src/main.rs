use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing::info;

use apytti::config::{Cli, Command, RunArgs};
use apytti::handler::ServerState;
use apytti::persist::PersistedConfig;
use apytti::{install, registry, setup};

fn main() -> anyhow::Result<()> {
    // No #[tokio::main] — macOS NSApp must own the main thread for LaunchServices
    // registration + Local Network Privacy. Tokio runs on a worker for daemon mode;
    // for one-shot commands we build a runtime locally and block on it.

    // GUI launches (Finder, `open -a /Applications/Apytti.app`) inherit a minimal
    // PATH from LaunchServices that doesn't include /opt/homebrew/bin or
    // /usr/local/bin where `claude`, `copilot`, `gemini` typically live. Augment
    // the process PATH so spawned subprocesses can find them.
    augment_path();

    // Strip Finder-launched -psn_X_Y arg before clap parses argv
    let args: Vec<String> = std::env::args()
        .filter(|a| !a.starts_with("-psn_"))
        .collect();
    let cli = Cli::parse_from(args);

    let config_path = cli
        .config
        .clone()
        .or_else(PersistedConfig::default_path)
        .ok_or_else(|| anyhow::anyhow!("could not determine config path"))?;

    match cli.command.unwrap_or(Command::Run(RunArgs::default())) {
        Command::Run(args) => run_server(args, config_path),
        Command::Setup => setup::run(&config_path),
        Command::Install(args) => install::install(&args),
        Command::Uninstall => install::uninstall(),
        Command::Status => install::status(),
        Command::InitModels => block_on(init_models(config_path)),
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
    rt.block_on(f)
}

/// Prepend common shell-install dirs to $PATH if they're not already on it.
/// Necessary on macOS where Finder/LaunchServices launches inherit a stripped
/// PATH (`/usr/bin:/bin:/usr/sbin:/sbin`) that doesn't include /opt/homebrew/bin
/// or /usr/local/bin — which is where claude, copilot, gemini live after npm/brew.
fn augment_path() {
    let extras = [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/bin",
        "/usr/local/sbin",
    ];
    let current = std::env::var_os("PATH").unwrap_or_default();
    let current_str = current.to_string_lossy().into_owned();
    let mut prepend = Vec::new();
    for dir in extras {
        if !current_str.split(':').any(|p| p == dir) {
            prepend.push(dir);
        }
    }
    if prepend.is_empty() {
        return;
    }
    let new_path = format!("{}:{current_str}", prepend.join(":"));
    // SAFETY: single-threaded at startup; no other thread reads/writes env yet.
    unsafe {
        std::env::set_var("PATH", new_path);
    }
}

async fn init_models(config_path: PathBuf) -> anyhow::Result<()> {
    init_tracing("info");
    let config = PersistedConfig::load(&config_path)?;
    let cache_path = apytti::models::ModelsCache::path_for(&config_path);
    println!("Probing enabled backends... (this can take 10-30s)");
    let cache = apytti::models::init_all(&config, &cache_path).await;
    println!("Wrote {}", cache_path.display());
    println!();
    println!("{}", serde_json::to_string_pretty(&cache)?);
    Ok(())
}

fn init_tracing(level: &str) {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| level.into()))
        .try_init();
}

fn run_server(args: RunArgs, config_path: PathBuf) -> anyhow::Result<()> {
    init_tracing(if args.verbose { "debug" } else { "info" });

    let mut config = PersistedConfig::load(&config_path)?;

    // Env-var overrides (set by `apytti install` so daemons can register without editing config.toml)
    if config.hermytt.is_none() {
        if let Ok(url) = std::env::var("APYTTI_HERMYTT_URL") {
            config.hermytt = Some(apytti::HermyttConfig {
                url,
                token: std::env::var("APYTTI_HERMYTT_TOKEN").ok(),
                ..Default::default()
            });
        }
    }

    if config.active.is_none() && config.backends.is_empty() {
        eprintln!(
            "warning: no backend configured. Run `apytti setup` first.\n\
             Server will start but /api/ask will return errors until at least one backend is enabled.",
        );
    }

    let bind_addr = args.bind_addr().to_owned();
    let port = args.port;
    let state = Arc::new(ServerState::new(config.clone(), config_path.clone()));

    // macOS: wrap in NSApp menu-bar host so we get LaunchServices registration
    // + Local Network Privacy grant. Bypass with --no-menu for headless dev/test.
    #[cfg(target_os = "macos")]
    {
        if !args.no_menu {
            // Spawn the hermytt heartbeat from inside the worker (it needs the
            // tokio runtime). Pass via a helper that takes ServerState + the
            // hermytt config snapshot.
            return apytti::macos_menu::run(state, bind_addr, port, config_path);
        }
    }

    block_on(async move {
        if let Some(hermytt) = config.hermytt.clone() {
            let endpoint = registry::resolve_endpoint(&hermytt, port);
            let version = env!("CARGO_PKG_VERSION").to_string();
            tokio::spawn(registry::heartbeat_loop(hermytt, endpoint, version));
        }
        let app = apytti::build_router(state);
        let listener = tokio::net::TcpListener::bind((&*bind_addr, port))
            .await
            .unwrap_or_else(|e| {
                eprintln!("fatal: cannot bind to {bind_addr}:{port}: {e}");
                std::process::exit(1);
            });
        info!("apytti listening on {bind_addr}:{port} (config: {})", config_path.display());
        axum::serve(listener, app).await?;
        Ok::<_, anyhow::Error>(())
    })
}
