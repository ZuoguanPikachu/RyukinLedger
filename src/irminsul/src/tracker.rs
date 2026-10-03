//! Tracks the current balance of every recorded currency and turns balance
//! changes into income / expense events.
//!
//! The game reports *balances*, not incomes and expenses: when something
//! changes, the server pushes the new value of the affected currency.  Two
//! kinds of update exist and they mean different things:
//!
//! - a targeted update (`PlayerPropNotify`, store item change / removal)
//!   concerns exactly one currency, so the difference to the previous value is
//!   one action whose direction is meaningful -- this becomes an income or an
//!   expense;
//! - a full snapshot (login, resync) arrives after an unknown amount of
//!   gameplay (this program may have been closed), so the difference is
//!   recorded as a gap that is kept out of the income/expense totals.
//!
//! [`crate::ledger::Change`] carries the previous value so the ledger can make
//! that distinction.

use std::collections::{BTreeMap, HashMap};

use crate::ledger::{Change, ChangeSource, Currency};
use crate::proto_wire;

/// Item ids of the wish-related currencies, as carried by `PlayerStoreNotify`
/// and the store item change notifies.
pub const ITEM_ID_MASTERLESS_STARGLITTER: u32 = 221;
pub const ITEM_ID_INTERWINED_FATE: u32 = 223;
pub const ITEM_ID_ACQUAINT_FATE: u32 = 224;

/// The store items that carry a recorded currency.
///
/// The distinction between these and [`TRACKED_PROPS`] matters: the game omits
/// items whose count is zero from every packet that would carry them, so a store
/// currency sitting at zero is mentioned by nothing at all -- while a player
/// prop is present in every snapshot whether it is zero or not.
const TRACKED_ITEMS: [(u32, Currency); 3] = [
    (ITEM_ID_MASTERLESS_STARGLITTER, Currency::MasterlessStarglitter),
    (ITEM_ID_INTERWINED_FATE, Currency::IntertwinedFate),
    (ITEM_ID_ACQUAINT_FATE, Currency::AcquaintFate),
];

/// The player props that carry a recorded currency: the 7.0 prop id and the
/// pre-7.0 prop id, with the currency they represent.
const TRACKED_PROPS: [((u32, u32), Currency); 3] = [
    (proto_wire::PRIMOGEM_PROP_IDS, Currency::Primogems),
    (proto_wire::MORA_PROP_IDS, Currency::Mora),
    (proto_wire::GENESIS_CRYSTAL_PROP_IDS, Currency::GenesisCrystals),
];

/// The currency carried by a store item, if it is one we record.
pub fn currency_for_item(item_id: u32) -> Option<Currency> {
    TRACKED_ITEMS.iter().find(|(id, _)| *id == item_id).map(|(_, currency)| *currency)
}

pub struct BalanceTracker {
    /// Last known balance per currency.  Seeded from the ledger on startup so
    /// that a change made while this program was closed is recognised as a
    /// gap rather than mistaken for income.
    balances: BTreeMap<Currency, i64>,
    /// guid -> item id, learned from the store sync and change notifies, so a
    /// removal notify (which carries a guid only) can be mapped back to an
    /// item.
    item_guids: HashMap<u64, u32>,
    nickname: Option<String>,
}

impl BalanceTracker {
    pub fn new(seed: BTreeMap<Currency, i64>) -> Self {
        Self {
            balances: seed,
            item_guids: HashMap::new(),
            nickname: None,
        }
    }

    pub fn balances(&self) -> &BTreeMap<Currency, i64> {
        &self.balances
    }

    /// Whether every recorded currency has a known value.
    pub fn is_complete(&self) -> bool {
        Currency::ALL.iter().all(|currency| self.balances.contains_key(currency))
    }

    pub fn nickname(&self) -> Option<&str> {
        self.nickname.as_deref()
    }

    /// Returns the nickname when it changed.
    pub fn set_nickname(&mut self, name: String) -> Option<String> {
        if self.nickname.as_deref() == Some(name.as_str()) {
            return None;
        }
        self.nickname = Some(name.clone());
        Some(name)
    }

    /// Record a full player-data snapshot.
    pub fn snapshot_props(&mut self, props: &BTreeMap<u32, i64>) -> Vec<Change> {
        let mut changes = Vec::new();
        for (ids, currency) in TRACKED_PROPS {
            if let Some(balance) = proto_wire::prop_value(props, ids) {
                self.push(&mut changes, currency, balance, ChangeSource::Snapshot);
            }
        }
        changes
    }

