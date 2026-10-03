//! Append-only ledger of currency income and expense events.
//!
//! The ledger is a JSONL file (`ledger.jsonl`) with one record per line.  It
//! is the durable output of this program: the WPF app only ever reads it, so
//! the data survives a GUI restart, and it can be inspected or edited with a
//! plain text editor.
//!
//! Income and expense are recorded as *separate* events rather than as a net
//! change: the game pushes the new balance of a currency every time it
//! changes, so the difference between two consecutive values gives the size
//! and the direction of a single change.
//!
//! Record kinds:
//!
//! - `baseline` -- the first balance ever seen for a currency.  No delta can
//!   be derived from it, so it is stored as a starting point only.
//! - `tx` -- a change observed live during a capture session.  `direction` is
//!   `income` for a gain and `expense` for a spend.
//! - `gap` -- the balance differs from the last known value, but the change
//!   happened while this program was not capturing (it was closed, or the
//!   session key could not be derived).  The size is known, the direction is
//!   known, but it cannot be attributed to one action and may be a mix of
//!   gains and spends, so it is kept out of the income/expense totals.
//! - `session` -- the program started, identified the account, or stopped.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Local, SecondsFormat};
use serde::Serialize;

pub const LEDGER_FILE: &str = "ledger.jsonl";

/// How far back to read the ledger when recovering the last known balances.
/// Large enough for tens of thousands of records, small enough that startup
/// stays instant no matter how long the ledger has been running.
const TAIL_BYTES: u64 = 4 << 20;

/// The currencies recorded by RyukinLedger.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Currency {
    Primogems,
    Mora,
    GenesisCrystals,
    IntertwinedFate,
    AcquaintFate,
    MasterlessStarglitter,
}

impl Currency {
    pub const ALL: [Currency; 6] = [
        Currency::Primogems,
        Currency::Mora,
        Currency::GenesisCrystals,
        Currency::IntertwinedFate,
        Currency::AcquaintFate,
        Currency::MasterlessStarglitter,
    ];

    /// Stable name used in the ledger, in `status.json` and by the WPF app.
    pub fn key(self) -> &'static str {
        match self {
            Currency::Primogems => "primogems",
            Currency::Mora => "mora",
            Currency::GenesisCrystals => "genesis_crystals",
            Currency::IntertwinedFate => "intertwined_fate",
            Currency::AcquaintFate => "acquaint_fate",
            Currency::MasterlessStarglitter => "masterless_starglitter",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Currency::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// Where a change came from, which decides how it is recorded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChangeSource {
    /// The game pushed a new value for exactly this currency: the change is
    /// one action (a reward, a purchase, ...) and its direction is meaningful.
    Live,
    /// A full snapshot (login / resync).  The difference to the previous
    /// value covers everything since we last looked, so it is a gap.
    Snapshot,
}

/// A balance change produced by the tracker, ready to be written.
#[derive(Copy, Clone, Debug)]
pub struct Change {
    pub currency: Currency,
    pub balance: i64,
    /// The previous balance, or `None` when this currency was never seen.
    pub previous: Option<i64>,
    pub source: ChangeSource,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Income,
    Expense,
}

/// One line of the ledger.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// The program started / identified the account / stopped.
    Session {
        at: String,
        event: &'static str,
        version: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        nickname: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// First balance ever seen for a currency.
    Baseline {
        at: String,
        currency: Currency,
        balance: i64,
    },
    /// A gain (`income`) or a spend (`expense`).
    Tx {
        at: String,
        currency: Currency,
        delta: i64,
        direction: Direction,
        balance: i64,
    },
    /// A change that happened while this program was not capturing.
    Gap {
        at: String,
        currency: Currency,
        delta: i64,
        balance: i64,
    },
}

pub fn now() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Millis, false)
}

/// Writer for the ledger file.
pub struct Ledger {
    path: PathBuf,
    file: BufWriter<File>,
    transactions: u64,
}

