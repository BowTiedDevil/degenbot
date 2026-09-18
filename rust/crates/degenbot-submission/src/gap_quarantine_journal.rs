//! Durable parking journal for the gap quarantine.
//!
//! The quarantine FSM is in-memory, so a process restart loses every parked
//! frame. This module persists parks and their resolutions as JSON Lines under
//! the typed `persistence.state_dir` root (default the XDG state home:
//! `$XDG_STATE_HOME` when absolute, else `$HOME/.local/state`, then
//! `degenbot/state`)
//! — OUTSIDE the per-session run directories, because the journal must outlive
//! the session that wrote it. On the next boot the still-pending parks re-enter
//! the live FSM exactly as they left it (tracked or tentative); nothing is
//! dropped for age.
//!
//! # Persistence model
//!
//! Append-on-change with resolution tombstones, compacted once at boot:
//!
//! - A park appends `{"v":2,"kind":"park", ...}` carrying the full feed event
//!   plus the claimed/expected nonce and the wall clock.
//! - A nonce-consumption classification appends `{"kind":"tentative", ...}`
//!   naming the frame hash and the carrying block (hash + kind), so a restart
//!   mid-finality-window reconstructs the tentative set.
//! - A resolution (`mined_finalized`, `slot_taken_finalized`,
//!   `rescue_consumed`, `evicted`) appends `{"kind":"resolve", ...}` naming
//!   the frame hash.
//! - Each record is one [`std::io::Write::write_all`] on the append handle, so
//!   a crash cannot interleave two records and a torn tail is at most one
//!   partial line.
//! - The loader folds parks and resolves into the pending set (tombstones
//!   remove their park). Boot then atomically rewrites the file to just the
//!   still-pending parks — this is the compaction pass.
//!
//! # Versioning and never-destroy
//!
//! Records carry `v` ([`JOURNAL_RECORD_VERSION`]); a line without it is
//! version 1, the format before versioning. A version this binary does not
//! fully understand is not a death sentence: a park-shaped line is migrated
//! into a [`ParkRecord`] and stays tracked (degraded when the wire cannot be
//! rebuilt), and anything the fold cannot classify is appended verbatim
//! to a `.corrupt` sidecar before compaction runs — a frame cannot die at boot
//! for the parser's ignorance, and rejected bytes are never overwritten.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use crate::gap_quarantine::{NonceConsumed, ParkedFrame};
use degenbot_rpc::backrun_feed::BackrunFeedEvent;

/// Fixed journal filename under the durable-state root.
pub const JOURNAL_FILE_NAME: &str = "backrun-quarantine.jsonl";

/// The record schema version this binary writes. A line without an explicit
/// `v` predates versioning and is read as [`LEGACY_RECORD_VERSION`].
pub const JOURNAL_RECORD_VERSION: u32 = 2;

/// The implicit version of a line that carries no `v` field.
const LEGACY_RECORD_VERSION: u32 = 1;

/// How much of a migrated record's wire survived. A degraded park keeps only
/// identity, so later stages know the feed event cannot be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordFidelity {
    /// The wire shape survives; the feed event can be rebuilt.
    #[default]
    Full,
    /// Only identity survives (hash, nonce, and the sender when readable);
    /// no event reconstruction is possible.
    Degraded,
}

impl RecordFidelity {
    /// Whether the record can rebuild its feed event.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Why a parked frame left the quarantine. Only finalized nonce-consumptions
/// (and the operational rescue/eviction) write tombstones -- there is no
/// clock and no pool-absence death.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// The frame's own tx mined and the carrying block finalized.
    MinedFinalized,
    /// A same-nonce tx consumed the slot and the carrying block finalized.
    SlotTakenFinalized,
    /// The gap closed in the caller's pool view (predecessors replayable).
    RescueConsumed,
    /// A forced eviction (sender flush / operator action).
    Evicted,
}