    /// Record a full store snapshot.
    ///
    /// The store packet enumerates every owned item, so a tracked item that is
    /// absent from it has a count of zero -- which is the only way this program
    /// ever learns that a wish currency is empty, since the game never mentions
    /// a zero-count item in any other packet.
    ///
    /// Takes `(item id, guid, count)` triples rather than the generated
    /// protobuf type: the container field number of the store packet changed in
    /// the 7.1 game update and the pinned decoder has not caught up, while our
    /// own wire parser reads the items out of it either way.  See
    /// `monitor::PLAYER_STORE_NOTIFY`.
    pub fn snapshot_items(&mut self, items: &[(u32, u64, u32)]) -> Vec<Change> {
        for &(item_id, guid, _) in items {
            if currency_for_item(item_id).is_some() {
                self.item_guids.insert(guid, item_id);
            }
        }

        let mut changes = Vec::new();
        for (item_id, currency) in TRACKED_ITEMS {
            let balance = items
                .iter()
                .find(|(id, _, _)| *id == item_id)
                .map_or(0, |(_, _, count)| i64::from(*count));
            self.push(&mut changes, currency, balance, ChangeSource::Snapshot);
        }
        changes
    }

    /// Record an incremental player-prop update.
    pub fn live_prop_updates(&mut self, props: &BTreeMap<u32, i64>) -> Vec<Change> {
        let mut changes = Vec::new();
        for (ids, currency) in TRACKED_PROPS {
            if let Some(balance) = proto_wire::prop_value(props, ids) {
                self.push(&mut changes, currency, balance, ChangeSource::Live);
            }
        }
        changes
    }

    /// Record incremental store item changes.
    pub fn live_item_changes(&mut self, changes: &[(u32, u64, u32)]) -> Vec<Change> {
        let mut result = Vec::new();
        for &(item_id, guid, count) in changes {
            let Some(currency) = currency_for_item(item_id) else {
                continue;
            };
            self.item_guids.insert(guid, item_id);
            self.push(&mut result, currency, i64::from(count), ChangeSource::Live);
        }
        result
    }

    /// Record store item removals: an item whose count reached zero is removed
    /// from the store, and the notify carries only the removed guids.
    pub fn live_item_removals(&mut self, guids: &[u64]) -> Vec<Change> {
        let mut result = Vec::new();
        for &guid in guids {
            let Some(&item_id) = self.item_guids.get(&guid) else {
                continue;
            };
            let Some(currency) = currency_for_item(item_id) else {
                continue;
            };
            self.push(&mut result, currency, 0, ChangeSource::Live);
        }
        result
    }

