//! Minimal protobuf wire-format helpers.
//!
//! Irminsul's `auto-artifactarium` dependency only ships generated messages
//! for a few packets (items, avatars, achievements), so the player-data
//! message that carries the player's currency properties (primogems, mora,
//! genesis crystals) and the notifies that change them have no generated
//! type.  Rather than forking that dependency just to add one message, we
//! decode the wire format directly.
//!
//! The parser is deliberately structural instead of keyed to a command id:
//! command ids (and proto field numbers) change with every game version.
//! Player property ids changed with version 7.0 from a 2xx namespace to a
//! 100xx namespace (10015 = primogems, 10016 = mora, 10025 = genesis
//! crystals); both namespaces are accepted so captures from older protocol
//! versions keep working.

use std::collections::BTreeMap;

// 7.0+ player prop ids.
/// `PROP_PLAYER_HCOIN` (7.0 id) -- 原石 / Primogems.
pub const PROP_PRIMOGEM: u32 = 10015;
/// `PROP_PLAYER_SCOIN` (7.0 id) -- 摩拉 / Mora.
pub const PROP_MORA: u32 = 10016;
/// `PROP_PLAYER_MCOIN` (7.0 id) -- 创世结晶 / Genesis crystals.
pub const PROP_GENESIS_CRYSTAL: u32 = 10025;

// Pre-7.0 player prop ids, kept so captures from older protocol versions
// still work.
/// `PROP_PLAYER_HCOIN` -- 原石 / Primogems (pre-7.0).
pub const PROP_HCOIN: u32 = 201;
/// `PROP_PLAYER_SCOIN` -- 摩拉 / Mora (pre-7.0).
pub const PROP_SCOIN: u32 = 202;
/// `PROP_PLAYER_MCOIN` -- 创世结晶 / Genesis crystals (pre-7.0).
pub const PROP_MCOIN: u32 = 203;
/// `PROP_PLAYER_RESIN` (7.0 id) -- 原粹树脂 / Original Resin.
///
/// Read from a capture of the 7.1 client: the login snapshot carries it beside
/// the currencies, and its value is the stamina the game displays next to 原粹
/// 树脂.
pub const PROP_ORIGINAL_RESIN: u32 = 10020;
/// `PROP_PLAYER_RESIN` -- 原粹树脂 / Original Resin (pre-7.0).
///
/// Inferred rather than captured, and kept only so a capture from an older
/// protocol version still finds the stamina: the pre-7.0 prop ids were the ids
/// of the game's virtual items -- 原石/摩拉/创世结晶 are 201/202/203 in that
/// table, which is exactly what those props used to be -- and it calls 106
/// `OriginalResin`.
pub const PROP_LEGACY_ORIGINAL_RESIN: u32 = 106;

/// The player props that carry a currency, as `(7.0 id, pre-7.0 id)`.
pub const PRIMOGEM_PROP_IDS: (u32, u32) = (PROP_PRIMOGEM, PROP_HCOIN);
pub const MORA_PROP_IDS: (u32, u32) = (PROP_MORA, PROP_SCOIN);
pub const GENESIS_CRYSTAL_PROP_IDS: (u32, u32) = (PROP_GENESIS_CRYSTAL, PROP_MCOIN);

/// The player props that carry 原粹树脂, in the order they are trusted.
///
/// A list rather than the `(modern, legacy)` pair the currencies use: there is
/// one modern id, and the pre-7.0 one is a fallback rather than a second
/// spelling of the same capture.
pub const RESIN_PROP_IDS: [u32; 2] = [PROP_ORIGINAL_RESIN, PROP_LEGACY_ORIGINAL_RESIN];

/// Player prop ids that predate the 7.0 namespace, used to recognise an
/// incremental prop update from an older capture.
const LEGACY_PROP_IDS: [u32; 4] = [PROP_HCOIN, PROP_SCOIN, PROP_MCOIN, PROP_LEGACY_ORIGINAL_RESIN];

/// The 7.0 player prop namespace.  Avatar props (1001..4001) fall outside it.
const MODERN_PROP_RANGE: std::ops::Range<u32> = 10000..20000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum WireType {
    Varint,
    Fixed64,
    LengthDelimited,
    Fixed32,
}