/// The consumption taxonomy stored in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsumedKind {
    /// The frame's own tx mined.
    Mined,
    /// A same-nonce tx took the slot.
    SlotTaken,
}

/// Persisted nonce-consumption evidence: the block + hash for the reorg
/// check and the kind + optional slot-stealer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TentativeRecord {
    pub block: u64,
    pub block_hash: B256,
    pub kind: ConsumedKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<B256>,
}

impl TentativeRecord {
    /// Encode a classification.
    #[must_use]
    pub const fn from_consumed(consumed: NonceConsumed) -> Self {
        match consumed {
            NonceConsumed::MinedAt { block, block_hash } => Self {
                block,
                block_hash,
                kind: ConsumedKind::Mined,
                by: None,
            },
            NonceConsumed::SlotTakenAt {
                block,
                block_hash,
                by,
            } => Self {
                block,
                block_hash,
                kind: ConsumedKind::SlotTaken,
                by,
            },
        }
    }

    /// Decode a classification.
    #[must_use]
    pub const fn to_consumed(self) -> NonceConsumed {
        match self.kind {
            ConsumedKind::Mined => NonceConsumed::MinedAt {
                block: self.block,
                block_hash: self.block_hash,
            },
            ConsumedKind::SlotTaken => NonceConsumed::SlotTakenAt {
                block: self.block,
                block_hash: self.block_hash,
                by: self.by,
            },
        }
    }
}

/// One tentatively-classified park: the frame hash plus its consumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TentativeEntryRecord {
    pub frame_hash: B256,
    pub consumed: TentativeRecord,
    pub entered_at_unix_ms: u64,
}

/// The feed event as it round-trips through the journal. The wire fields are
/// hex strings so a line stays human-readable in the forensics surface; the
/// conversion is lossless against [`BackrunFeedEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFrame {
    pub hash: String,
    pub chain_id: u64,
    pub from: String,
    pub to: Option<String>,
    pub value: String,
    pub data: String,
    pub gas: u64,
    /// Decimal string: `serde_json` cannot deserialize a bare `u128`.
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub nonce: u64,
    pub tx_type: u8,
    pub received_unix_ms: u64,
    pub access_list: serde_json::Value,
}

impl WireFrame {
    /// Encode one feed event.
    #[must_use]
    pub fn encode(event: &BackrunFeedEvent) -> Self {
        Self {
            hash: event.hash.to_string(),
            chain_id: event.chain_id,
            from: event.from.to_string(),
            to: event.to.map(|a| a.to_string()),
            value: format!("0x{:x}", event.value),
            data: format!("0x{}", alloy::hex::encode(&event.data)),
            gas: event.gas,
            max_fee_per_gas: event.max_fee_per_gas.to_string(),
            max_priority_fee_per_gas: event.max_priority_fee_per_gas.to_string(),
            nonce: event.nonce,
            tx_type: event.tx_type,
            received_unix_ms: event.received_unix_ms,
            access_list: event.access_list.clone(),
        }
    }

    /// Decode back to a feed event.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when a hex field does not parse (a hand-edited or
    /// truncated line).
    pub fn decode(&self) -> Result<BackrunFeedEvent, JournalError> {
        let hash = self.hash.parse::<B256>()?;
        let from = self.from.parse::<Address>()?;
        let to = self.to.as_deref().map(str::parse::<Address>).transpose()?;
        let value = parse_u256(&self.value)?;
        let max_fee_per_gas = self
            .max_fee_per_gas
            .parse::<u128>()
            .map_err(|e| JournalError::Field(e.to_string()))?;
        let max_priority_fee_per_gas = self
            .max_priority_fee_per_gas
            .parse::<u128>()
            .map_err(|e| JournalError::Field(e.to_string()))?;
        let data = Bytes::from(alloy::hex::decode(self.data.trim_start_matches("0x"))?);
        Ok(BackrunFeedEvent {
            chain_id: self.chain_id,
            from,
            to,
            value,
            data,
            gas: self.gas,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            nonce: self.nonce,
            hash,
            access_list: self.access_list.clone(),
            tx_type: self.tx_type,
            received_unix_ms: self.received_unix_ms,
        })
    }
}

