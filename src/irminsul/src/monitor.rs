//! Packet capture loop: decode game traffic, track balances, write the ledger.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use auto_artifactarium::r#gen::protos::PacketHead;
use auto_artifactarium::{ConnectionPacket, GameCommand, GamePacket, GameSniffer};
use base64::prelude::*;
use chrono::Local;

use crate::capture::{self, CaptureError};
use crate::ledger::{Change, Ledger, now};
use crate::proto_wire;
use crate::status::{self, State, Status};
use crate::tracker::BalanceTracker;

/// How often the status heartbeat is refreshed and the stop request checked.
const TICK: Duration = Duration::from_secs(1);

/// Command id of `PlayerStoreNotify`, the packet that enumerates the whole
/// inventory.
///
/// It is private inside `auto-artifactarium`, and the game moved it with the
/// 7.1 update (it was 8132 in 7.0).  The pinned revision of that crate still
/// looks for the old id *and* the old field number for the items inside it, so
/// it finds nothing in a 7.1 packet -- while our own wire parser reads the items
/// out of it either way.  That is why this program recognises the packet by id
/// itself; see `handle_command`.
///
/// **When the game updates and this stops matching**, upstream's `Update for
/// 7.x` commit in <https://github.com/konkers/auto-artifactarium> contains the
/// new number (it is a one-line change to the `CommandId` enum).  The warning in
/// `report_missing_store_sync` is what tells you it is time to look.
const PLAYER_STORE_NOTIFY: u16 = 22160;

/// How long after the first game data to wait for a store sync before saying
/// that none arrived.  The real one lands milliseconds after login.
const STORE_SYNC_GRACE: Duration = Duration::from_secs(30);

/// How many packets the decoder may fail to decrypt, without decoding anything
/// in between, before the connection state is thrown away.
///
/// A packet that does not fit the key is the decoder's own verdict, and the only
/// one worth acting on: it is reported per packet that really was undecryptable,
/// which is what "a packet produced no command" is not -- acknowledgements,
/// duplicates and segments the KCP receive window refuses all produce that empty
/// batch too, and treating them as failure once dropped a healthy connection
/// in the middle of a login.
///
/// Two very different situations arrive here, and both are hopeless until the
/// player logs in again.  A session key that no longer fits: the decoder pays for
/// a brute-force search over the seeds of the session it remembers on every
/// following packet -- 3000 candidate seeds, five keys each -- which was 2835
/// searches over 14 minutes in the run that prompted this.  And a connection
/// whose key material was never obtained at all, where nothing is searched
/// because there is nothing to search: the core was started under an established
/// session, or an in-game reconnect left it holding traffic it cannot read.
///
/// A working connection produces none of either -- a login decodes its first
/// packet with the key it just derived -- so thirty is far above anything
/// healthy, and small enough to stop the waste within seconds.
const UNREADABLE_LIMIT: u64 = 30;

pub struct Monitor {
    data_dir: PathBuf,
    /// Directory the app writes to; this process only reads `stop.request` from
    /// it.  `None` means "nobody can ask us to stop except Ctrl-C".
    control_dir: Option<PathBuf>,
    /// Identity of this run, chosen by the app.  A stop request must name it to
    /// be honoured, so a request left over from an earlier run is ignored.
    session: Option<String>,
    /// The app's process id: when it is gone, so is the reason to keep running.
    watch_pid: Option<u32>,
    /// The dispatch (version) keys, kept out here because the sniffer has to be
    /// rebuilt from scratch when a connection ends -- the session key and the
    /// KCP receive state live inside it, and there is no API to clear them.
    keys: HashMap<u16, Vec<u8>>,
    sniffer: GameSniffer,
    tracker: BalanceTracker,
    ledger: Ledger,
    started_at: String,
    state: State,
    error: Option<String>,
    /// The session key has been derived, so commands are being decoded.
    handshake: bool,
    /// Real game data has arrived during this run.  Balances seeded from the
    /// ledger must not be mistaken for live capture.
    session_data: bool,
    /// A command has been decoded at some point during this connection, and the
    /// connection state has not been dropped since.  Only a connection that has
    /// proved it could decode is worth giving up on; one that never decoded
    /// anything has simply not started yet.
    decoded_a_command: bool,
    /// Packets the decoder has failed to decrypt since the last command it
    /// decoded.  See `UNREADABLE_LIMIT`.
    unreadable: u64,
    /// Of those, how many also cost a brute-force search over the seeds.  Kept
    /// apart from the total because the difference is the diagnosis: no searches
    /// at all means the key material was never obtained.
    lost_searches: u64,
    /// The values `decoder_log` had when they were last read, which is what turns
    /// its running totals into "how many since the last packet".
    unreadable_seen: u64,
    lost_searches_seen: u64,
    /// When the first command of this connection decoded, used to time the
    /// check for a login that never produced any game data.
    first_command_at: Option<Instant>,
    /// The stalled-login warning has been emitted for this connection.
    stall_reported: bool,
    /// How often this run had to give up on a connection because the decoder
    /// could no longer read it.  Anything that happened while it was blind could
    /// not be recorded, which is what the interface explains; a connection the
    /// game merely replaced -- leaving co-op, say -- is not counted, because
    /// nothing is missed there.
    reconnects: u64,
    /// When the first game data of this run arrived, used to time the check for
    /// a missing store sync.
    first_data_at: Option<Instant>,
    /// The full inventory has been enumerated during this run.
    store_sync_seen: bool,
    /// The missing-store-sync warning has been emitted (at most once per run).
    missing_store_reported: bool,
    /// How many commands of each id this run has decoded.  Only used to explain
    /// a missing store sync.
    commands_seen: BTreeMap<u16, u64>,
}