/// A reader over a protobuf payload that yields one field at a time.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn read_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let byte = *self.data.get(self.pos)?;
            self.pos += 1;
            if shift >= 64 {
                // Overlong varint: more than 10 bytes / shift out of range.
                return None;
            }
            if shift == 63 && byte > 1 {
                return None;
            }
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
        }
    }

    fn next_field(&mut self) -> Option<(u32, WireType, u64, &'a [u8])> {
        let tag = self.read_varint()?;
        let field_number = (tag >> 3) as u32;
        if field_number == 0 {
            return None;
        }
        let wire_type = match tag & 0x7 {
            0 => WireType::Varint,
            1 => WireType::Fixed64,
            2 => WireType::LengthDelimited,
            5 => WireType::Fixed32,
            // Groups are not used by these messages and are never emitted by
            // protobuf encoders for proto3, so treat them as invalid.
            _ => return None,
        };

        match wire_type {
            WireType::Varint => {
                let value = self.read_varint()?;
                Some((field_number, wire_type, value, &[]))
            }
            WireType::Fixed64 => {
                // Bounds-check the fixed size instead of walking past the end.
                self.data.get(self.pos..self.pos + 8)?;
                self.pos += 8;
                Some((field_number, wire_type, 0, &[]))
            }
            WireType::Fixed32 => {
                self.data.get(self.pos..self.pos + 4)?;
                self.pos += 4;
                Some((field_number, wire_type, 0, &[]))
            }
            WireType::LengthDelimited => {
                let len = self.read_varint()? as usize;
                let bytes = self.data.get(self.pos..self.pos.checked_add(len)?)?;
                self.pos += len;
                Some((field_number, wire_type, 0, bytes))
            }
        }
    }
}

/// Decode a `PropValue` message:
///
/// ```proto
/// message PropValue {
///     uint32 type = 1;
///     oneof value {
///         int64 ival = 2;
///         float fval = 3;
///     }
///     int64 val = 4;
/// }
/// ```
///
/// Returns the integer value (`ival` or `val`), or `None` when the message
/// only carries a float or is malformed.
fn parse_prop_value(data: &[u8]) -> Option<i64> {
    let mut reader = Reader::new(data);
    let mut value: Option<i64> = None;
    while let Some((field_number, wire_type, varint, _)) = reader.next_field() {
        match (field_number, wire_type) {
            (2, WireType::Varint) => value = Some(varint as i64), // ival
            (4, WireType::Varint) => value = Some(varint as i64), // val
            _ => (),
        }
    }
    value
}

/// Try to interpret `bytes` as a single `map<uint32, PropValue>` entry:
///
/// ```proto
/// message MapEntry {
///     uint32 key = 1;
///     PropValue value = 2;
/// }
/// ```
fn parse_map_entry(bytes: &[u8]) -> Option<(u32, i64)> {
    let mut reader = Reader::new(bytes);
    let mut key: Option<u32> = None;
    let mut value: Option<i64> = None;
    while let Some((field_number, wire_type, varint, field_bytes)) = reader.next_field() {
        match (field_number, wire_type) {
            (1, WireType::Varint) => key = Some(varint as u32),
            (2, WireType::LengthDelimited) => value = parse_prop_value(field_bytes),
            _ => return None,
        }
    }
    match (key, value) {
        (Some(key), Some(value)) => Some((key, value)),
        _ => None,
    }
}

/// Recursively collect `map<uint32, PropValue>` entries from `data`.
///
/// Walks the fields of the message and interprets each length-delimited field
/// as a map entry; fields that are not map entries may themselves be messages
/// containing the prop map (e.g. player data wrapped in an outer message), so
/// they are scanned up to `depth` levels deep.  Recursion is bounded to keep
/// pathological inputs cheap.
fn collect_props(data: &[u8], depth: u32, props: &mut BTreeMap<u32, i64>) {
    let mut reader = Reader::new(data);

    while let Some((_, wire_type, _, field_bytes)) = reader.next_field() {
        if wire_type != WireType::LengthDelimited {
            continue;
        }
        if let Some((key, value)) = parse_map_entry(field_bytes) {
            props.insert(key, value);
        } else if depth > 0 {
            collect_props(field_bytes, depth - 1, props);
        }
    }
}