/// Parse a `0x`-prefixed (or bare) hex `U256`.
fn parse_u256(raw: &str) -> Result<U256, JournalError> {
    let hex = raw.trim();
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    U256::from_str_radix(hex, 16).map_err(|e| JournalError::Field(e.to_string()))
}

/// One park: the full feed event plus the gap edges, wall clock, and optional
/// tentative classification (present once the nonce consumption is known).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParkRecord {
    pub frame: WireFrame,
    pub sender: Address,
    pub claimed_nonce: u64,
    pub expected_nonce: u64,
    pub parked_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tentative: Option<TentativeRecord>,
    /// Wire survival of a migrated record; a degraded park carries only
    /// identity and cannot rebuild the feed event.
    #[serde(default, skip_serializing_if = "RecordFidelity::is_full")]
    pub fidelity: RecordFidelity,
}

impl ParkRecord {
    /// Build a park record from the event that produced the gap.
    #[must_use]
    pub fn new(event: &BackrunFeedEvent, expected_nonce: u64, parked_at_unix_ms: u64) -> Self {
        Self {
            frame: WireFrame::encode(event),
            sender: event.from,
            claimed_nonce: event.nonce,
            expected_nonce,
            parked_at_unix_ms,
            tentative: None,
            fidelity: RecordFidelity::Full,
        }
    }

    /// Attach a nonce-consumption classification (journal reload).
    #[must_use]
    pub const fn with_tentative(mut self, consumed: NonceConsumed) -> Self {
        self.tentative = Some(TentativeRecord::from_consumed(consumed));
        self
    }

    /// Rebuild the feed event a replay needs.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when a hex field fails to parse.
    pub fn to_event(&self) -> Result<BackrunFeedEvent, JournalError> {
        self.frame.decode()
    }

    /// Rebuild the FSM frame. No clock rides the frame: liveness is finality,
    /// so the rebuild is lossless regardless of downtime. A degraded record
    /// rebuilds only its identity and leaves the wire zeroed, so the frame can
    /// never be rescued into the funnel.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when the frame hash fails to parse.
    pub fn to_parked_frame(&self) -> Result<ParkedFrame, JournalError> {
        if self.fidelity == RecordFidelity::Degraded {
            let hash = self.frame.hash.parse::<B256>()?;
            return Ok(ParkedFrame {
                hash,
                from: self.frame.from.parse::<Address>().unwrap_or(Address::ZERO),
                to: self
                    .frame
                    .to
                    .as_deref()
                    .and_then(|raw| raw.parse::<Address>().ok()),
                value: U256::ZERO,
                data: Bytes::new(),
                gas: 0,
                max_fee_per_gas: 0,
                max_priority_fee_per_gas: 0,
                claimed_nonce: self.claimed_nonce,
                expected_at_capture: self.expected_nonce,
                chain_id: 0,
                tx_type: 0,
                access_list: serde_json::json!([]),
                received_unix_ms: 0,
            });
        }
        let event = self.to_event()?;
        Ok(ParkedFrame {
            hash: event.hash,
            from: event.from,
            to: event.to,
            value: event.value,
            data: event.data,
            gas: event.gas,
            max_fee_per_gas: event.max_fee_per_gas,
            max_priority_fee_per_gas: event.max_priority_fee_per_gas,
            claimed_nonce: self.claimed_nonce,
            expected_at_capture: self.expected_nonce,
            chain_id: event.chain_id,
            tx_type: event.tx_type,
            access_list: event.access_list,
            received_unix_ms: event.received_unix_ms,
        })
    }

    /// The original receive time, preserved for forensics.
    #[must_use]
    pub fn original_received_unix_ms(&self) -> u64 {
        self.frame.received_unix_ms
    }
}