    /// Record a change, skipping values that did not actually change.
    fn push(&mut self, out: &mut Vec<Change>, currency: Currency, balance: i64, source: ChangeSource) {
        let previous = self.balances.get(&currency).copied();
        if previous == Some(balance) {
            return;
        }

        self.balances.insert(currency, balance);
        out.push(Change {
            currency,
            balance,
            previous,
            source,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Ledger, Record};

    /// Balances as the ledger would hand them over.
    fn known(values: &[(Currency, i64)]) -> BTreeMap<Currency, i64> {
        values.iter().copied().collect()
    }

    /// A realistic store sync: the whole inventory, of which we track three
    /// items.  Anything we track that is missing from the list is at zero.
    #[test]
    fn a_store_sync_supplies_every_tracked_item() {
        let mut items: Vec<(u32, u64, u32)> = (0..9).map(|i| (1_000 + i, 0x1000_0000 + u64::from(i), 1)).collect();
        items.push((ITEM_ID_INTERWINED_FATE, 0x1_0000_0000_11, 12));
        items.push((ITEM_ID_ACQUAINT_FATE, 0x1_0000_0000_22, 3));
        items.push((ITEM_ID_MASTERLESS_STARGLITTER, 0x1_0000_0000_33, 40));

        let mut tracker = BalanceTracker::new(known(&[]));
        let changes = tracker.snapshot_items(&items);
        assert_eq!(changes.len(), 3);
        assert!(changes.iter().all(|c| c.previous.is_none() && c.source == ChangeSource::Snapshot));
        assert_eq!(tracker.balances().get(&Currency::IntertwinedFate), Some(&12));
        assert_eq!(tracker.balances().get(&Currency::AcquaintFate), Some(&3));
        assert_eq!(tracker.balances().get(&Currency::MasterlessStarglitter), Some(&40));
        assert!(
            !tracker.is_complete(),
            "the store sync covers the items only; the props come from the login snapshot"
        );
    }

    /// The item the player is out of is absent from the store, and that is the
    /// only way this program ever learns it is zero: the game never mentions a
    /// zero-count item in any other packet.
    #[test]
    fn absent_items_count_as_zero() {
        let mut tracker = BalanceTracker::new(known(&[]));
        let changes = tracker.snapshot_items(&[(ITEM_ID_INTERWINED_FATE, 7, 2)]);
        assert_eq!(changes.len(), 3);
        assert_eq!(tracker.balances().get(&Currency::IntertwinedFate), Some(&2));
        assert_eq!(tracker.balances().get(&Currency::AcquaintFate), Some(&0));
        assert_eq!(tracker.balances().get(&Currency::MasterlessStarglitter), Some(&0));

        // Observed as zero, so the first one really is an income.
        let changes = tracker.live_item_changes(&[(ITEM_ID_ACQUAINT_FATE, 0x44, 1)]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].previous, Some(0), "the zero was observed, not invented");
        assert_eq!(changes[0].balance, 1);
    }

    /// Spending and gaining each produce their own event instead of cancelling
    /// out into a net change.
    #[test]
    fn income_and_expense_are_separate_events() {
        let mut tracker = BalanceTracker::new(known(&[]));
        tracker.snapshot_props(&BTreeMap::from([
            (proto_wire::PROP_PRIMOGEM, 10_000),
            (proto_wire::PROP_MORA, 1_000_000),
            (proto_wire::PROP_GENESIS_CRYSTAL, 0),
        ]));

        // Gain 160 primogems from a chest.
        let changes = tracker.live_prop_updates(&BTreeMap::from([(proto_wire::PROP_PRIMOGEM, 10_160)]));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].currency, Currency::Primogems);
        assert_eq!(changes[0].balance, 10_160);
        assert_eq!(changes[0].previous, Some(10_000));
        assert_eq!(changes[0].source, ChangeSource::Live);

        // Spend 160 primogems and 1000 mora.
        let changes = tracker.live_prop_updates(&BTreeMap::from([
            (proto_wire::PROP_PRIMOGEM, 10_000),
            (proto_wire::PROP_MORA, 999_000),
        ]));
        assert_eq!(changes.len(), 2);