/// Result of parsing the player-data message: the currency prop map plus the
/// player's nickname (which travels in the same message).
pub struct PlayerDataPacket {
    pub props: BTreeMap<u32, i64>,
    pub nick_name: Option<String>,
}

/// Extract incremental player-prop updates from a prop-update notify.
///
/// These are pushed by the server whenever a player prop changes (e.g. the
/// primogem count after spending).  Unlike the full player-data packet, the
/// notify only carries the changed props, as repeated `map<uint32, PropValue>`
/// entries at the top level -- so the pair gate of [`extract_player_props`]
/// does not apply.  We accept any top-level map with at least one key in the
/// player-prop namespace.  Avatar prop maps (keys 1001..4001, nested inside
/// avatar messages) do not match: their entries are not at the top level and
/// their keys fall outside the namespace.
pub fn extract_prop_updates(data: &[u8]) -> Option<BTreeMap<u32, i64>> {
    let mut props = BTreeMap::new();
    // No recursion: update notifies carry their entries directly at the top
    // level (field 1).
    collect_props(data, 0, &mut props);

    props.retain(|&key, _| MODERN_PROP_RANGE.contains(&key) || LEGACY_PROP_IDS.contains(&key));
    if props.is_empty() {
        None
    } else {
        Some(props)
    }
}

/// Extract item count changes from a store-item-change notify.
///
/// When an item count changes during play (e.g. spending or gaining fates
/// and starglitter), the server pushes each changed item as a top-level
/// `Item` entry -- `{1: item_id, 2: guid, 5: Material { 1: count }}` -- with
/// its new count.  Returns `(item_id, guid, count)` triples; the guid lets us
/// recognize later removal notifies, which carry guid only.
pub fn extract_item_changes(data: &[u8]) -> Option<Vec<(u32, u64, u32)>> {
    let mut reader = Reader::new(data);
    let mut changes = Vec::new();
    while let Some((_, wire_type, _, field_bytes)) = reader.next_field() {
        if wire_type != WireType::LengthDelimited {
            continue;
        }
        if let Some((item_id, guid, count)) = parse_item_with_count(field_bytes) {
            changes.push((item_id, guid, count));
        }
    }
    if changes.is_empty() {
        None
    } else {
        Some(changes)
    }
}

/// Extract the guids of removed items from a store-item-del notify.
///
/// When an item's count reaches zero it is removed from the store, and the
/// server pushes a notify that carries only the removed instance guids (each
/// a varint wrapped in a length-delimited field), with no item id.  We map
/// the guid back to its item id using associations learned from the store
/// sync and change notifies.
pub fn extract_removed_item_guids(data: &[u8]) -> Option<Vec<u64>> {
    let mut reader = Reader::new(data);
    let mut guids = Vec::new();
    while let Some((_, wire_type, _, field_bytes)) = reader.next_field() {
        if wire_type != WireType::LengthDelimited {
            continue;
        }
        let mut inner = Reader::new(field_bytes);
        // A guid is a single large varint filling the whole field.
        if let Some(guid) = inner.read_varint()
            && inner.remaining() == 0
            && guid > (1 << 40)
        {
            guids.push(guid);
        }
    }
    if guids.is_empty() {
        None
    } else {
        Some(guids)
    }
}

/// Decode a single `Item`-shaped message into `(item_id, guid, count)`.
fn parse_item_with_count(data: &[u8]) -> Option<(u32, u64, u32)> {
    let mut reader = Reader::new(data);
    let mut item_id: Option<u32> = None;
    let mut guid: Option<u64> = None;
    let mut count: Option<u32> = None;
    while let Some((field_number, wire_type, varint, field_bytes)) = reader.next_field() {
        match (field_number, wire_type) {
            (1, WireType::Varint) => item_id = Some(varint as u32),
            (2, WireType::Varint) => guid = Some(varint),
            (5, WireType::LengthDelimited) => {
                let mut material = Reader::new(field_bytes);
                while let Some((f2, wt2, v2, _)) = material.next_field() {
                    if (f2, wt2) == (1, WireType::Varint) {
                        count = Some(v2 as u32);
                    }
                }
            }
            _ => (),
        }
    }
    match (item_id, guid, count) {
        (Some(item_id), Some(guid), Some(count)) => Some((item_id, guid, count)),
        _ => None,
    }
}