impl Monitor {
    pub fn new(
        data_dir: &Path,
        control_dir: Option<PathBuf>,
        session: Option<String>,
        watch_pid: Option<u32>,
    ) -> Result<Self> {
        let keys = load_keys()?;
        let (mut ledger, known) = Ledger::open(data_dir)?;

        // The balances the ledger already has seed the tracker, so a change made
        // while this program was closed is recognised as a gap rather than
        // mistaken for income.
        //
        // Nothing is invented here.  A currency nobody has observed yet simply
        // has no value, and the interface shows "unknown" for it until the game
        // says otherwise -- which is the truth, and the same truth for all six
        // currencies: the props arrive in the login snapshot (zero included),
        // while a wish currency sitting at zero is mentioned by no packet at all
        // until the store sync enumerates the inventory.
        ledger.session_started()?;

        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            control_dir,
            session,
            watch_pid,
            keys: keys.clone(),
            sniffer: GameSniffer::new().set_initial_keys(keys),
            tracker: BalanceTracker::new(known),
            ledger,
            started_at: now(),
            state: State::WaitingForHandshake,
            error: None,
            handshake: false,
            session_data: false,
            decoded_a_command: false,
            unreadable: 0,
            lost_searches: 0,
            unreadable_seen: 0,
            lost_searches_seen: 0,
            first_command_at: None,
            stall_reported: false,
            reconnects: 0,
            first_data_at: None,
            store_sync_seen: false,
            missing_store_reported: false,
            commands_seen: BTreeMap::new(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        // Checked before capture starts as well as on every heartbeat: the app
        // may have asked to stop while the elevation prompt was still up, and an
        // app that is already gone means there is nobody left to report to.
        if let Some(reason) = self.stop_reason() {
            tracing::info!("stopping before capture started ({reason})");
            self.finish(reason)?;
            return Ok(());
        }

        let mut capture = match capture::create_capture() {
            Ok(capture) => capture,
            Err(e) => {
                // The app talks to us only through status.json, so the failure
                // has to be published before we give up.  The overwhelmingly
                // common cause is a missing elevation, so say so.
                let message = format!(
                    "could not start packet capture (this program has to run as administrator): {e}"
                );
                self.fail(message.clone()).ok();
                return Err(anyhow!("{message}"));
            }
        };
        tracing::info!(
            "capturing Genshin traffic; start the game now and log in (data directory: {})",
            self.data_dir.display()
        );
        self.publish()?;

        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let packet = tokio::select! {
                packet = capture.next_packet() => packet,
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("interrupted, stopping");
                    self.finish("interrupted")?;
                    return Ok(());
                }
                _ = tick.tick() => {
                    // Both ways the app can end this run are checked on the
                    // heartbeat, so neither needs any privilege.
                    if let Some(reason) = self.stop_reason() {
                        tracing::info!("stopping ({reason})");
                        self.finish(reason)?;
                        return Ok(());
                    }
                    // The heartbeat is also the only place a *missing* packet can
                    // be noticed: nothing arrives to trigger it.
                    self.report_missing_store_sync();
                    self.report_stalled_login();
                    // Heartbeat: also proves to the app that we are alive.
                    self.publish()?;
                    continue;
                }
            };

            let packet = match packet {
                Ok(packet) => packet,
                Err(CaptureError::CaptureClosed) => {
                    self.fail("the packet capture session ended".to_string())?;
                    return Err(anyhow!("packet capture session ended"));
                }
                Err(e) => {
                    tracing::error!("capture error: {e}");
                    // Do not spin on a persistent error.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };

            if let Err(e) = self.handle_packet(packet) {
                tracing::error!("failed to handle packet: {e:#}");
            }
        }
    }

    /// Why this run should stop right now, if it should.
    ///
    /// Both signals come from the app and neither needs any privilege: a stop
    /// request that names this session, and the app's own process still
    /// existing.
    fn stop_reason(&self) -> Option<&'static str> {
        if let (Some(control_dir), Some(session)) = (&self.control_dir, &self.session)
            && status::take_stop_request(control_dir, session)
        {
            return Some("stop-request");
        }

        if let Some(pid) = self.watch_pid
            && !crate::process::is_running(pid)
        {
            return Some("app-exited");
        }

        None
    }

    fn handle_packet(&mut self, packet: Vec<u8>) -> Result<()> {
        let Some(parsed) = self.sniffer.receive_packet(packet) else {
            return Ok(());
        };

        // Read after the decoder has run: what it could not read on this packet
        // is part of the totals by now, because they are counted while its
        // complaints are on their way to the log.
        let unreadable = self.count_unreadable();

        match parsed {
            GamePacket::Connection(ConnectionPacket::HandshakeRequested) => {
                // The player is logging in again.  Upstream drops the session
                // key and both KCP receive states here; what it deliberately
                // keeps -- the seeds, the send time and the client seed it
                // derived -- is what lets it recognise the same session again
                // when the game only replaced its connection rather than its
                // login.  Only the bookkeeping below describes the old
                // connection, so only that is cleared.
                tracing::info!("handshake requested; deriving a new session key");
                self.forget_connection_progress();
                self.publish()?;
            }
            GamePacket::Connection(ConnectionPacket::Disconnected) => {
                // A connection ended.  That is *not* the same as a session
                // ending, and pretending it is cost more than it bought: the
                // game replaces its connection for ordinary reasons -- leaving
                // co-op, or leaving the Serenitea Pot, which counts as co-op --
                // and the log shows what that looks like from here: nine of
                // these, then a fresh handshake eleven milliseconds later.
                //
                // The key, the seeds and the client seed survive that
                // replacement, and the decoder needs them: with its client seed
                // gone it can only search around the new token response's send
                // time, which is what failed for thirty packets in that run
                // while recording sat still.  So this only says what happened,
                // and leaving the connection state alone is what makes the next
                // connection readable.  A connection that really is gone is
                // caught by the packets that cannot be read -- see
                // `note_unreadable` -- rather than by this event.
                tracing::warn!("the game's connection ended");
                self.forget_connection_progress();
                self.publish()?;
            }
            GamePacket::Connection(_) => {}
            GamePacket::Commands(commands) => {
                if commands.is_empty() {
                    // Nothing decoded.  On its own that says nothing -- see
                    // `UNREADABLE_LIMIT` -- so the only thing counted here is
                    // what the decoder itself reported failing on.
                    self.note_unreadable(unreadable)?;
                    return Ok(());
                }

                // A command decoded, so the key fits this connection again.
                self.unreadable = 0;
                self.lost_searches = 0;
                self.handshake = true;
                for command in &commands {
                    self.handle_command(command)?;
                }
            }
        }
        Ok(())
    }

    /// Count a connection that ended while this run was recording.
    ///
    /// The first login of a run is not a reconnect: nothing had been recorded
    /// yet, so nothing can have been missed.  Counting it only while data is
    /// there also collapses the bursts the game sends -- four 404s and five
    /// handshake packets can arrive in the same millisecond -- into one event,
    /// because the first one clears the flag.
    ///
    /// Only a connection that was *given up on* counts, because that is the
    /// only case in which data can have been missed: the connection events
    /// themselves are ordinary, and saying "data may be missing, log in again"
    /// every time the player leaves co-op is how a warning stops being read.
    fn note_reconnect(&mut self) {
        if self.session_data {
            self.reconnects += 1;
        }
    }

    /// Give up on a connection the decoder can no longer read.
    ///
    /// The decoder keeps its session key, its seeds and its KCP receive window
    /// inside itself, and it repairs a key that no longer fits by searching the
    /// seeds of the session it remembers.  That search cannot succeed once the
    /// game has moved on, and it is not cheap: 3000 candidate seeds, five keys
    /// each, for every seed, on *every* following packet.  A connection like
    /// that, or one whose key material was never obtained in the first place, is
    /// not coming back on its own -- only a fresh login brings the packet that
    /// carries the seeds.
    ///
    /// Dropping the state is what makes that login work.  It leaves the decoder
    /// exactly as a restarted core leaves it, so the next login decodes the way
    /// it does after a restart.  With the seeds gone there is nothing left to
    /// search either, so the packets still arriving on the dead connection cost
    /// nothing until then -- and the interface stops claiming to be waiting for
    /// data it can never read.
    fn note_unreadable(&mut self, unreadable: u64) -> Result<()> {
        // Nothing has ever decoded: there is no key to give up on, and the
        // packets of a login that has not reached its first command yet must not
        // throw away a state that is about to be used.
        if !self.decoded_a_command {
            return Ok(());
        }

        self.unreadable += unreadable;
        if self.unreadable < UNREADABLE_LIMIT {
            return Ok(());
        }

        tracing::warn!(
            "the decoder could not read {UNREADABLE_LIMIT} packets in a row ({lost} of them cost a \
             search over the seeds) without decoding anything; the connection state is dropped.  \
             A new login is needed to record again",
            lost = self.lost_searches
        );
        self.note_reconnect();
        self.reset_capture_state();
        self.decoded_a_command = false;
        self.publish()
    }