impl Ledger {
    /// Open the ledger in `dir`, recovering the last known balance of every
    /// currency from the records there.  Those balances seed the tracker, which
    /// is what lets a change made while the program was closed be recognised as
    /// a gap instead of being mistaken for income.
    pub fn open(dir: &Path) -> Result<(Self, BTreeMap<Currency, i64>)> {
        let path = dir.join(LEDGER_FILE);
        let known = read_last_balances(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open ledger {}", path.display()))?;
        Ok((
            Self {
                path,
                file: BufWriter::new(file),
                transactions: 0,
            },
            known,
        ))
    }

    pub fn transactions(&self) -> u64 {
        self.transactions
    }

    pub fn session_started(&mut self) -> Result<()> {
        self.session("start", None, None)
    }

    pub fn session_identified(&mut self, nickname: &str) -> Result<()> {
        self.session("identified", Some(nickname.to_string()), None)
    }

    pub fn session_ended(&mut self, reason: &str) -> Result<()> {
        self.session("end", None, Some(reason))
    }

    fn session(&mut self, event: &'static str, nickname: Option<String>, reason: Option<&str>) -> Result<()> {
        self.write(&Record::Session {
            at: now(),
            event,
            version: env!("CARGO_PKG_VERSION"),
            nickname,
            reason: reason.map(str::to_string),
        })
    }

    /// Record one batch of balance changes.  Returns the number of income /
    /// expense events written.
    pub fn record(&mut self, changes: &[Change]) -> Result<u64> {
        let mut transactions = 0;
        for change in changes {
            let at = now();
            let record = match (change.source, change.previous) {
                // A live push is exactly one action, so its direction is
                // trustworthy.
                (ChangeSource::Live, Some(previous)) => {
                    let delta = change.balance - previous;
                    if delta == 0 {
                        continue;
                    }
                    transactions += 1;
                    Record::Tx {
                        at,
                        currency: change.currency,
                        delta,
                        direction: if delta > 0 { Direction::Income } else { Direction::Expense },
                        balance: change.balance,
                    }
                }
                // A snapshot that differs from what we knew covers an unknown
                // amount of gameplay.
                (ChangeSource::Snapshot, Some(previous)) => {
                    let delta = change.balance - previous;
                    if delta == 0 {
                        continue;
                    }
                    Record::Gap {
                        at,
                        currency: change.currency,
                        delta,
                        balance: change.balance,
                    }
                }
                // First value ever for this currency: nothing to compare to.
                (_, None) => Record::Baseline {
                    at,
                    currency: change.currency,
                    balance: change.balance,
                },
            };
            self.write(&record)?;
        }
        self.transactions += transactions;
        Ok(transactions)
    }

    fn write(&mut self, record: &Record) -> Result<()> {
        let mut line = serde_json::to_string(record).context("serialize ledger record")?;
        line.push('\n');
        // Append and flush immediately: the ledger is the product, and a
        // crash should cost at most the record being written.
        self.file
            .write_all(line.as_bytes())
            .with_context(|| format!("append to {}", self.path.display()))?;
        self.file.flush().with_context(|| format!("flush {}", self.path.display()))?;
        Ok(())
    }
}

/// Read the last known balance of every currency from an existing ledger.
///
/// Only the tail of the file is read, and lines that do not parse (including a
/// partial line at the seek point, or a record written by a newer version) are
/// skipped, so this never fails on a damaged ledger.
///
/// Records of kind `assumed` are skipped as well.  An older version of this
/// program wrote them to display a zero for a currency the game had never
/// mentioned; they are not observations, so they must not become a previous
/// value for a delta -- which is also what lets a ledger written back then
/// behave correctly now.
pub fn read_last_balances(path: &Path) -> Result<BTreeMap<Currency, i64>> {
    let mut balances = BTreeMap::new();
    let Ok(mut file) = File::open(path) else {
        return Ok(balances);
    };
    let len = file.metadata().with_context(|| format!("stat {}", path.display()))?.len();
    if len > TAIL_BYTES {
        file.seek(SeekFrom::Start(len - TAIL_BYTES))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).with_context(|| format!("read {}", path.display()))?;

    for line in String::from_utf8_lossy(&buf).lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(currency) = value.get("currency").and_then(|v| v.as_str()).and_then(Currency::from_key) else {
            continue;
        };
        let Some(balance) = value.get("balance").and_then(|v| v.as_i64()) else {
            continue;
        };
        if value.get("kind").and_then(|v| v.as_str()) == Some("assumed") {
            continue;
        }

        balances.insert(currency, balance);
    }
    Ok(balances)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ryukin-ledger-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn change(currency: Currency, previous: Option<i64>, balance: i64, source: ChangeSource) -> Change {
        Change {
            currency,
            balance,
            previous,
            source,
        }
    }