/// Extract the full player-data message: props plus nickname.
///
/// The nickname is a plain UTF-8 string field at the top level of the same
/// message as the prop map.  See [`extract_player_props`] for the message
/// acceptance criteria.
pub fn extract_player_packet(data: &[u8]) -> Option<PlayerDataPacket> {
    let props = extract_player_props(data)?;
    Some(PlayerDataPacket {
        props,
        nick_name: find_nickname(data),
    })
}

/// Scan the top-level length-delimited fields of the player-data message for
/// a plausible nickname: a short (<= 64 byte) UTF-8 string containing at
/// least one alphanumeric character.  The prop-map entries are skipped; of
/// the remaining fields, the longest candidate wins.
fn find_nickname(data: &[u8]) -> Option<String> {
    let mut reader = Reader::new(data);
    let mut best: Option<String> = None;
    while let Some((_, wire_type, _, field_bytes)) = reader.next_field() {
        if wire_type != WireType::LengthDelimited || parse_map_entry(field_bytes).is_some() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(field_bytes) else {
            continue;
        };
        if s.is_empty() || s.len() > 64 || !s.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        if best.as_ref().is_none_or(|b| s.len() > b.len()) {
            best = Some(s.to_string());
        }
    }
    best
}

/// Extract player properties from the payload of a player-data command.
///
/// The message is only accepted when it contains the primogems and mora
/// properties under either the 7.0 ids (10015/10016) or the pre-7.0 ids
/// (201/202).  The server always sends both of a pair together in the
/// player-data message, and no other command carries either pair, so this
/// signature is specific enough that false positives are not a practical
/// concern.
pub fn extract_player_props(data: &[u8]) -> Option<BTreeMap<u32, i64>> {
    let mut props = BTreeMap::new();
    collect_props(data, 2, &mut props);

    let modern = props.contains_key(&PROP_PRIMOGEM) && props.contains_key(&PROP_MORA);
    let legacy = props.contains_key(&PROP_HCOIN) && props.contains_key(&PROP_SCOIN);
    if modern || legacy {
        Some(props)
    } else {
        None
    }
}

/// Read the value of the first prop id that is present.
pub fn prop_value(props: &BTreeMap<u32, i64>, ids: (u32, u32)) -> Option<i64> {
    props.get(&ids.0).or_else(|| props.get(&ids.1)).copied()
}

/// Read the value of the first of `ids` that is present.
pub fn prop_value_any(props: &BTreeMap<u32, i64>, ids: &[u32]) -> Option<i64> {
    ids.iter().find_map(|id| props.get(id).copied())
}

/// Encoders for the wire format, used by the tests to build realistic
/// payloads without a capture of the live game.
#[cfg(test)]
pub(crate) mod test_enc {
    pub fn varint(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    pub fn tag(field: u32, wire_type: u8, out: &mut Vec<u8>) {
        varint(u64::from(field) << 3 | u64::from(wire_type), out);
    }

    pub fn uint(field: u32, value: u64, out: &mut Vec<u8>) {
        tag(field, 0, out);
        varint(value, out);
    }

    pub fn bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }

    pub fn string(field: u32, value: &str, out: &mut Vec<u8>) {
        bytes(field, value.as_bytes(), out);
    }

    /// `MapEntry { 1: key, 2: PropValue { 2: ival } }` as field 1 of a map.
    pub fn prop_entry(prop_id: u32, value: i64, out: &mut Vec<u8>) {
        let mut prop = Vec::new();
        uint(2, value as u64, &mut prop);
        let mut entry = Vec::new();
        uint(1, u64::from(prop_id), &mut entry);
        bytes(2, &prop, &mut entry);
        bytes(1, &entry, out);
    }

    /// A full player-data notify: prop map entries plus a nickname.
    pub fn player_data_notify(props: &[(u32, i64)], nickname: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        for (id, value) in props {
            prop_entry(*id, *value, &mut out);
        }
        if let Some(nickname) = nickname {
            string(4, nickname, &mut out);
        }
        out
    }

