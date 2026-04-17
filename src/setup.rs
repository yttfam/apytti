use std::path::PathBuf;

use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

use crate::backend::BackendKind;
use crate::persist::PersistedConfig;

/// Run the interactive setup menu. Loads existing config, modifies it, saves back.
pub fn run(config_path: &PathBuf) -> anyhow::Result<()> {
    let mut cfg = PersistedConfig::load(config_path)?;
    let theme = ColorfulTheme::default();

    println!("apytti setup — configure your AI backends");
    println!("config file: {}", config_path.display());
    println!();

    loop {
        let mut items: Vec<String> = BackendKind::ALL
            .iter()
            .map(|k| {
                let bc = cfg.backend(*k);
                let active = if cfg.active == Some(*k) { " (active)" } else { "" };
                let enabled = if bc.enabled { "✓" } else { " " };
                format!("[{enabled}] {k}{active}")
            })
            .collect();

        let set_active_idx = items.len();
        items.push("Set active backend".into());
        let save_idx = items.len();
        items.push("Save and exit".into());
        let cancel_idx = items.len();
        items.push("Cancel".into());

        let choice = Select::with_theme(&theme)
            .with_prompt("What do you want to do?")
            .items(&items)
            .default(0)
            .interact()?;

        if choice == cancel_idx {
            println!("Cancelled.");
            return Ok(());
        }
        if choice == save_idx {
            cfg.save(config_path)?;
            println!("Saved to {}", config_path.display());
            return Ok(());
        }
        if choice == set_active_idx {
            let enabled: Vec<BackendKind> = BackendKind::ALL
                .iter()
                .copied()
                .filter(|k| cfg.backend(*k).enabled)
                .collect();
            if enabled.is_empty() {
                println!("Enable at least one backend first.");
                continue;
            }
            let labels: Vec<String> = enabled.iter().map(|k| k.to_string()).collect();
            let pick = Select::with_theme(&theme)
                .with_prompt("Active backend")
                .items(&labels)
                .default(0)
                .interact()?;
            cfg.active = Some(enabled[pick]);
            continue;
        }

        let kind = BackendKind::ALL[choice];
        configure_backend(&theme, &mut cfg, kind)?;
    }
}

fn configure_backend(
    theme: &ColorfulTheme,
    cfg: &mut PersistedConfig,
    kind: BackendKind,
) -> anyhow::Result<()> {
    let mut bc = cfg.backend(kind);

    println!();
    println!("Configuring {kind}");

    bc.enabled = Confirm::with_theme(theme)
        .with_prompt(format!("Enable {kind}?"))
        .default(bc.enabled)
        .interact()?;

    if !bc.enabled {
        cfg.set_backend(kind, bc);
        return Ok(());
    }

    bc.model = optional_input(theme, "Default model (blank for backend default)", bc.model.as_deref())?;

    if matches!(kind, BackendKind::Claude | BackendKind::Copilot) {
        bc.effort = optional_input(theme, "Default effort (low/medium/high/max)", bc.effort.as_deref())?;
    }

    if matches!(kind, BackendKind::Ollama) {
        bc.endpoint = optional_input(
            theme,
            "Endpoint URL (default: http://localhost:11434)",
            bc.endpoint.as_deref(),
        )?;
    } else {
        bc.skip_permissions = Confirm::with_theme(theme)
            .with_prompt("Skip permission prompts?")
            .default(bc.skip_permissions)
            .interact()?;
    }

    bc.dir = optional_input(theme, "Working directory (blank for cwd)", bc.dir.as_deref())?;

    cfg.set_backend(kind, bc);
    println!("{kind} configured.");
    Ok(())
}

fn optional_input(
    theme: &ColorfulTheme,
    prompt: &str,
    current: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let s: String = Input::with_theme(theme)
        .with_prompt(prompt)
        .default(current.unwrap_or("").to_string())
        .allow_empty(true)
        .interact_text()?;
    Ok(if s.is_empty() { None } else { Some(s) })
}