    /// A first sighting is a baseline, a live gain is income, a live spend is
    /// an expense, and a snapshot difference is a gap -- never mixed up.
    #[test]
    fn classifies_changes() {
        let dir = temp_dir("classify");
        let (mut ledger, last) = Ledger::open(&dir).unwrap();
        assert!(last.is_empty());

        ledger
            .record(&[change(Currency::Primogems, None, 1000, ChangeSource::Snapshot)])
            .unwrap();
        // Live gain of 160.
        let tx = ledger
            .record(&[change(Currency::Primogems, Some(1000), 1160, ChangeSource::Live)])
            .unwrap();
        assert_eq!(tx, 1);
        // Live spend of 160 -- recorded as its own event, not netted away.
        let tx = ledger
            .record(&[change(Currency::Primogems, Some(1160), 1000, ChangeSource::Live)])
            .unwrap();
        assert_eq!(tx, 1);
        // A change while we were not capturing.
        let tx = ledger
            .record(&[change(Currency::Primogems, Some(1000), 4000, ChangeSource::Snapshot)])
            .unwrap();
        assert_eq!(tx, 0, "a gap is not an income event");

        let raw = std::fs::read_to_string(dir.join(LEDGER_FILE)).unwrap();
        let kinds: Vec<String> = raw
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(kinds, ["baseline", "tx", "tx", "gap"]);

        let deltas: Vec<i64> = raw
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap().get("delta").and_then(|v| v.as_i64()))
            .collect();
        // Both directions survive as separate signed events.
        assert_eq!(deltas, [160, -160, 3000]);

        let directions: Vec<String> = raw
            .lines()
            .filter_map(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .unwrap()
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(directions, ["income", "expense"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A value that did not actually change produces no record at all.
    #[test]
    fn ignores_unchanged_values() {
        let dir = temp_dir("unchanged");
        let (mut ledger, _) = Ledger::open(&dir).unwrap();
        assert_eq!(ledger.record(&[change(Currency::Mora, Some(5), 5, ChangeSource::Live)]).unwrap(), 0);
        assert_eq!(
            ledger.record(&[change(Currency::Mora, Some(5), 5, ChangeSource::Snapshot)]).unwrap(),
            0
        );
        let raw = std::fs::read_to_string(dir.join(LEDGER_FILE)).unwrap();
        assert!(raw.is_empty(), "expected no records, got {raw:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Balances survive a restart, so a change made while the program was
    /// closed is later recognised as a gap rather than as income.
    #[test]
    fn recovers_balances_across_restarts() {
        let dir = temp_dir("restart");
        {
            let (mut ledger, _) = Ledger::open(&dir).unwrap();
            ledger
                .record(&[
                    change(Currency::Primogems, None, 500, ChangeSource::Snapshot),
                    change(Currency::Mora, None, 900, ChangeSource::Snapshot),
                    change(Currency::Mora, Some(900), 100, ChangeSource::Live),
                ])
                .unwrap();
        }
        let (_, last) = Ledger::open(&dir).unwrap();
        assert_eq!(last.get(&Currency::Primogems), Some(&500));
        assert_eq!(last.get(&Currency::Mora), Some(&100));
        assert_eq!(last.get(&Currency::IntertwinedFate), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A truncated final line must not break startup.
    #[test]
    fn tolerates_damaged_ledger() {
        let dir = temp_dir("damaged");
        std::fs::write(
            dir.join(LEDGER_FILE),
            "{\"kind\":\"baseline\",\"currency\":\"mora\",\"balance\":7}\n{\"kind\":\"tx\",\"curr",
        )
        .unwrap();
        let last = read_last_balances(&dir.join(LEDGER_FILE)).unwrap();
        assert_eq!(last.get(&Currency::Mora), Some(&7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ledger written by an older version may contain `assumed` records: a
    /// zero that was only ever *displayed*, for a currency the game had never
    /// mentioned.  They must not be read back as balances, or the first real
    /// value would be recorded as an income of the whole holding -- the exact
    /// bug those records caused.
    #[test]
    fn assumed_records_from_an_older_ledger_are_not_balances() {
        let dir = temp_dir("assumed");
        std::fs::write(
            dir.join(LEDGER_FILE),
            "{\"kind\":\"session\",\"at\":\"2026-10-01T10:00:00.000+08:00\",\"event\":\"start\",\"version\":\"0.4.0\"}\n\
             {\"kind\":\"assumed\",\"at\":\"2026-10-01T10:00:00.000+08:00\",\"currency\":\"intertwined_fate\",\"balance\":0}\n\
             {\"kind\":\"assumed\",\"at\":\"2026-10-01T10:00:00.000+08:00\",\"currency\":\"masterless_starglitter\",\"balance\":0}\n\
             {\"kind\":\"baseline\",\"at\":\"2026-10-01T10:01:00.000+08:00\",\"currency\":\"mora\",\"balance\":1784804}\n",
        )
        .unwrap();

        let known = read_last_balances(&dir.join(LEDGER_FILE)).unwrap();
        assert_eq!(known.get(&Currency::Mora), Some(&1_784_804), "a real observation is read back");
        assert_eq!(known.get(&Currency::IntertwinedFate), None, "an assumed zero is not a balance");
        assert_eq!(known.get(&Currency::MasterlessStarglitter), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn currency_keys_round_trip() {
        for currency in Currency::ALL {
            assert_eq!(Currency::from_key(currency.key()), Some(currency));
        }
        assert_eq!(Currency::from_key("nope"), None);
    }
}