    /// An incremental prop update notify.
    pub fn prop_notify(props: &[(u32, i64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (id, value) in props {
            prop_entry(*id, *value, &mut out);
        }
        out
    }

    /// `Item { 1: item_id, 2: guid, 5: Material { 1: count } }`.
    pub fn item(item_id: u32, guid: u64, count: u32) -> Vec<u8> {
        let mut material = Vec::new();
        uint(1, u64::from(count), &mut material);
        let mut out = Vec::new();
        uint(1, u64::from(item_id), &mut out);
        uint(2, guid, &mut out);
        bytes(5, &material, &mut out);
        out
    }

    /// A store notify carrying full items.
    ///
    /// The items travel in field 6 (`PacketWithItems.items`), which is where the
    /// 7.1 game update moved them; it was field 5 before.  The extractor is
    /// deliberately blind to the field number, so this only matters for keeping
    /// the synthetic packet shaped like the real one.
    pub fn store_notify(items: &[(u32, u64, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (item_id, guid, count) in items {
            bytes(6, &item(*item_id, *guid, *count), &mut out);
        }
        out
    }

    /// A store-item-change notify.  Each changed item travels in its own
    /// length-delimited field (`StoreItemChangeNotify.item_list`, field 1);
    /// the extractor is deliberately blind to the field number.
    pub fn item_change_notify(items: &[(u32, u64, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (item_id, guid, count) in items {
            bytes(1, &item(*item_id, *guid, *count), &mut out);
        }
        out
    }

    /// A store-item-del notify (guid only, each wrapped in field 1).
    pub fn item_del_notify(guids: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        for guid in guids {
            let mut inner = Vec::new();
            varint(*guid, &mut inner);
            bytes(1, &inner, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_enc::*;
    use super::*;

    const MODERN: [(u32, i64); 3] = [
        (PROP_PRIMOGEM, 12_480),
        (PROP_MORA, 3_500_000),
        (PROP_GENESIS_CRYSTAL, 980),
    ];

    #[test]
    fn extracts_modern_player_props_and_nickname() {
        let payload = player_data_notify(&MODERN, Some("Traveler"));
        let packet = extract_player_packet(&payload).expect("player data should be recognised");
        assert_eq!(prop_value(&packet.props, PRIMOGEM_PROP_IDS), Some(12_480));
        assert_eq!(prop_value(&packet.props, MORA_PROP_IDS), Some(3_500_000));
        assert_eq!(prop_value(&packet.props, GENESIS_CRYSTAL_PROP_IDS), Some(980));
        assert_eq!(packet.nick_name.as_deref(), Some("Traveler"));
    }

    #[test]
    fn extracts_legacy_player_props() {
        let payload = player_data_notify(&[(PROP_HCOIN, 100), (PROP_SCOIN, 200), (PROP_MCOIN, 30)], None);
        let packet = extract_player_packet(&payload).expect("legacy player data should be recognised");
        assert_eq!(prop_value(&packet.props, PRIMOGEM_PROP_IDS), Some(100));
        assert_eq!(prop_value(&packet.props, MORA_PROP_IDS), Some(200));
        assert_eq!(prop_value(&packet.props, GENESIS_CRYSTAL_PROP_IDS), Some(30));
    }

    /// A prop map without both primogems and mora is not the player-data
    /// message, so unrelated packets are not mistaken for it.
    #[test]
    fn rejects_unrelated_messages() {
        let payload = player_data_notify(&[(PROP_PRIMOGEM, 10)], None);
        assert!(extract_player_packet(&payload).is_none());
        // An avatar message: nested prop map with keys outside the namespace.
        let mut avatar = Vec::new();
        prop_entry(2001, 5, &mut avatar);
        let mut nested = Vec::new();
        bytes(3, &avatar, &mut nested);
        assert!(extract_player_packet(&nested).is_none());
    }

    #[test]
    fn extracts_incremental_prop_updates() {
        let payload = prop_notify(&[(PROP_PRIMOGEM, 12_640), (PROP_MORA, 3_499_000)]);
        let props = extract_prop_updates(&payload).expect("prop update should be recognised");
        assert_eq!(props.get(&PROP_PRIMOGEM), Some(&12_640));
        assert_eq!(props.get(&PROP_MORA), Some(&3_499_000));

        // Legacy ids survive the namespace filter too.
        let payload = prop_notify(&[(PROP_HCOIN, 5)]);
        assert_eq!(extract_prop_updates(&payload).and_then(|p| p.get(&PROP_HCOIN).copied()), Some(5));
    }

    /// The resin id is the one prop id in this file that came out of a capture,
    /// so it is worth pinning: the game moving it is exactly the event that
    /// requires another capture, and a silent edit to this number without one
    /// would just show a wrong value with no evidence behind it.
    #[test]
    fn the_resin_prop_id_comes_from_a_capture() {
        assert_eq!(PROP_ORIGINAL_RESIN, 10020);
    }

    /// 原粹树脂 is the one prop that is not part of a `(modern, legacy)` pair:
    /// the modern id has to survive the incremental-update filter, and so does
    /// the pre-7.0 one, which predates the 100xx namespace entirely.
    #[test]
    fn a_resin_prop_update_survives_the_namespace_filter() {
        let payload = prop_notify(&[(PROP_ORIGINAL_RESIN, 137), (1_001, 9)]);
        let props = extract_prop_updates(&payload).expect("resin should be recognised");
        assert_eq!(prop_value_any(&props, &RESIN_PROP_IDS), Some(137));
        assert!(!props.contains_key(&1_001), "an avatar prop is not a player prop");

        let payload = prop_notify(&[(PROP_LEGACY_ORIGINAL_RESIN, 12)]);
        let props = extract_prop_updates(&payload).expect("legacy resin should be recognised");
        assert_eq!(prop_value_any(&props, &RESIN_PROP_IDS), Some(12));

        // The 7.0 id is the one that is preferred when both are somehow present.
        let both = BTreeMap::from([(PROP_ORIGINAL_RESIN, 40), (PROP_LEGACY_ORIGINAL_RESIN, 12)]);
        assert_eq!(prop_value_any(&both, &RESIN_PROP_IDS), Some(40));
    }

    #[test]
    fn extracts_item_changes() {
        let payload = item_change_notify(&[(223, 0x1234_5678_9abc, 4), (221, 0x1234_5678_9abd, 17)]);
        let changes = extract_item_changes(&payload).expect("item change should be recognised");
        assert_eq!(changes, [(223, 0x1234_5678_9abc, 4), (221, 0x1234_5678_9abd, 17)]);
    }

    #[test]
    fn extracts_removed_item_guids() {
        let guids = [0x1_0000_0000_11u64, 0x1_0000_0000_22u64];
        let payload = item_del_notify(&guids);
        assert_eq!(extract_removed_item_guids(&payload).unwrap(), guids);
        // A guid-sized varint that is too small is not a guid.
        assert!(extract_removed_item_guids(&item_del_notify(&[7])).is_none());
    }

    /// The three command shapes must not be confused with each other: a prop
    /// update is not an item change, and vice versa.
    #[test]
    fn command_shapes_are_distinct() {
        let props = prop_notify(&[(PROP_PRIMOGEM, 1)]);
        assert!(extract_item_changes(&props).is_none(), "prop update read as item change");
        assert!(extract_removed_item_guids(&props).is_none(), "prop update read as item removal");
        assert!(extract_player_packet(&props).is_none(), "prop update read as full player data");

        let items = item_change_notify(&[(223, 0x1_0000_0000_11, 2)]);
        assert!(extract_prop_updates(&items).is_none(), "item change read as prop update");
        assert!(extract_player_packet(&items).is_none(), "item change read as full player data");
    }

    /// Truncated input must be rejected instead of panicking.
    #[test]
    fn truncated_input_is_safe() {
        let payload = player_data_notify(&MODERN, Some("Traveler"));
        for cut in 0..payload.len() {
            let _ = extract_player_packet(&payload[..cut]);
            let _ = extract_prop_updates(&payload[..cut]);
            let _ = extract_item_changes(&payload[..cut]);
            let _ = extract_removed_item_guids(&payload[..cut]);
        }
    }
}
