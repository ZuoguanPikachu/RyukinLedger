//! The channel from the capture core to the RyukinLedger app.
//!
//! The core is started by the app with the `runas` verb so that packet capture
//! has the privileges it needs, and Windows does not let an elevated process
//! inherit redirected stdio.  stdout is therefore unusable as an IPC channel.
//!
//! Instead the two sides communicate through files, and the directories are
//! split by *who owns what*, because Windows blocks writes "upwards" in
//! integrity level: an object created by this elevated process cannot be
//! written by the unelevated app, but the elevated process can always read what
//! the app created.
//!
//! - the **data directory** (this process writes, the app only reads):
//!   `status.json` is rewritten at least once a second, which doubles as the
//!   heartbeat that tells the app whether the core is alive;
//! - the **control directory** (the app writes, this process only reads):
//!   `stop.request` asks for a clean shutdown.
//!
//! Ownership is what makes the split necessary: whichever side creates a
//! directory can write in it forever, and the other side can read it forever.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

pub const STATUS_FILE: &str = "status.json";

/// Name of the stop request inside the app's control directory.
pub const STOP_FILE: &str = "stop.request";

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Capture is running but the session key has not been derived yet: the
    /// game has to be started (or logged into again) after this program.
    WaitingForHandshake,
    /// The session key works and packets are being decoded.
    Receiving,
    /// Real game data has arrived during this run and balances are being
    /// tracked.  Seeded balances from an earlier run do *not* count: the state
    /// has to describe this run, not the ledger behind it.
    Tracking,
    /// Capture could not start, or stopped unexpectedly.
    Error,
    /// Clean shutdown.
    Stopped,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub app: &'static str,
    pub version: &'static str,
    pub pid: u32,
    pub started_at: String,
    pub updated_at: String,
    pub state: State,
    /// Identity of this run, chosen by the app that launched it.  The app uses
    /// it to tell its own core apart from a status left by an earlier one, and
    /// to address a stop request at the right process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// currency key -> balance.  Before this run has seen any data these are
    /// the last values from the ledger, which is useful but not "live".
    pub balances: BTreeMap<&'static str, i64>,
    /// Every recorded currency has a known balance (possibly from the ledger).
    pub complete: bool,
    /// Whether *this* run has received game data yet.
    pub session_data: bool,
    /// How often the game's connection was replaced after this run had already
    /// been recording.  Anything that happened in those windows could not be
    /// recorded, which is what the interface explains when the core asks for a
    /// login the player has already done.
    pub reconnects: u64,
    /// Whether the game process is running, so the app can tell "start the
    /// game" apart from "the game is running but the handshake was missed".
    pub game_running: bool,
    /// Income/expense events recorded by this run.
    pub transactions: u64,
}

/// Write the status file atomically, so a reader never sees a half-written
/// file.
pub fn write(dir: &Path, status: &Status) -> Result<()> {
    let path = dir.join(STATUS_FILE);
    let tmp = dir.join("status.json.tmp");
    let json = serde_json::to_vec_pretty(status).context("serialize status")?;
    std::fs::write(&tmp, &json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Consume a stop request addressed to this session.
///
/// The file has to contain this run's session id.  A leftover file from an
/// earlier session therefore cannot stop a fresh capture -- which it once did:
/// every start died about 35 ms later because a stale `stop.request` was still
/// in the control directory.
///
/// The session id is used instead of the file's timestamp because file
/// modification times are quantised by the file system, which made "was this
/// written after we started?" unreliable in both directions.  An id comparison
/// has no such problem, and it also tells two running cores apart.
pub fn take_stop_request(control_dir: &Path, session: &str) -> bool {
    let path = control_dir.join(STOP_FILE);

    // Never read more than a token's worth: the file is written by another
    // process and its contents are only used for this comparison.
    let requested = std::fs::read(&path)
        .ok()
        .filter(|contents| contents.len() <= 128)
        .and_then(|contents| String::from_utf8(contents).ok())
        .is_some_and(|contents| contents.trim() == session);

    if requested {
        // The request has been acted on; removing it keeps the directory clean.
        // Best effort only -- the id check is what guarantees correctness.
        let _ = std::fs::remove_file(&path);
    }

    requested
}