/// One resolution tombstone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveRecord {
    pub frame_hash: B256,
    pub resolution: Resolution,
    pub resolved_at_unix_ms: u64,
}

/// One journal line: a version tag plus the tagged payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRecord {
    /// The schema version this line was written with. Missing means version 1.
    #[serde(default = "legacy_record_version", rename = "v")]
    pub version: u32,
    #[serde(flatten)]
    pub payload: JournalRecordPayload,
}

impl JournalRecord {
    /// Wrap a payload with the current schema version.
    #[must_use]
    pub fn new(payload: JournalRecordPayload) -> Self {
        Self {
            version: JOURNAL_RECORD_VERSION,
            payload,
        }
    }
}

const fn legacy_record_version() -> u32 {
    LEGACY_RECORD_VERSION
}

/// The tagged payload of one journal line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalRecordPayload {
    Park(Box<ParkRecord>),
    Tentative(Box<TentativeEntryRecord>),
    Resolve(ResolveRecord),
}

/// What a load found.
#[derive(Debug, Default)]
pub struct PendingRead {
    /// Still-pending parks, ordered by frame hash.
    pub pending: Vec<ParkRecord>,
    /// Lines the fold could not classify; each is preserved in the `.corrupt`
    /// sidecar.
    pub skipped: usize,
    /// Older- or unknown-version parks folded as tracked frames.
    pub migrated: usize,
    /// Migrated parks that kept only identity (hash, nonce, sender when
    /// readable).
    pub degraded: usize,
}

/// One line's classification by the boot fold.
enum Classified {
    /// A record this binary understands; fold it semantically.
    Fold(JournalRecordPayload),
    /// A park-shaped record (any version or field state) rebuilt as a park,
    /// degraded when its wire cannot be reconstructed.
    Migrated {
        record: Box<ParkRecord>,
        fidelity: RecordFidelity,
    },
    /// Genuinely unparseable; preserve verbatim in the sidecar.
    Corrupt,
}

/// The `.corrupt` sibling of `journal_path`. Rejected lines are appended there
/// verbatim, never overwritten, so a later compaction cannot destroy them.
#[must_use]
pub fn corrupt_sidecar_path(journal_path: &Path) -> PathBuf {
    let mut name = journal_path.as_os_str().to_os_string();
    name.push(".corrupt");
    PathBuf::from(name)
}

/// The still-pending parks after folding the journal at `path`. A missing file
/// is an empty journal; a legacy or future park migrates to a tracked record;
/// a corrupt line is preserved in the `.corrupt` sidecar and counted in
/// `skipped`, never fatal.
///
/// # Errors
///
/// I/O errors other than `NotFound`. A failure to preserve a corrupt line is
/// returned rather than dropped: evidence loss is not silent.
pub fn read_pending(path: &Path) -> io::Result<PendingRead> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PendingRead::default());
        }
        Err(error) => return Err(error),
    };
    let mut pending: BTreeMap<String, ParkRecord> = BTreeMap::new();
    let mut skipped = 0usize;
    let mut migrated = 0usize;
    let mut degraded = 0usize;
    let sidecar = corrupt_sidecar_path(path);
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        // A UTF-8 BOM is a byte-order artifact, not data: strip it for the
        // parse only, preserving the raw line byte-for-byte in the sidecar.
        let parse_line = line.strip_prefix('\u{feff}').unwrap_or(line);
        match classify_line(parse_line) {
            Classified::Fold(payload) => apply_fold(&mut pending, payload),
            Classified::Migrated { record, fidelity } => {
                migrated += 1;
                if fidelity == RecordFidelity::Degraded {
                    degraded += 1;
                }
                pending.insert(record.frame.hash.clone(), *record);
            }
            Classified::Corrupt => {
                skipped += 1;
                tracing::warn!(
                    path = %path.display(),
                    line = index + 1,
                    "quarantine journal line unparseable - preserved in .corrupt sidecar"
                );
                append_corrupt(&sidecar, line)?;
            }
        }
    }
    if migrated > 0 {
        tracing::info!(
            path = %path.display(),
            migrated,
            degraded,
            "quarantine journal legacy records migrated as tracked parks"
        );
    }
    Ok(PendingRead {
        pending: pending.into_values().collect(),
        skipped,
        migrated,
        degraded,
    })
}

