use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use polycrab::{
    bot::{self, BotKind},
    config::AppConfig,
    ui,
};
use std::io;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "polycrab")]
#[command(about = "PolyCrab Polymarket copy-trading toolkit.", long_about = None)]
#[command(version)]
struct Cli {
    /// Path to public config (JSON).
    #[arg(long, default_value = "config.json")]
    config: PathBuf,

    /// Path to credentials file (YAML).
    #[arg(long, default_value = "config.yaml")]
    credentials: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Launch the operator dashboard and select a bot. (Default if no subcommand.)
    Tui,
    /// Validate configuration and exit without connecting to external services.
    ValidateConfig,
    /// Run a specific bot headlessly (no TUI).
    Run {
        /// Which bot to run.
        #[arg(value_enum)]
        bot: BotKind,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let in_tui = matches!(cli.command, None | Some(Command::Tui));
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("polycrab=info,info"));
    if in_tui {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(io::sink)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("polycrab=info,info")),
            )
            .with_target(false)
            .with_writer(io::stderr)
            .init();
    }

    let cfg = AppConfig::load(&cli.config, &cli.credentials).context("loading configuration")?;

    info!(
        wallets = cfg.bot.wallets_to_track.len(),
        enable_trading = cfg.bot.enable_trading,
        mock_trading = cfg.bot.mock_trading,
        "configuration loaded"
    );

    // `validate-config` reports on a live configuration instead of refusing to
    // run, so an operator can check one before the gate is lifted. Every path
    // that could actually trade still goes through the gate first.
    if let Some(Command::ValidateConfig) = cli.command {
        println!(
            "configuration valid · mode={} · wallets={}",
            if cfg.live_trading_allowed() {
                "LIVE (disabled by safety gate)"
            } else {
                "SIMULATED"
            },
            cfg.bot.wallets_to_track.len()
        );
        return Ok(());
    }

    cfg.ensure_live_trading_ready()
        .context("validating live-trading safety gate")?;

    match cli.command {
        Some(Command::Run { bot: kind }) => bot::run(kind, cfg).await,
        Some(Command::Tui) | None => ui::run(cfg).await,
        Some(Command::ValidateConfig) => unreachable!("handled above"),
    }
}
