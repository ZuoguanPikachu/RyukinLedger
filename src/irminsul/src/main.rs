//! RyukinLedger capture core (Irminsul).
//!
//! Records the player's Genshin Impact currency **income and expenses** by
//! reading the game's network traffic, and appends them to a ledger file:
//!
//!   - primogems (原石), mora (摩拉), genesis_crystals (创世结晶) come from the
//!     player properties packet,
//!   - intertwined_fate (纠缠之缘), acquaint_fate (相遇之缘) and
//!     masterless_starglitter (无主的星辉) come from the item store packet.
//!
//! Income and expense are stored as separate events, not as a net change.
//!
//! This binary has no user interface.  It is normally launched by the
//! RyukinLedger app (which runs it elevated), but it can also be started by
//! hand as administrator.  It must be running *before* the game finishes
//! logging in, otherwise the session key cannot be derived.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window on Windows in release

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod admin;
mod capture;
mod decoder_log;
mod ledger;
mod monitor;
mod process;
mod proto_wire;
mod resin;
mod status;
mod tracker;

/// Directory name used under %LOCALAPPDATA% (this process writes here).
const DATA_DIR_NAME: &str = "RyukinLedger";

/// Records Genshin Impact currency income and expenses to a ledger file.
///
/// Run this as administrator BEFORE launching the game, then log in: every
/// gain and spend of the recorded currencies is appended to `ledger.jsonl`.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Directory this process writes to (ledger.jsonl, status.json, log/).
    /// Defaults to %LOCALAPPDATA%\RyukinLedger, or to the RYUKINLEDGER_DATA_DIR
    /// environment variable when it is set.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Directory the app writes to (stop.request).  This process only reads it.
    /// When omitted, stop requests are not watched.
    #[arg(long, value_name = "DIR")]
    control_dir: Option<PathBuf>,

    /// Identity of this run, chosen by the app.  It is published in
    /// status.json, and a stop request only counts when it names this session.
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    /// Exit when this process id is gone: the app that launched us.  This is
    /// what keeps an elevated core from outliving the app that started it.
    #[arg(long, value_name = "PID")]
    watch_pid: Option<u32>,

    /// Skip the automatic elevation prompt.  Packet capture will fail unless
    /// the process already has the required privileges.
    #[arg(long)]
    no_admin: bool,

    /// Enable verbose protocol logging (handshake / key derivation details).
    #[arg(long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let data_dir = resolve_data_dir(args.data_dir)?;
    std::fs::create_dir_all(&data_dir).with_context(|| format!("create {}", data_dir.display()))?;

    tracing_init(&data_dir, args.verbose).context("failed to initialize logging")?;

    if !args.no_admin {
        admin::ensure_admin();
    }

    let mut monitor = monitor::Monitor::new(&data_dir, args.control_dir, args.session, args.watch_pid)?;
    monitor.run().await
}

/// The directory this process writes to: explicit argument first, then the
/// environment variable (which the app also honours), then %LOCALAPPDATA%.
///
/// Local rather than roaming on purpose: the ledger, the status file and the
/// logs are machine-local state, and a roaming profile would try to carry a file
/// that two machines could then append to at the same time.
fn resolve_data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        return Ok(dir);
    }
    if let Some(dir) = std::env::var_os("RYUKINLEDGER_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let local_appdata = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local_appdata).join(DATA_DIR_NAME))
}

fn tracing_init(data_dir: &std::path::Path, verbose: bool) -> Result<()> {
    let log_dir = data_dir.join("log");
    std::fs::create_dir_all(&log_dir).with_context(|| format!("create log dir {}", log_dir.display()))?;

    let appender = tracing_appender::rolling::Builder::new()
        .filename_prefix("irminsul")
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .max_log_files(7)
        .build(&log_dir)
        .with_context(|| format!("open log file in {}", log_dir.display()))?;
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    // The appender guard must live for the whole process; leak it on purpose.
    std::mem::forget(guard);

    // RUST_LOG overrides everything, otherwise --verbose enables the protocol
    // details that explain "found no data".
    let filter = match std::env::var("RUST_LOG") {
        Ok(filter) => filter,
        Err(_) if verbose => "auto_artifactarium=info,irminsul=info".to_string(),
        Err(_) => "warn,irminsul=info".to_string(),
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(EnvFilter::new(filter))
                .with_filter(decoder_log::DecoderTrouble),
        )
        .init();

    tracing::info!("irminsul {} starting, logging to {}", env!("CARGO_PKG_VERSION"), log_dir.display());
    Ok(())
}