/// Classify one journal line by sampled version and shape before any typed
/// deserialization can reject it.
fn classify_line(line: &str) -> Classified {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Classified::Corrupt;
    };
    if sample_version(&value) <= JOURNAL_RECORD_VERSION {
        if let Ok(payload) = serde_json::from_value::<JournalRecordPayload>(value.clone()) {
            return Classified::Fold(payload);
        }
    }
    // A park-shaped line is never junk: whatever the version or field state,
    // fold it as a park, degrading the wire it cannot rebuild.
    if park_shaped(&value) {
        if let Some((record, fidelity)) = migrate_legacy_park(&value) {
            return Classified::Migrated {
                record: Box::new(record),
                fidelity,
            };
        }
    }
    Classified::Corrupt
}

/// The record's schema version; a missing `v` is the pre-versioning format.
fn sample_version(value: &serde_json::Value) -> u32 {
    match value.get("v") {
        None => LEGACY_RECORD_VERSION,
        Some(raw) => raw
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(u32::MAX),
    }
}

fn apply_fold(pending: &mut BTreeMap<String, ParkRecord>, payload: JournalRecordPayload) {
    match payload {
        JournalRecordPayload::Park(mut record) => {
            if record.fidelity == RecordFidelity::Full && !wire_is_full(&record.frame) {
                record.fidelity = RecordFidelity::Degraded;
            }
            pending.insert(record.frame.hash.clone(), *record);
        }
        JournalRecordPayload::Tentative(record) => {
            if let Some(park) = pending.get_mut(&record.frame_hash.to_string()) {
                park.tentative = Some(record.consumed);
            }
        }
        JournalRecordPayload::Resolve(record) => {
            pending.remove(&record.frame_hash.to_string());
        }
    }
}

/// Whether a value looks like a park even if the current schema rejects it.
fn park_shaped(value: &serde_json::Value) -> bool {
    value.get("kind").and_then(serde_json::Value::as_str) == Some("park")
}

fn append_corrupt(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")
}

/// A park as an unknown-version writer shaped it: every field optional so a
/// missing one degrades instead of rejecting the line.
#[derive(Debug, Default, Deserialize)]
struct LegacyParkRecord {
    frame: Option<LegacyWireFrame>,
    sender: Option<Address>,
    claimed_nonce: Option<u64>,
    expected_nonce: Option<u64>,
    parked_at_unix_ms: Option<u64>,
    hash: Option<B256>,
    nonce: Option<u64>,
    tentative: Option<serde_json::Value>,
}

/// The wire fields a legacy park might carry. All optional: whatever survives
/// is folded, and hash-plus-nonce alone still yields a degraded tracked park.
#[derive(Debug, Default, Deserialize)]
struct LegacyWireFrame {
    hash: Option<String>,
    chain_id: Option<u64>,
    from: Option<String>,
    to: Option<String>,
    value: Option<String>,
    data: Option<String>,
    gas: Option<u64>,
    max_fee_per_gas: Option<String>,
    max_priority_fee_per_gas: Option<String>,
    nonce: Option<u64>,
    tx_type: Option<u8>,
    received_unix_ms: Option<u64>,
    access_list: Option<serde_json::Value>,
}