    /// How many packets the decoder has failed to decrypt since the last packet
    /// this run looked at, and how many of those cost a search over the seeds.
    fn count_unreadable(&mut self) -> u64 {
        let packets = crate::decoder_log::unreadable_packets();
        let searches = crate::decoder_log::lost_searches();

        let unreadable = packets.saturating_sub(self.unreadable_seen);
        self.lost_searches += searches.saturating_sub(self.lost_searches_seen);

        self.unreadable_seen = packets;
        self.lost_searches_seen = searches;
        unreadable
    }

    /// Forget what the run knows about the connection that just ended.
    ///
    /// These flags all describe a live connection: whether handshake traffic has
    /// been decoded, whether real game data has arrived, and the timers behind
    /// the two "nothing arrived" warnings.  The balances are *not* part of this
    /// -- they belong to the ledger and survive -- and neither is what the run
    /// has learned about the *account*: a store sync enumerates the inventory
    /// once per login, and the connection the game replaced eleven milliseconds
    /// after leaving co-op is not a new login that owes us another one.
    fn forget_connection_progress(&mut self) {
        self.handshake = false;
        self.session_data = false;
        self.unreadable = 0;
        self.lost_searches = 0;
        self.first_command_at = None;
        self.stall_reported = false;
        self.first_data_at = None;
    }

    /// Put the capture back into the state a freshly started process is in.
    ///
    /// Rebuilding the sniffer is the only way to get there: the session key, its
    /// seeds, the client seed and the KCP state all live inside it, and it
    /// exposes no way to clear any of them.  It is therefore a last resort, used
    /// only when the packets have proved that the decoder cannot read the
    /// connection it is holding -- never on a connection event, which the game
    /// sends for ordinary reasons.
    fn reset_capture_state(&mut self) {
        self.sniffer = GameSniffer::new().set_initial_keys(self.keys.clone());
        self.forget_connection_progress();
    }

    fn handle_command(&mut self, command: &GameCommand) -> Result<()> {
        // A command that decoded proves the session key fits this connection,
        // which is what makes a later run of lost searches mean the connection
        // has ended rather than not yet started.
        self.decoded_a_command = true;
        self.unreadable = 0;
        self.lost_searches = 0;
        self.first_command_at.get_or_insert_with(Instant::now);

        *self.commands_seen.entry(command.command_id).or_insert(0) += 1;

        // What the game sent and when it says it sent it.  `sent_ms` is the
        // clock a client seeds its stream keys from, so when a connection is
        // replaced and the decoder can no longer derive the session key, these
        // times are what tells "the search window is in the wrong place" apart
        // from "the key material is gone" -- the two need different repairs and
        // look identical from outside.  Off by default; `RUST_LOG=irminsul=debug`
        // turns it on, and the parse is skipped entirely unless it is on.
        if tracing::enabled!(tracing::Level::DEBUG)
            && let Ok(head) = command.parse_proto::<PacketHead>()
        {
            tracing::debug!(command_id = command.command_id, sent_ms = head.sent_ms, "decoded command");
        }

        // The store sync is recognised by its command id, and its items are read
        // with our own wire parser rather than the generated protobuf type.  The
        // 7.1 game update moved both the id and the field number the items live
        // in, and the pinned decoder knows neither, so asking it would silently
        // find nothing -- which is exactly how this packet came to be handled as
        // an ordinary item change for months.
        if command.command_id == PLAYER_STORE_NOTIFY
            && let Some(items) = proto_wire::extract_item_changes(&command.proto_data)
        {
            tracing::info!("store sync: {} items", items.len());
            self.session_data = true;
            self.store_sync_seen = true;
            let changes = self.tracker.snapshot_items(&items);
            return self.apply(changes);
        }

        // Each arm produces its changes before touching the ledger, because
        // `apply` needs `&mut self` while the tracker is borrowed.  Every arm
        // that recognises real data marks the session as having seen some: a
        // snapshot counts even when it matches what we already knew, otherwise a
        // restart under an unchanged account would never look "tracking".
        if let Some(packet) = proto_wire::extract_player_packet(&command.proto_data) {
            self.session_data = true;
            self.note_first_data();
            let changes = self.tracker.snapshot_props(&packet.props);
            if let Some(nickname) = packet.nick_name
                && let Some(nickname) = self.tracker.set_nickname(nickname)
            {
                tracing::info!("player identified as {nickname:?}");
                self.ledger.session_identified(&nickname)?;
            }
            return self.apply(changes);
        }
        if let Some(props) = proto_wire::extract_prop_updates(&command.proto_data) {
            self.session_data = true;
            self.note_first_data();
            let changes = self.tracker.live_prop_updates(&props);
            return self.apply(changes);
        }
        if let Some(items) = proto_wire::extract_item_changes(&command.proto_data) {
            self.session_data = true;
            self.note_first_data();
            let changes = self.tracker.live_item_changes(&items);
            return self.apply(changes);
        }
        if let Some(guids) = proto_wire::extract_removed_item_guids(&command.proto_data) {
            self.session_data = true;
            self.note_first_data();
            let changes = self.tracker.live_item_removals(&guids);
            return self.apply(changes);
        }
        Ok(())
    }

    fn note_first_data(&mut self) {
        self.first_data_at.get_or_insert_with(Instant::now);
    }

    /// Say so, once, when the inventory was never enumerated.
    ///
    /// Without the store sync there is no way to learn that a wish currency is
    /// empty (the game never mentions a zero-count item anywhere else), so those
    /// balances stay unknown and the interface shows "unknown" for them.  That
    /// is the visible symptom; this warning is the diagnosis, and the histogram
    /// is what makes it actionable -- when the game moves a command id, the new
    /// one is right there in the list.
    fn report_missing_store_sync(&mut self) {
        if self.store_sync_seen || self.missing_store_reported {
            return;
        }

        let Some(first) = self.first_data_at else {
            return;
        };

        if first.elapsed() < STORE_SYNC_GRACE {
            return;
        }

        self.missing_store_reported = true;

        let histogram = self.command_histogram();

        tracing::warn!(
            "no store sync (command {PLAYER_STORE_NOTIFY}) seen in {}s; item balances cannot be \
             known.  Commands seen: {histogram}",
            STORE_SYNC_GRACE.as_secs()
        );
    }

    /// Say so, once, when the decoder reads commands but no game data follows.
    ///
    /// This is the one state the run cannot otherwise describe.  "Handshake
    /// done, waiting for game data" on the interface means exactly one thing --
    /// a command decoded -- and says nothing about why nothing did afterwards,
    /// which is the difference between a login that never finished, a login
    /// whose key could not be derived, and a decoder that no longer recognises
    /// the game's packets at all.  The command ids tell those apart: a game
    /// update that moves one shows up here as the new id sitting in the list.
    ///
    /// Deliberately only logged: a clock may decide what to *say*, never what to
    /// throw away.
    fn report_stalled_login(&mut self) {
        if self.session_data || self.stall_reported {
            return;
        }

        let Some(first) = self.first_command_at else {
            return;
        };

        if first.elapsed() < STORE_SYNC_GRACE {
            return;
        }

        self.stall_reported = true;

        let decoded: u64 = self.commands_seen.values().sum();
        let histogram = self.command_histogram();

        tracing::warn!(
            "no game data {}s after the first decoded command; {decoded} command(s) decoded, \
             none of them recognised.  Commands seen: {histogram}.  A login that never reached \
             the game, or a key that could not be derived, looks like this -- the interface says \
             it is waiting for game data either way",
            first.elapsed().as_secs()
        );
    }