        // An unchanged value is not an event.
        assert!(tracker.live_prop_updates(&BTreeMap::from([(proto_wire::PROP_MORA, 999_000)])).is_empty());
    }

    /// Starting the capture after the game is already running means no store
    /// sync was seen, so the first packet that mentions an item carries an
    /// absolute count with nothing to compare against.  It has to be a first
    /// sighting: the player may have owned that item all along, and claiming it
    /// as income would invent a gain.
    #[test]
    fn a_first_sighting_without_a_store_sync_is_not_income() {
        let mut tracker = BalanceTracker::new(known(&[]));

        let changes = tracker.live_item_changes(&[(ITEM_ID_MASTERLESS_STARGLITTER, 0x33, 523)]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].balance, 523);
        assert_eq!(changes[0].previous, None, "523 was not earned now, so there is nothing to compare to");

        // Having really been observed, the next change is a change.
        let changes = tracker.live_item_changes(&[(ITEM_ID_MASTERLESS_STARGLITTER, 0x33, 600)]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].previous, Some(523));
        assert_eq!(changes[0].balance, 600);
    }

    /// A change that happened while we were not capturing must be marked as a
    /// snapshot, so the ledger can record it as a gap.
    #[test]
    fn snapshot_after_downtime_is_marked_as_snapshot() {
        let mut tracker = BalanceTracker::new(known(&[(Currency::Primogems, 500)]));
        let changes = tracker.snapshot_props(&BTreeMap::from([
            (proto_wire::PROP_PRIMOGEM, 9_000),
            (proto_wire::PROP_MORA, 1),
            (proto_wire::PROP_GENESIS_CRYSTAL, 2),
        ]));
        let primogems = changes.iter().find(|c| c.currency == Currency::Primogems).unwrap();
        assert_eq!(primogems.previous, Some(500));
        assert_eq!(primogems.source, ChangeSource::Snapshot);
    }

    #[test]
    fn legacy_prop_namespace_still_works() {
        let mut tracker = BalanceTracker::new(known(&[]));
        let changes = tracker.snapshot_props(&BTreeMap::from([
            (proto_wire::PROP_HCOIN, 42),
            (proto_wire::PROP_SCOIN, 43),
        ]));
        assert_eq!(changes.len(), 2);
        assert_eq!(tracker.balances().get(&Currency::Primogems), Some(&42));
        assert_eq!(tracker.balances().get(&Currency::Mora), Some(&43));
    }

    /// Item removals resolve through the guid map learned earlier.
    #[test]
    fn removals_resolve_through_guid_map() {
        let mut tracker = BalanceTracker::new(known(&[]));
        tracker.snapshot_items(&[(ITEM_ID_INTERWINED_FATE, 0x99, 1)]);

        let changes = tracker.live_item_removals(&[0x99]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].currency, Currency::IntertwinedFate);
        assert_eq!(changes[0].balance, 0);
        assert_eq!(changes[0].source, ChangeSource::Live);

        // An unknown guid is ignored, and a removal of an already empty
        // currency is not a change.
        assert!(tracker.live_item_removals(&[0xDEAD]).is_empty());
        assert!(tracker.live_item_removals(&[0x99]).is_empty());
    }

    /// The full path: player-data payload -> tracker -> ledger, checking that
    /// a gain and a spend are written as two separate ledger events.
    #[test]
    fn end_to_end_gain_and_spend() {
        let dir = std::env::temp_dir().join(format!("ryukin-tracker-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let (mut ledger, last) = Ledger::open(&dir).unwrap();
        let mut tracker = BalanceTracker::new(last);

        // Login: full snapshot.
        let payload = proto_wire::test_enc::player_data_notify(
            &[
                (proto_wire::PROP_PRIMOGEM, 8_000),
                (proto_wire::PROP_MORA, 2_000_000),
                (proto_wire::PROP_GENESIS_CRYSTAL, 300),
            ],
            Some("Traveler"),
        );
        let packet = proto_wire::extract_player_packet(&payload).unwrap();
        assert_eq!(tracker.set_nickname(packet.nick_name.unwrap()).as_deref(), Some("Traveler"));
        assert_eq!(ledger.record(&tracker.snapshot_props(&packet.props)).unwrap(), 0);

        // The store sync supplies the three item-backed currencies.
        assert!(!tracker.is_complete());
        let store = [
            (ITEM_ID_INTERWINED_FATE, 0x1_0000_0000_11, 12),
            (ITEM_ID_ACQUAINT_FATE, 0x1_0000_0000_22, 3),
            (ITEM_ID_MASTERLESS_STARGLITTER, 0x1_0000_0000_33, 40),
        ];
        assert_eq!(ledger.record(&tracker.snapshot_items(&store)).unwrap(), 0);

        // A gain, then a spend of the same size: two events, not one zero.
        let gain = proto_wire::test_enc::prop_notify(&[(proto_wire::PROP_PRIMOGEM, 8_160)]);
        let props = proto_wire::extract_prop_updates(&gain).unwrap();
        assert_eq!(ledger.record(&tracker.live_prop_updates(&props)).unwrap(), 1);

        let spend = proto_wire::test_enc::prop_notify(&[(proto_wire::PROP_PRIMOGEM, 8_000)]);
        let props = proto_wire::extract_prop_updates(&spend).unwrap();
        assert_eq!(ledger.record(&tracker.live_prop_updates(&props)).unwrap(), 1);

        assert_eq!(ledger.transactions(), 2);
        assert!(tracker.is_complete());

        let raw = std::fs::read_to_string(dir.join(crate::ledger::LEDGER_FILE)).unwrap();
        let records: Vec<serde_json::Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let txs: Vec<&serde_json::Value> = records.iter().filter(|r| r["kind"] == "tx").collect();
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0]["delta"], 160);
        assert_eq!(txs[0]["direction"], "income");
        assert_eq!(txs[1]["delta"], -160);
        assert_eq!(txs[1]["direction"], "expense");

        // Exactly the six currencies, each with a starting point.
        let baselines = records.iter().filter(|r| r["kind"] == "baseline").count();
        assert_eq!(baselines, 6, "expected one baseline per currency");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Record` is only referenced to keep the serde representation covered.
    #[test]
    fn record_shapes_are_stable() {
        let json = serde_json::to_string(&Record::Tx {
            at: "2026-08-20T21:00:00.000+08:00".to_string(),
            currency: Currency::Mora,
            delta: -1000,
            direction: crate::ledger::Direction::Expense,
            balance: 500,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"kind":"tx","at":"2026-08-20T21:00:00.000+08:00","currency":"mora","delta":-1000,"direction":"expense","balance":500}"#
        );
    }
}