/// Rebuild a tracked park from a park-shaped line the current schema rejects.
/// The wire is FULL only when every field survives and decodes to a
/// non-degenerate value; any shortcoming degrades to identity only. Returns
/// `None` when not even the hash and claimed nonce survive.
fn migrate_legacy_park(value: &serde_json::Value) -> Option<(ParkRecord, RecordFidelity)> {
    let legacy: LegacyParkRecord = serde_json::from_value(value.clone()).ok()?;
    let frame = legacy.frame.unwrap_or_default();
    let hash = frame
        .hash
        .as_deref()
        .and_then(|raw| raw.parse::<B256>().ok())
        .or(legacy.hash)?;
    let claimed_nonce = legacy.claimed_nonce.or(frame.nonce).or(legacy.nonce)?;
    let sender = legacy
        .sender
        .or_else(|| {
            frame
                .from
                .as_deref()
                .and_then(|raw| raw.parse::<Address>().ok())
        })
        .unwrap_or(Address::ZERO);
    let (wire, fidelity) = match full_wire_candidate(&frame, hash, claimed_nonce) {
        Some(wire) => (wire, RecordFidelity::Full),
        None => (
            degraded_wire(&frame, hash, claimed_nonce),
            RecordFidelity::Degraded,
        ),
    };
    let tentative = legacy
        .tentative
        .and_then(|raw| serde_json::from_value::<TentativeRecord>(raw).ok());
    Some((
        ParkRecord {
            frame: wire,
            sender,
            claimed_nonce,
            expected_nonce: legacy.expected_nonce.unwrap_or(claimed_nonce),
            parked_at_unix_ms: legacy.parked_at_unix_ms.unwrap_or(0),
            tentative,
            fidelity,
        },
        fidelity,
    ))
}

/// The full wire a legacy park carries, when every field is present and
/// decodes to a non-degenerate value. `None` on the first shortcoming.
fn full_wire_candidate(frame: &LegacyWireFrame, hash: B256, nonce: u64) -> Option<WireFrame> {
    let wire = WireFrame {
        hash: hash.to_string(),
        chain_id: frame.chain_id?,
        from: frame.from.clone()?,
        to: frame.to.clone(),
        value: frame.value.clone()?,
        data: frame.data.clone()?,
        gas: frame.gas?,
        max_fee_per_gas: frame.max_fee_per_gas.clone()?,
        max_priority_fee_per_gas: frame.max_priority_fee_per_gas.clone()?,
        nonce,
        tx_type: frame.tx_type?,
        received_unix_ms: frame.received_unix_ms?,
        access_list: frame.access_list.clone()?,
    };
    wire_is_full(&wire).then_some(wire)
}

/// A degraded park keeps its identity (hash, nonce, and the sender when
/// readable); the wire defaults are zeroed so no event can be fabricated.
fn degraded_wire(frame: &LegacyWireFrame, hash: B256, nonce: u64) -> WireFrame {
    WireFrame {
        hash: hash.to_string(),
        chain_id: 0,
        from: frame
            .from
            .clone()
            .unwrap_or_else(|| Address::ZERO.to_string()),
        to: frame.to.clone(),
        value: String::from("0x0"),
        data: String::from("0x"),
        gas: 0,
        max_fee_per_gas: String::from("0"),
        max_priority_fee_per_gas: String::from("0"),
        nonce,
        tx_type: 0,
        received_unix_ms: 0,
        access_list: serde_json::json!([]),
    }
}

/// A wire that can rebuild a real feed event: every field decodes and the
/// identity fields are non-degenerate. A fabricated default is never FULL.
fn wire_is_full(wire: &WireFrame) -> bool {
    let Ok(event) = wire.decode() else {
        return false;
    };
    event.hash != B256::ZERO
        && event.from != Address::ZERO
        && event.chain_id != 0
        && event.gas != 0
        && wire.access_list.is_array()
}