    /// `id x count` for the commands this connection has decoded, most frequent
    /// first.  What makes a "nothing was recognised" warning actionable.
    fn command_histogram(&self) -> String {
        let mut counts: Vec<(u16, u64)> = self.commands_seen.iter().map(|(id, n)| (*id, *n)).collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        counts
            .iter()
            .take(20)
            .map(|(id, n)| format!("{id}x{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Write a batch of balance changes to the ledger.
    fn apply(&mut self, changes: Vec<Change>) -> Result<()> {
        if changes.is_empty() {
            // Nothing to record -- but the first data of a session also moves
            // the reported state to "tracking", and that transition has to be
            // published even when the snapshot matched what we already knew
            // (which is the normal case right after a restart).
            if self.session_data && self.state != State::Tracking {
                return self.publish();
            }
            return Ok(());
        }

        for change in &changes {
            tracing::debug!(?change, "balance change");
        }
        let transactions = self.ledger.record(&changes)?;
        if transactions > 0 {
            tracing::info!("recorded {transactions} income/expense event(s)");
        }
        self.publish()
    }

    fn refresh_state(&mut self) {
        if self.error.is_some() {
            self.state = State::Error;
        } else if matches!(self.state, State::Error | State::Stopped) {
            // Terminal states are never left implicitly.
        } else if self.session_data {
            // This run has seen game data, not just balances left over in the
            // ledger from an earlier run.
            self.state = State::Tracking;
        } else if self.handshake {
            self.state = State::Receiving;
        } else {
            self.state = State::WaitingForHandshake;
        }
    }

    fn publish(&mut self) -> Result<()> {
        self.refresh_state();
        status::write(&self.data_dir, &self.status())
    }

    fn status(&self) -> Status {
        Status {
            app: "irminsul",
            version: env!("CARGO_PKG_VERSION"),
            pid: std::process::id(),
            started_at: self.started_at.clone(),
            updated_at: now(),
            state: self.state,
            session: self.session.clone(),
            error: self.error.clone(),
            nickname: self.tracker.nickname().map(str::to_string),
            balances: self
                .tracker
                .balances()
                .iter()
                .map(|(currency, balance)| (currency.key(), *balance))
                .collect(),
            complete: self.tracker.is_complete(),
            original_resin: self.tracker.original_resin(Local::now()),
            original_resin_last_increase_at: self
                .tracker
                .original_resin_increased_at()
                .map(crate::ledger::timestamp),
            session_data: self.session_data,
            reconnects: self.reconnects,
            game_running: crate::process::game_running(),
            transactions: self.ledger.transactions(),
        }
    }

    fn fail(&mut self, message: String) -> Result<()> {
        tracing::error!("{message}");
        self.error = Some(message);
        self.publish()
    }

    fn finish(&mut self, reason: &str) -> Result<()> {
        self.ledger.session_ended(reason)?;
        self.error = None;
        self.state = State::Stopped;
        status::write(&self.data_dir, &self.status())
    }
}

fn load_keys() -> Result<HashMap<u16, Vec<u8>>> {
    let keys: HashMap<u16, String> =
        serde_json::from_slice(include_bytes!("../keys/gi.json")).context("parse embedded gi.json")?;

    keys.iter()
        .map(|(key, value)| -> Result<_, _> { Ok((*key, BASE64_STANDARD.decode(value)?)) })
        .collect::<Result<HashMap<_, _>>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ryukin-monitor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn command(command_id: u16, proto_data: Vec<u8>) -> GameCommand {
        GameCommand {
            command_id,
            header_len: 0,
            data_len: proto_data.len() as u32,
            proto_data,
        }
    }

    /// A raw Ethernet/IPv4/UDP frame, in the shape the capture backend hands
    /// over.
    ///
    /// Connection events carry no protobuf and are recognised before the KCP
    /// layer, so they cannot be tested by calling `handle_command` -- the only
    /// way in is a whole packet.
    fn udp_frame(src_port: u16, dst_port: u16, payload: &[u8]) -> Vec<u8> {
        let udp_len = 8 + payload.len();
        let ip_len = 20 + udp_len;

        let mut frame = Vec::with_capacity(14 + ip_len);
        // Ethernet: destination, source, ethertype IPv4.
        frame.extend_from_slice(&[0x02, 0, 0, 0, 0, 0x01]);
        frame.extend_from_slice(&[0x02, 0, 0, 0, 0, 0x02]);
        frame.extend_from_slice(&[0x08, 0x00]);
        // IPv4: version 4, five 32-bit words, total length, protocol 17 (UDP).
        frame.push(0x45);
        frame.push(0x00);
        frame.extend_from_slice(&(ip_len as u16).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x00]); // identification
        frame.extend_from_slice(&[0x40, 0x00]); // don't fragment
        frame.push(64); // ttl
        frame.push(17); // UDP
        frame.extend_from_slice(&[0x00, 0x00]); // header checksum, not verified
        frame.extend_from_slice(&[10, 0, 0, 1]); // source address
        frame.extend_from_slice(&[10, 0, 0, 2]); // destination address
        // UDP: ports, length, checksum 0 ("none").
        frame.extend_from_slice(&src_port.to_be_bytes());
        frame.extend_from_slice(&dst_port.to_be_bytes());
        frame.extend_from_slice(&(udp_len as u16).to_be_bytes());
        frame.extend_from_slice(&[0x00, 0x00]);
        frame.extend_from_slice(payload);
        frame
    }

    /// The game's connection-level packets: a 20-byte-or-shorter UDP payload
    /// whose first four bytes are the code.
    fn connection_packet(code: u32) -> Vec<u8> {
        udp_frame(crate::capture::PORT_RANGE.0, 40_000, &code.to_be_bytes())
    }

    fn read_status(dir: &Path) -> serde_json::Value {
        let raw = std::fs::read(dir.join(status::STATUS_FILE)).expect("status.json should exist");
        serde_json::from_slice(&raw).unwrap()
    }

    fn ledger_lines(dir: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(dir.join(crate::ledger::LEDGER_FILE))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// `currency=balance` for every record of one kind, in file order.
    fn balances_of(records: &[serde_json::Value], kind: &str) -> Vec<String> {
        records
            .iter()
            .filter(|line| line["kind"] == kind)
            .filter_map(|line| {
                Some(format!("{}={}", line["currency"].as_str()?, line["balance"].as_i64()?))
            })
            .collect()
    }

    /// Before anything is captured, nothing is known -- and every currency says
    /// so the same way, by having no value at all.  The interface shows "unknown"
    /// for all six, which is the truth and the same truth for both sources: the
    /// props arrive in the login snapshot (zero included), while a wish currency
    /// sitting at zero is mentioned by no packet until the store sync enumerates
    /// the inventory.
    ///
    /// Then the two packets that carry absolute values arrive -- a login snapshot
    /// for the props, a store sync for the items -- and everything becomes a
    /// `baseline`.  Nothing may be recorded as income: the player's entire mora
    /// balance, or 523 无主的星辉 they have owned all along, is not a gain earned
    /// at that instant.
    #[test]
    fn the_first_values_are_baselines_not_income() {
        let dir = temp_dir("first-values");

        // Nothing captured yet.
        {
            let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
            monitor.publish().unwrap();
        }

        let status = read_status(&dir);
        assert!(
            status["balances"].as_object().is_none_or(|b| b.is_empty()),
            "nothing is known yet: {status:#?}"
        );
        assert_eq!(status["complete"], false);
        assert!(ledger_lines(&dir).iter().all(|r| r["kind"] != "baseline"));

        // The real balances turn up, both packets carrying absolute values.
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor
            .handle_command(&command(4_001, login_payload(7_150, 1_784_804, 3_000)))
            .unwrap();
        monitor
            .handle_command(&command(PLAYER_STORE_NOTIFY, store_payload(10, 0, 523)))
            .unwrap();

        let status = read_status(&dir);
        assert_eq!(status["balances"]["mora"], 1_784_804);
        assert_eq!(status["balances"]["masterless_starglitter"], 523);
        assert_eq!(status["balances"]["acquaint_fate"], 0, "absent from the store means zero");
        assert_eq!(status["complete"], true);
        assert_eq!(status["transactions"], 0, "a first sighting is not income");

        let records = ledger_lines(&dir);
        assert!(balances_of(&records, "tx").is_empty(), "no income may be fabricated");
        let baselines = balances_of(&records, "baseline");
        assert_eq!(baselines.len(), 6, "six first sightings: {baselines:?}");
        assert!(baselines.contains(&"mora=1784804".to_string()), "got {baselines:?}");
        assert!(baselines.contains(&"masterless_starglitter=523".to_string()), "got {baselines:?}");
        assert!(baselines.contains(&"acquaint_fate=0".to_string()), "got {baselines:?}");

        // Once observed, a later gain really is income -- including for the item
        // currency that was observed as an empty store.
        let gain = proto_wire::test_enc::prop_notify(&[(proto_wire::PROP_PRIMOGEM, 7_310)]);
        monitor.handle_command(&command(4_002, gain)).unwrap();
        let fate = proto_wire::test_enc::item_change_notify(&[(
            crate::tracker::ITEM_ID_ACQUAINT_FATE,
            0x1_0000_0000_44,
            1,
        )]);
        monitor.handle_command(&command(4_003, fate)).unwrap();

        let records = ledger_lines(&dir);
        let txs: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "tx").collect();
        assert_eq!(txs.len(), 2, "the two gains are the only income: {records:#?}");
        assert_eq!(txs[0]["currency"], "primogems");
        assert_eq!(txs[0]["delta"], 160);
        assert_eq!(txs[0]["direction"], "income");
        assert_eq!(txs[1]["currency"], "acquaint_fate");
        assert_eq!(txs[1]["delta"], 1, "the zero was observed, so this is a real gain");

        // A later start keeps every balance the ledger holds.
        {
            let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
            monitor.publish().unwrap();
        }

        let status = read_status(&dir);
        assert_eq!(status["balances"]["mora"], 1_784_804);
        assert_eq!(status["balances"]["intertwined_fate"], 10);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Starting the capture after the game is already running means no store
    /// sync was seen, so the first packet that mentions an item carries an
    /// absolute count with nothing to compare against -- and the same bytes mean
    /// either "you just gained this" or "here is the one you have had all
    /// along".  It has to be a `baseline`: claiming income there would invent a
    /// gain, which is the bug this whole area keeps producing.
    ///
    /// The second one is a change, because by then there is something to compare
    /// against.
    #[test]
    fn a_first_sighting_without_a_store_sync_is_a_baseline() {
        let dir = temp_dir("first-sighting");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();

        // A live store change carrying the absolute count.
        let gain = proto_wire::test_enc::item_change_notify(&[(
            crate::tracker::ITEM_ID_ACQUAINT_FATE,
            0x1_0000_0000_44,
            1,
        )]);
        monitor.handle_command(&command(4_001, gain)).unwrap();

        let records = ledger_lines(&dir);
        let baselines = balances_of(&records, "baseline");
        assert!(baselines.contains(&"acquaint_fate=1".to_string()), "got {baselines:?}");
        assert!(
            balances_of(&records, "tx").is_empty(),
            "the first sighting may not be claimed as income: {records:#?}"
        );
        assert_eq!(read_status(&dir)["transactions"], 0);

        // The next one really is a gain.
        let gain = proto_wire::test_enc::item_change_notify(&[(
            crate::tracker::ITEM_ID_ACQUAINT_FATE,
            0x1_0000_0000_44,
            2,
        )]);
        monitor.handle_command(&command(4_002, gain)).unwrap();

        let records = ledger_lines(&dir);
        let txs: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "tx").collect();
        assert_eq!(txs.len(), 1, "expected exactly one income: {records:#?}");
        assert_eq!(txs[0]["currency"], "acquaint_fate");
        assert_eq!(txs[0]["delta"], 1);
        assert_eq!(txs[0]["direction"], "income");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A realistic store payload: the real decoder only accepts packets with
    /// at least 10 items, so pad it the way the game does.
    fn store_payload(fate: u32, acquaint: u32, starglitter: u32) -> Vec<u8> {
        let mut items: Vec<(u32, u64, u32)> = (0..9)
            .map(|i| (1_000 + i, 0x1000_0000 + u64::from(i), 1))
            .collect();
        items.push((crate::tracker::ITEM_ID_INTERWINED_FATE, 0x1_0000_0000_11, fate));
        items.push((crate::tracker::ITEM_ID_ACQUAINT_FATE, 0x1_0000_0000_22, acquaint));
        items.push((
            crate::tracker::ITEM_ID_MASTERLESS_STARGLITTER,
            0x1_0000_0000_33,
            starglitter,
        ));
        proto_wire::test_enc::store_notify(&items)
    }

    /// The 原粹树脂 the login payloads above carry: the value a real 7.1 login
    /// reported, sitting between the mora (10016) and the genesis crystals
    /// (10025) of the same prop map.
    const LOGIN_ORIGINAL_RESIN: i64 = 170;

    fn login_payload(primogems: i64, mora: i64, crystals: i64) -> Vec<u8> {
        proto_wire::test_enc::player_data_notify(
            &[
                (proto_wire::PROP_PRIMOGEM, primogems),
                (proto_wire::PROP_MORA, mora),
                (proto_wire::PROP_GENESIS_CRYSTAL, crystals),
                (proto_wire::PROP_ORIGINAL_RESIN, LOGIN_ORIGINAL_RESIN),
            ],
            Some("Traveler"),
        )
    }

    /// The whole pipeline: login snapshot, store snapshot, then a gain and a
    /// spend -- the ledger must end up with two separate events, and
    /// status.json must describe the session for the app.
    #[test]
    fn records_income_and_expense_from_game_packets() {
        let dir = temp_dir("e2e");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();

        monitor.publish().unwrap();
        let status = read_status(&dir);
        assert_eq!(status["state"], "waiting_for_handshake");
        assert_eq!(status["complete"], false);

        // Player-data packet (props + nickname) and the item store packet.
        monitor.handle_command(&command(4_001, login_payload(5_000, 800_000, 120))).unwrap();
        monitor
            .handle_command(&command(PLAYER_STORE_NOTIFY, store_payload(5, 3, 40)))
            .unwrap();

        let status = read_status(&dir);
        assert_eq!(status["state"], "tracking");
        assert_eq!(status["complete"], true);
        assert_eq!(status["nickname"], "Traveler");
        assert_eq!(status["balances"]["primogems"], 5_000);
        assert_eq!(status["balances"]["mora"], 800_000);
        assert_eq!(status["balances"]["genesis_crystals"], 120);
        assert_eq!(status["balances"]["intertwined_fate"], 5);
        assert_eq!(status["balances"]["acquaint_fate"], 3);
        assert_eq!(status["balances"]["masterless_starglitter"], 40);

        // 原粹树脂 is reported beside the balances -- it is not one of them, so
        // it does not affect "complete" or the wish totals, and it never
        // reaches the ledger.  A sync alone cannot anchor the regeneration
        // cadence, so the value is published without an anchor: a reader can
        // still extrapolate, but the result can be one point low.
        assert_eq!(status["original_resin"], LOGIN_ORIGINAL_RESIN);
        assert!(
            status.get("original_resin_last_increase_at").is_none(),
            "a sync is not a regeneration point: {status:#?}"
        );

        // The game adding a point is what anchors it.  A resin change is not a
        // ledger change, so nothing is published on its account -- the
        // heartbeat is what carries it out, which the test stands in for.
        let tick = proto_wire::test_enc::prop_notify(&[(
            proto_wire::PROP_ORIGINAL_RESIN,
            LOGIN_ORIGINAL_RESIN + 1,
        )]);
        monitor.handle_command(&command(4_009, tick)).unwrap();
        monitor.publish().unwrap();
        let status = read_status(&dir);
        assert_eq!(status["original_resin"], LOGIN_ORIGINAL_RESIN + 1);
        assert!(
            status["original_resin_last_increase_at"].is_string(),
            "a live +1 anchors the cadence: {status:#?}"
        );

        // A chest gives 160 primogems, then a wish spends 160.
        let gain = proto_wire::test_enc::prop_notify(&[(proto_wire::PROP_PRIMOGEM, 5_160)]);
        monitor.handle_command(&command(4_002, gain)).unwrap();
        let spend = proto_wire::test_enc::prop_notify(&[(proto_wire::PROP_PRIMOGEM, 5_000)]);
        monitor.handle_command(&command(4_003, spend)).unwrap();

        let status = read_status(&dir);
        assert_eq!(status["transactions"], 2);
        assert_eq!(status["balances"]["primogems"], 5_000);

        let records = ledger_lines(&dir);
        let txs: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "tx").collect();
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0]["currency"], "primogems");
        assert_eq!(txs[0]["delta"], 160);
        assert_eq!(txs[0]["direction"], "income");
        assert_eq!(txs[0]["balance"], 5_160);
        assert_eq!(txs[1]["delta"], -160);
        assert_eq!(txs[1]["direction"], "expense");

        // Six starting points, one per currency, and the session books.
        assert_eq!(records.iter().filter(|r| r["kind"] == "baseline").count(), 6);
        assert!(
            records.iter().all(|r| r["currency"] != "original_resin"),
            "the stock is published in status.json only: {records:#?}"
        );
        assert_eq!(records[0]["kind"], "session");
        assert_eq!(records[0]["event"], "start");
        assert!(records.iter().any(|r| r["event"] == "identified" && r["nickname"] == "Traveler"));

        // Spending a fate is an expense of that currency.
        let fate_spend = proto_wire::test_enc::item_change_notify(&[(
            crate::tracker::ITEM_ID_INTERWINED_FATE,
            0x1_0000_0000_11,
            4,
        )]);
        monitor.handle_command(&command(4_004, fate_spend)).unwrap();
        let records = ledger_lines(&dir);
        let last_tx = records.iter().rev().find(|r| r["kind"] == "tx").unwrap();
        assert_eq!(last_tx["currency"], "intertwined_fate");
        assert_eq!(last_tx["delta"], -1);
        assert_eq!(last_tx["direction"], "expense");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A change that happened while the core was not running is recorded as a
    /// gap, and stays out of the income/expense totals.
    #[test]
    fn downtime_is_recorded_as_a_gap() {
        let dir = temp_dir("gap");

        // First session: record a balance, then stop.
        {
            let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
            monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();
            monitor.finish("stop-request").unwrap();
        }

        // Second session: the player earned 4_000 primogems while we were off.
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.handle_command(&command(4_001, login_payload(5_000, 500, 0))).unwrap();

        let records = ledger_lines(&dir);
        let gaps: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "gap").collect();
        assert_eq!(gaps.len(), 1, "expected exactly one gap record: {records:#?}");
        assert_eq!(gaps[0]["currency"], "primogems");
        assert_eq!(gaps[0]["delta"], 4_000);
        assert_eq!(gaps[0]["balance"], 5_000);
        assert_eq!(
            records.iter().filter(|r| r["kind"] == "tx").count(),
            0,
            "a gap must not be recorded as income"
        );

        // The first session is closed in the ledger.
        assert!(records.iter().any(|r| r["event"] == "end" && r["reason"] == "stop-request"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The stop channel (an app-owned directory the core only reads) and the
    /// error state the app relies on.
    #[test]
    fn stop_request_and_errors_are_published() {
        let dir = temp_dir("stop");
        let control = temp_dir("stop-control");
        let mut monitor = Monitor::new(&dir, Some(control.clone()), Some("s1".to_string()), None).unwrap();

        // Nothing is asked for at first, then the app writes its request.
        assert!(!status::take_stop_request(&control, "s1"));
        std::fs::write(control.join(status::STOP_FILE), b"s1").unwrap();
        assert!(status::take_stop_request(&control, "s1"));

        monitor.fail("pktmon said no".to_string()).unwrap();
        let status = read_status(&dir);
        assert_eq!(status["state"], "error");
        assert_eq!(status["error"], "pktmon said no");
        assert_eq!(status["session"], "s1", "the run's identity is published for the app");

        monitor.finish("stop-request").unwrap();
        let status = read_status(&dir);
        assert_eq!(status["state"], "stopped");
        assert!(status.get("error").is_none(), "the error must be cleared on a clean stop");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&control);
    }

    /// The bug this rule exists for: a `stop.request` left over from an earlier
    /// session killed every new capture within ~35 ms, so starting capture
    /// looked like it went straight to "stopped".
    #[test]
    fn stop_requests_from_another_session_are_ignored() {
        let control = temp_dir("stale-stop");
        let request = control.join(status::STOP_FILE);

        // Left behind by a previous run (the app crashed, or it was restored
        // from a backup): it names a different session.
        std::fs::write(&request, b"previous-session").unwrap();
        assert!(
            !status::take_stop_request(&control, "this-session"),
            "a request from a previous session must not stop a fresh capture"
        );
        assert!(request.exists(), "and must be left for the app to clean up");

        // The app asks this session to stop: it counts, and is consumed so it
        // cannot affect a later run.
        std::fs::write(&request, b"this-session\n").unwrap();
        assert!(status::take_stop_request(&control, "this-session"), "trailing whitespace is fine");
        assert!(!request.exists(), "an honoured request must be removed");
        assert!(!status::take_stop_request(&control, "this-session"), "and must not fire twice");

        // No file at all is never a request.
        assert!(!status::take_stop_request(&control, "this-session"));

        let _ = std::fs::remove_dir_all(&control);
    }

    /// A login that decodes commands and then produces no game data is the one
    /// state the run cannot otherwise describe, so it says so -- once, and with
    /// the command ids, because that list is what tells "the login never
    /// finished" apart from "the decoder no longer recognises the packets".
    #[test]
    fn a_login_that_produces_no_game_data_says_so() {
        let dir = temp_dir("stalled-login");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.publish().unwrap();

        // Nothing decoded yet: there is nothing to be stalled after.
        monitor.report_stalled_login();
        assert!(!monitor.stall_reported);

        // A command that is not game data at all -- what the interface calls
        // "waiting for game data".
        monitor.handle_command(&command(4_001, vec![0x45, 0x67, 0x89, 0xab])).unwrap();

        monitor.report_stalled_login();
        assert!(!monitor.stall_reported, "a moment later is not a stall");

        // Long enough, and it is said.
        monitor.first_command_at = Some(Instant::now() - STORE_SYNC_GRACE - Duration::from_secs(1));
        monitor.report_stalled_login();
        assert!(monitor.stall_reported);

        // A run that is recording is never stalled, however long it took to get
        // there: a snapshot that changes nothing still counts as having arrived.
        monitor.handle_command(&command(4_002, login_payload(1_000, 500, 0))).unwrap();
        assert_eq!(read_status(&dir)["state"], "tracking");
        monitor.stall_reported = false;
        monitor.report_stalled_login();
        assert!(!monitor.stall_reported);

        // And a new connection starts over.
        monitor.handle_packet(connection_packet(0xFF)).unwrap();
        assert!(!monitor.stall_reported);
        assert!(monitor.first_command_at.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Balances recovered from the ledger are not "capture in progress": a run
    /// that has seen no game data still reports that it is waiting.
    #[test]
    fn seeded_balances_do_not_look_like_live_capture() {
        let dir = temp_dir("seeded");

        {
            let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
            monitor.handle_command(&command(4_001, login_payload(7_000, 900, 30))).unwrap();
            monitor
                .handle_command(&command(PLAYER_STORE_NOTIFY, store_payload(9, 4, 55)))
                .unwrap();
            monitor.finish("stop-request").unwrap();
        }

        // A new run: the ledger supplies all six balances immediately, but
        // nothing has been captured yet.
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.publish().unwrap();
        let status = read_status(&dir);
        assert_eq!(status["balances"]["primogems"], 7_000, "the last known balance is reported");
        assert_eq!(status["complete"], true, "all six are known from the ledger");
        assert_eq!(status["session_data"], false, "but this run has captured nothing");
        assert_eq!(status["state"], "waiting_for_handshake", "so it must not claim to be recording");

        // As soon as real data arrives -- even a snapshot that changes nothing
        // -- the run is tracking.
        monitor.handle_command(&command(4_001, login_payload(7_000, 900, 30))).unwrap();
        let status = read_status(&dir);
        assert_eq!(status["session_data"], true);
        assert_eq!(status["state"], "tracking");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unrelated traffic must not produce ledger records or corrupt balances.
    #[test]
    fn ignores_unrelated_packets() {
        let dir = temp_dir("noise");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.handle_command(&command(4_001, login_payload(10, 20, 30))).unwrap();

        let before = ledger_lines(&dir).len();
        let noise = vec![
            command(1, vec![0x45, 0x67, 0x89, 0xab]),
            command(2, proto_wire::test_enc::item_del_notify(&[0x1_0000_0000_55])),
            command(3, b"not protobuf at all".to_vec()),
            command(9_999, vec![0xff; 32]),
        ];
        for command in &noise {
            monitor.handle_command(command).unwrap();
        }

        assert_eq!(ledger_lines(&dir).len(), before, "noise must not write ledger records");
        let status = read_status(&dir);
        assert_eq!(status["balances"]["primogems"], 10, "balances must be unchanged");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The game ends a connection for ordinary reasons, and the one the log
    /// showed was leaving co-op: nine of these, then a fresh handshake eleven
    /// milliseconds later.  Nothing is decided here beyond telling the truth
    /// about what the run is doing -- the keys and the KCP state stay, because
    /// they are what makes the replacement readable, and a connection that
    /// really is gone is caught by the packets that cannot be read.
    #[test]
    fn a_disconnected_packet_does_not_throw_the_keys_away() {
        let dir = temp_dir("disconnected");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();

        // A session that is up and running, with the inventory enumerated.
        monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();
        monitor
            .handle_command(&command(PLAYER_STORE_NOTIFY, store_payload(9, 4, 55)))
            .unwrap();
        let status = read_status(&dir);
        assert_eq!(status["state"], "tracking");
        assert_eq!(status["session_data"], true);

        // The game closes the connection.
        monitor.handle_packet(connection_packet(404)).unwrap();

        // "Recording" would be a lie until packets are decoded again -- and the
        // balances, which belong to the ledger rather than to the connection,
        // survive.
        let status = read_status(&dir);
        assert_eq!(status["state"], "waiting_for_handshake");
        assert_eq!(status["session_data"], false);
        assert_eq!(status["balances"]["primogems"], 1_000);
        assert_eq!(status["reconnects"], 0, "a connection event is not yet a reconnect");
        assert!(
            monitor.store_sync_seen,
            "the inventory the run already enumerated does not need enumerating again"
        );

        // The next connection is read as usual: its snapshot is compared against
        // what the ledger already knows, so a change made in between is a gap
        // and not income.
        monitor.handle_command(&command(4_002, login_payload(1_600, 500, 0))).unwrap();

        let records = ledger_lines(&dir);
        let gaps: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "gap").collect();
        assert_eq!(gaps.len(), 1, "expected exactly one gap: {records:#?}");
        assert_eq!(gaps[0]["currency"], "primogems");
        assert_eq!(gaps[0]["delta"], 600);
        assert_eq!(
            records.iter().filter(|r| r["kind"] == "tx").count(),
            0,
            "a gap must not be recorded as income"
        );
        assert_eq!(read_status(&dir)["state"], "tracking");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A new handshake means the player is logging in again, so the run is
    /// waiting for a key once more -- not still "tracking" the connection that
    /// just ended.
    #[test]
    fn a_new_handshake_makes_the_run_wait_again() {
        let dir = temp_dir("rehandshake");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();
        assert_eq!(read_status(&dir)["state"], "tracking");

        monitor.handle_packet(connection_packet(0xFF)).unwrap();

        let status = read_status(&dir);
        assert_eq!(status["state"], "waiting_for_handshake");
        assert_eq!(status["session_data"], false);
        assert_eq!(status["balances"]["primogems"], 1_000, "the ledger's balances stay");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A connection event must not disturb whatever else is running: the relay
    /// of these packets is a burst (five arrived at once in the log that
    /// prompted this), and each one has to leave the ledger alone.
    #[test]
    fn a_burst_of_disconnects_writes_nothing_to_the_ledger() {
        let dir = temp_dir("disconnect-burst");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();

        let before = ledger_lines(&dir).len();
        for _ in 0..5 {
            monitor.handle_packet(connection_packet(404)).unwrap();
        }

        assert_eq!(ledger_lines(&dir).len(), before, "no ledger records for a connection event");
        assert_eq!(read_status(&dir)["state"], "waiting_for_handshake");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The interface tells the player when data may have been missed, so the
    /// count behind that has to mean exactly "this run was blind for a while".
    /// Replacing a connection is not that: leaving co-op does it, it costs
    /// nothing, and warning about it every time is how a warning stops being
    /// read.  What counts is having given up on a connection because nothing
    /// could be decoded.
    #[test]
    fn only_a_connection_that_was_given_up_on_is_counted() {
        let dir = temp_dir("reconnect-count");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();

        // The login this run was started for.
        monitor.handle_packet(connection_packet(0xFF)).unwrap();
        assert_eq!(read_status(&dir)["reconnects"], 0, "the first login is not a reconnect");

        // Data arrives, then the connection is replaced twice in a burst, and
        // then the game closes it.  None of that is a reconnect on its own.
        monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();
        monitor.handle_packet(connection_packet(0xFF)).unwrap();
        monitor.handle_packet(connection_packet(0xFF)).unwrap();
        monitor.handle_packet(connection_packet(404)).unwrap();
        assert_eq!(read_status(&dir)["reconnects"], 0);

        // Data flows again, and then stops being readable: that is a reconnect,
        // and the second one is a second reconnect.
        monitor.handle_command(&command(4_002, login_payload(1_100, 500, 0))).unwrap();
        monitor.note_unreadable(UNREADABLE_LIMIT).unwrap();
        assert_eq!(read_status(&dir)["reconnects"], 1);

        monitor.handle_command(&command(4_003, login_payload(1_200, 500, 0))).unwrap();
        monitor.note_unreadable(UNREADABLE_LIMIT).unwrap();
        assert_eq!(read_status(&dir)["reconnects"], 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A connection the decoder cannot read is not "still recording": it is the
    /// state in which every following packet is either paid for with another
    /// brute-force search over the seeds of a session that has ended, or -- when
    /// no seeds were ever obtained -- silently dropped.  The state goes instead,
    /// and the run waits for a login, which is what actually recovers.
    #[test]
    fn a_connection_the_decoder_cannot_read_is_dropped() {
        let dir = temp_dir("unreadable");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();
        assert_eq!(read_status(&dir)["state"], "tracking");

        // A few unreadable packets are the normal price of a session whose seeds
        // are not known yet.
        monitor.note_unreadable(UNREADABLE_LIMIT - 1).unwrap();
        assert_eq!(read_status(&dir)["state"], "tracking", "a few must not drop the key");

        // A command that decodes clears the count, because the key fits again.
        monitor.handle_command(&command(4_002, login_payload(1_000, 500, 0))).unwrap();
        monitor.note_unreadable(UNREADABLE_LIMIT - 1).unwrap();
        assert_eq!(read_status(&dir)["state"], "tracking");

        // Without one in between, the count keeps adding up and the state goes.
        monitor.note_unreadable(1).unwrap();

        let status = read_status(&dir);
        assert_eq!(status["state"], "waiting_for_handshake");
        assert_eq!(status["session_data"], false);
        assert_eq!(status["reconnects"], 1, "the connection the player was using ended");
        assert_eq!(status["balances"]["primogems"], 1_000, "the ledger is not the connection");

        // Having given up, it does not give up again and again while the dead
        // connection keeps sending.
        monitor.note_unreadable(UNREADABLE_LIMIT * 10).unwrap();
        assert_eq!(read_status(&dir)["reconnects"], 1);

        // And the next login decodes exactly as it does after a restart.
        monitor.handle_command(&command(4_003, login_payload(1_600, 500, 0))).unwrap();
        assert_eq!(read_status(&dir)["state"], "tracking");
        let records = ledger_lines(&dir);
        assert_eq!(records.iter().filter(|r| r["kind"] == "gap").count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The case a counts-only-searches rule missed, and the run that exposed it:
    /// a connection whose key material was never obtained has nothing to search,
    /// so it fails silently and by the packet.  It is just as unreadable, and
    /// leaving the interface claiming to wait for game data is worse than saying
    /// that a new login is needed.
    #[test]
    fn a_connection_with_no_key_material_at_all_is_dropped() {
        let dir = temp_dir("unreadable-no-seeds");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.handle_command(&command(4_001, login_payload(1_000, 500, 0))).unwrap();

        // Every packet fails, none of them costs a search: no seeds were known.
        monitor.note_unreadable(UNREADABLE_LIMIT).unwrap();
        assert_eq!(monitor.lost_searches, 0, "nothing was searched for");

        let status = read_status(&dir);
        assert_eq!(status["state"], "waiting_for_handshake");
        assert_eq!(status["reconnects"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Before anything has ever decoded there is nothing to give up on: the
    /// packets of a login that has not reached its first command yet must not
    /// throw away a state that is about to be used.
    #[test]
    fn packets_the_decoder_cannot_read_before_it_decoded_anything_do_not_give_up() {
        let dir = temp_dir("unreadable-early");
        let mut monitor = Monitor::new(&dir, None, None, None).unwrap();
        monitor.publish().unwrap();

        monitor.note_unreadable(UNREADABLE_LIMIT * 10).unwrap();

        let status = read_status(&dir);
        assert_eq!(status["state"], "waiting_for_handshake");
        assert_eq!(status["reconnects"], 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The trap that cost a debugging round, pinned down here: the decoder
    /// answers a datagram carrying nothing but acknowledgements with an empty
    /// command batch -- the very same answer it gives for a packet it cannot
    /// decrypt.  Reading "no commands" as "the key is wrong" therefore drops a
    /// healthy connection as soon as acknowledgements arrive in a run, and a
    /// login burst produces exactly that.
    #[test]
    fn acknowledgement_only_traffic_decodes_to_nothing() {
        let mut sniffer = GameSniffer::new();

        // A KCP acknowledgement, in the shape the game sends: the conversation
        // id, four bytes it inserts, the rest of the header, four more bytes,
        // and no payload.
        let mut segment = Vec::new();
        segment.extend_from_slice(&7u32.to_le_bytes()); // conversation
        segment.extend_from_slice(&[0, 0, 0, 0]); // the game's inserted bytes
        segment.push(82); // IKCP_CMD_ACK
        segment.push(0); // fragment
        segment.extend_from_slice(&128u16.to_le_bytes()); // window
        segment.extend_from_slice(&0u32.to_le_bytes()); // timestamp
        segment.extend_from_slice(&1u32.to_le_bytes()); // sequence number
        segment.extend_from_slice(&0u32.to_le_bytes()); // unacknowledged
        segment.extend_from_slice(&0u32.to_le_bytes()); // payload length
        segment.extend_from_slice(&[0, 0, 0, 0]); // the game's, before the payload

        let parsed = sniffer.receive_packet(udp_frame(crate::capture::PORT_RANGE.0, 40_000, &segment));
        assert!(
            matches!(parsed, Some(GamePacket::Commands(ref commands)) if commands.is_empty()),
            "an acknowledgement must decode to nothing, not to a verdict about the key"
        );
    }
}
