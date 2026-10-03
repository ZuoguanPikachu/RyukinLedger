//! Packet capture loop: decode game traffic, track balances, write the ledger.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use auto_artifactarium::{ConnectionPacket, GameCommand, GamePacket, GameSniffer};
use base64::prelude::*;

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
            sniffer: GameSniffer::new().set_initial_keys(keys),
            tracker: BalanceTracker::new(known),
            ledger,
            started_at: now(),
            state: State::WaitingForHandshake,
            error: None,
            handshake: false,
            session_data: false,
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

        match parsed {
            GamePacket::Connection(ConnectionPacket::HandshakeRequested) => {
                // The sniffer has dropped its keys: the player is logging in
                // again, and a fresh session key will be derived.  The known
                // balances stay valid, so no data is lost here.
                tracing::info!("handshake requested; deriving a new session key");
                self.handshake = false;
            }
            GamePacket::Connection(_) => {}
            GamePacket::Commands(commands) => {
                if !commands.is_empty() {
                    self.handshake = true;
                }
                for command in &commands {
                    self.handle_command(command)?;
                }
            }
        }
        Ok(())
    }

    fn handle_command(&mut self, command: &GameCommand) -> Result<()> {
        *self.commands_seen.entry(command.command_id).or_insert(0) += 1;

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

        let mut counts: Vec<(u16, u64)> = self.commands_seen.iter().map(|(id, n)| (*id, *n)).collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1));
        let histogram = counts
            .iter()
            .take(20)
            .map(|(id, n)| format!("{id}x{n}"))
            .collect::<Vec<_>>()
            .join(" ");

        tracing::warn!(
            "no store sync (command {PLAYER_STORE_NOTIFY}) seen in {}s; item balances cannot be \
             known.  Commands seen: {histogram}",
            STORE_SYNC_GRACE.as_secs()
        );
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
            session_data: self.session_data,
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

    fn login_payload(primogems: i64, mora: i64, crystals: i64) -> Vec<u8> {
        proto_wire::test_enc::player_data_notify(
            &[
                (proto_wire::PROP_PRIMOGEM, primogems),
                (proto_wire::PROP_MORA, mora),
                (proto_wire::PROP_GENESIS_CRYSTAL, crystals),
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
}