/// Atomically rewrite `path` to exactly `pending`, creating parent directories.
/// Writes via a sibling temp file + rename so a crash mid-write leaves the
/// previous journal intact.
///
/// # Errors
///
/// Filesystem errors from the write, sync, or rename.
pub fn compact(path: &Path, pending: &[ParkRecord]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = Vec::new();
    for record in pending {
        let line = JournalRecord::new(JournalRecordPayload::Park(Box::new(record.clone())));
        serde_json::to_writer(&mut body, &line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        body.push(b'\n');
    }
    let tmp = temp_path(path);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Append handle for the live journal. One process owns it (the sidecar is a
/// singleton); every append is a single `write_all` on the append handle.
#[derive(Debug)]
pub struct QuarantineJournal {
    file: File,
    path: PathBuf,
}

impl QuarantineJournal {
    /// Open (or create) the journal at `path` for appending.
    ///
    /// # Errors
    ///
    /// Filesystem errors from creating the parent directory or the file.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Open the default journal under the typed durable-state root.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], plus config resolution failure.
    pub fn open_default() -> io::Result<Self> {
        let root = degenbot_runs::resolve_state_root()?;
        Self::open(&root.join(JOURNAL_FILE_NAME))
    }

    /// The journal file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one park.
    ///
    /// # Errors
    ///
    /// Serialization or I/O failure.
    pub fn record_park(&mut self, record: &ParkRecord) -> io::Result<()> {
        self.append(&JournalRecord::new(JournalRecordPayload::Park(Box::new(
            record.clone(),
        ))))
    }

    /// Append one resolution tombstone.
    ///
    /// # Errors
    ///
    /// Serialization or I/O failure.
    pub fn record_resolve(
        &mut self,
        frame_hash: B256,
        resolution: Resolution,
        resolved_at_unix_ms: u64,
    ) -> io::Result<()> {
        self.append(&JournalRecord::new(JournalRecordPayload::Resolve(
            ResolveRecord {
                frame_hash,
                resolution,
                resolved_at_unix_ms,
            },
        )))
    }

    /// Append one tentative classification for an existing park.
    ///
    /// # Errors
    ///
    /// Serialization or I/O failure.
    pub fn record_tentative(
        &mut self,
        frame_hash: B256,
        consumed: NonceConsumed,
        entered_at_unix_ms: u64,
    ) -> io::Result<()> {
        self.append(&JournalRecord::new(JournalRecordPayload::Tentative(
            Box::new(TentativeEntryRecord {
                frame_hash,
                consumed: TentativeRecord::from_consumed(consumed),
                entered_at_unix_ms,
            }),
        )))
    }

    fn append(&mut self, record: &JournalRecord) -> io::Result<()> {
        let mut line = serde_json::to_vec(record)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        line.push(b'\n');
        self.file.write_all(&line)
    }
}

/// Error decoding a journal field.
#[derive(Debug)]
pub enum JournalError {
    /// A hex/numeric field failed to parse.
    Field(String),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Field(message) => write!(f, "journal field: {message}"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<alloy::hex::FromHexError> for JournalError {
    fn from(error: alloy::hex::FromHexError) -> Self {
        JournalError::Field(error.to_string())
    }
}

impl From<alloy::primitives::ruint::ParseError> for JournalError {
    fn from(error: alloy::primitives::ruint::ParseError) -> Self {
        JournalError::Field(error.to_string())
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("journal"),
        std::ffi::OsString::from,
    );
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "test fixtures and assertions fail loudly"
    )]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn scratch(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "degenbot-quarantine-journal-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn event(hash_byte: u8, nonce: u64, received_unix_ms: u64) -> BackrunFeedEvent {
        let mut raw = [0u8; 32];
        raw[0] = hash_byte;
        BackrunFeedEvent {
            chain_id: 1,
            from: Address::with_last_byte(7),
            to: Some(Address::with_last_byte(9)),
            value: U256::from(123u64),
            data: Bytes::from(vec![0xab, 0xcd]),
            gas: 300_000,
            max_fee_per_gas: 514_684_409,
            max_priority_fee_per_gas: 1_000_000_000,
            nonce,
            hash: B256::from(raw),
            access_list: serde_json::json!([]),
            tx_type: 2,
            received_unix_ms,
        }
    }

    #[test]
    fn wire_frame_round_trips_the_feed_event() {
        let original = event(1, 12, 1_700_000_000_000);
        let decoded = WireFrame::encode(&original).decode().expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn park_and_resolve_fold_to_pending() {
        let path = scratch("fold").join(JOURNAL_FILE_NAME);
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let first = ParkRecord::new(&event(1, 12, 1_000), 10, now_ms());
        let second = ParkRecord::new(&event(2, 20, 2_000), 18, now_ms());
        journal.record_park(&first).expect("park 1");
        journal.record_park(&second).expect("park 2");
        journal
            .record_resolve(
                second.frame.hash.parse().expect("hash"),
                Resolution::MinedFinalized,
                now_ms(),
            )
            .expect("resolve");

        let read = read_pending(&path).expect("read");
        assert_eq!(read.skipped, 0);
        assert_eq!(read.pending.len(), 1);
        assert_eq!(read.pending[0].claimed_nonce, 12);
        assert_eq!(read.pending[0].original_received_unix_ms(), 1_000);

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn corrupt_tail_is_skipped_and_compaction_heals_it() {
        let path = scratch("corrupt").join(JOURNAL_FILE_NAME);
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let record = ParkRecord::new(&event(3, 30, 5_000), 28, now_ms());
        journal.record_park(&record).expect("park");
        {
            use std::fs::OpenOptions;
            let mut file = OpenOptions::new().append(true).open(&path).expect("reopen");
            file.write_all(b"{\"kind\":\"pa").expect("write torn tail");
        }

        let read = read_pending(&path).expect("read");
        assert_eq!(read.skipped, 1, "the torn line is skipped, not fatal");
        assert_eq!(read.pending.len(), 1);

        compact(&path, &read.pending).expect("compact");
        let healed = read_pending(&path).expect("read healed");
        assert_eq!(healed.skipped, 0);
        assert_eq!(healed.pending.len(), 1);
        assert_eq!(healed.pending[0].claimed_nonce, 30);

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    #[test]
    fn missing_journal_reads_as_empty_and_compact_creates_it() {
        let dir = scratch("missing");
        let path = dir.join(JOURNAL_FILE_NAME);
        let read = read_pending(&path).expect("read missing");
        assert!(read.pending.is_empty());
        assert_eq!(read.skipped, 0);

        let record = ParkRecord::new(&event(4, 40, 6_000), 39, now_ms());
        compact(&path, std::slice::from_ref(&record)).expect("compact creates");
        assert!(path.is_file());
        assert_eq!(read_pending(&path).expect("read").pending.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parked_frame_rebuild_is_clock_free_and_lossless() {
        let record = ParkRecord::new(&event(5, 50, 7_000), 48, now_ms());
        let frame = record.to_parked_frame().expect("parked frame");
        assert_eq!(frame.expected_at_capture, 48);
        assert_eq!(frame.claimed_nonce, 50);
    }

    #[test]
    fn tentative_record_folds_onto_its_park_and_compacts() {
        let path = scratch("tentative").join(JOURNAL_FILE_NAME);
        let mut journal = QuarantineJournal::open(&path).expect("open");
        let record = ParkRecord::new(&event(6, 60, 8_000), 58, now_ms());
        journal.record_park(&record).expect("park");
        let consumed = NonceConsumed::SlotTakenAt {
            block: 21_000_000,
            block_hash: B256::from([0xab; 32]),
            by: None,
        };
        journal
            .record_tentative(record.frame.hash.parse().expect("hash"), consumed, now_ms())
            .expect("tentative");

        let read = read_pending(&path).expect("read");
        let parked = &read.pending[0];
        assert_eq!(
            parked.tentative.map(TentativeRecord::to_consumed),
            Some(consumed)
        );

        // Compaction writes the tentative state inline; the fold still reads it.
        compact(&path, &read.pending).expect("compact");
        let reread = read_pending(&path).expect("reread");
        assert_eq!(
            reread.pending[0]
                .tentative
                .map(TentativeRecord::to_consumed),
            Some(consumed)
        );
        assert_eq!(reread.skipped, 0);

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }
}
