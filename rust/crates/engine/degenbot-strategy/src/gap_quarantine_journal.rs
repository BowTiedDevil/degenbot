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
//! - A park appends `{"v":3,"kind":"park","wire":"complete", ...}` carrying the
//!   complete feed wire plus the identity (hash, sender, claimed/expected
//!   nonce) and the wall clock. A park whose wire did not survive a migration
//!   appends `{"v":3,"kind":"park","wire":"degraded","reason":"...", ...}`
//!   carrying identity and the missing-wire reason and NO wire fields at all.
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
//! into a [`ParkRecord`] and stays tracked — as a [`ParkWire::Complete`] park
//! when every wire field survives and its own `fidelity` label does not claim
//! otherwise, else as a [`ParkWire::Degraded`] park carrying
//! [`MissingWire::LegacyIncomplete`]. The label decides, never a zero value: a
//! real zero is a value, and a label this binary does not recognize is
//! malformed input rather than a full-wire claim. A park-shaped line whose
//! identity (hash, claimed nonce) or wire label does not survive is rejected by
//! the corruption policy below rather than reinterpreted, and anything the fold
//! cannot classify is appended verbatim to a `.corrupt` sidecar before
//! compaction runs — a frame cannot die at boot for the parser's ignorance, and
//! rejected bytes are never overwritten.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use crate::gap_quarantine::{
    CompleteFrame, DegradedFrame, FrameIdentity, FrameWire, MissingWire, NonceConsumed, ParkedFrame,
};
use degenbot_eventhub::PendingTx;

/// Fixed journal filename under the durable-state root.
pub const JOURNAL_FILE_NAME: &str = "backrun-quarantine.jsonl";

/// Fixed resolution-archive filename under the durable-state root, sibling of
/// [`JOURNAL_FILE_NAME`]. Unlike the journal this file is append-only: never
/// compacted, never rewritten, so every resolved outcome survives every
/// restart.
pub const RESOLVED_ARCHIVE_FILE_NAME: &str = "backrun-quarantine-resolved.jsonl";

/// The resolution-archive record schema version. Independent of
/// [`JOURNAL_RECORD_VERSION`]: the archive is a separate surface that never
/// migrates or compacts.
pub const RESOLVED_ARCHIVE_RECORD_VERSION: u32 = 1;

/// The record schema version this binary writes. A line without an explicit
/// `v` predates versioning and is read as [`LEGACY_RECORD_VERSION`]. Version 3
/// replaced version 2's wire-plus-`fidelity`-label record with the typed
/// [`ParkWire`] variant.
pub const JOURNAL_RECORD_VERSION: u32 = 3;

/// The implicit version of a line that carries no `v` field.
const LEGACY_RECORD_VERSION: u32 = 1;

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

/// The persisted half of a complete park's wire: every feed field that is not
/// identity. Fields the feed carries as numbers ride as hex/decimal strings so
/// a line stays human-readable in the forensics surface; the conversion is
/// lossless against [`FrameWire`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFields {
    pub chain_id: u64,
    pub to: Option<String>,
    pub value: String,
    pub data: String,
    pub gas: u64,
    /// Decimal string: `serde_json` cannot deserialize a bare `u128`.
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub tx_type: u8,
    pub received_unix_ms: u64,
    pub access_list: serde_json::Value,
    /// Hex-encoded signed wire bytes (`None` preserves the pre-mevblocker-free
    /// journal lines via the default).
    #[serde(default)]
    pub raw_signed_tx: Option<String>,
}

impl WireFields {
    /// Encode the wire half of one feed event.
    #[must_use]
    pub fn encode(event: &PendingTx) -> Self {
        Self {
            chain_id: event.chain_id,
            to: event.to.map(|a| a.to_string()),
            value: format!("0x{:x}", event.value),
            data: format!("0x{}", alloy::hex::encode(&event.data)),
            gas: event.gas,
            max_fee_per_gas: event.max_fee_per_gas.to_string(),
            max_priority_fee_per_gas: event.max_priority_fee_per_gas.to_string(),
            tx_type: event.tx_type,
            received_unix_ms: event.received_unix_ms,
            access_list: event.access_list.clone(),
            raw_signed_tx: event
                .raw_signed_tx
                .as_ref()
                .map(|b| format!("0x{}", alloy::hex::encode(b))),
        }
    }

    /// Decode back to the typed wire the FSM holds.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when a hex field does not parse (a hand-edited or
    /// truncated line).
    pub fn decode(&self) -> Result<FrameWire, JournalError> {
        let to = self.to.as_deref().map(str::parse::<Address>).transpose()?;
        let value = parse_u256(&self.value)?;
        let data = Bytes::from(alloy::hex::decode(self.data.trim_start_matches("0x"))?);
        let max_fee_per_gas = self
            .max_fee_per_gas
            .parse::<u128>()
            .map_err(|e| JournalError::Field(e.to_string()))?;
        let max_priority_fee_per_gas = self
            .max_priority_fee_per_gas
            .parse::<u128>()
            .map_err(|e| JournalError::Field(e.to_string()))?;
        Ok(FrameWire {
            to,
            value,
            data,
            gas: self.gas,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            chain_id: self.chain_id,
            tx_type: self.tx_type,
            access_list: self.access_list.clone(),
            received_unix_ms: self.received_unix_ms,
            raw_signed_tx: match self.raw_signed_tx.as_deref() {
                None => None,
                Some(s) => Some(Bytes::from(
                    alloy::hex::decode(s.trim_start_matches("0x"))
                        .map_err(|e| JournalError::Field(e.to_string()))?,
                )),
            },
        })
    }
}

/// Parse a `0x`-prefixed (or bare) hex `U256`.
fn parse_u256(raw: &str) -> Result<U256, JournalError> {
    let hex = raw.trim();
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    U256::from_str_radix(hex, 16).map_err(|e| JournalError::Field(e.to_string()))
}

/// The wire a park record persists: the complete wire, or the reason it is
/// gone. A degraded record stores no wire field at all, so a zero value is
/// never read back as "the wire was lost".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "wire", rename_all = "snake_case")]
pub enum ParkWire {
    /// The whole wire survived: the record can rebuild its feed event.
    Complete { fields: Box<WireFields> },
    /// Identity only; `reason` names the missing wire.
    Degraded { reason: MissingWire },
}

/// One park: the identity, the wall clock, the wire (or the reason it is gone),
/// and the optional tentative classification (present once the nonce
/// consumption is known).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParkRecord {
    /// The frame hash the record belongs to; the fold's key.
    pub hash: B256,
    /// The frame's sender when the record carries one. `None` is absence as a
    /// fact: no fold ever borrows an address for a sender the line did not
    /// carry, and [`Self::to_parked_frame`] refuses to rebuild a sender-less
    /// frame rather than park one under a fabricated identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<Address>,
    pub claimed_nonce: u64,
    pub expected_nonce: u64,
    pub parked_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tentative: Option<TentativeRecord>,
    /// The wire, or the explicit reason it is gone.
    #[serde(flatten)]
    pub wire: ParkWire,
}

impl ParkRecord {
    /// Build a complete park record from the event that produced the gap.
    #[must_use]
    pub fn new(event: &PendingTx, expected_nonce: u64, parked_at_unix_ms: u64) -> Self {
        Self {
            hash: event.hash,
            sender: Some(event.from),
            claimed_nonce: event.nonce,
            expected_nonce,
            parked_at_unix_ms,
            tentative: None,
            wire: ParkWire::Complete {
                fields: Box::new(WireFields::encode(event)),
            },
        }
    }

    /// A record whose wire did not survive a migration: identity plus the
    /// reason, never a zero-filled wire.
    #[must_use]
    pub const fn degraded(
        hash: B256,
        sender: Option<Address>,
        claimed_nonce: u64,
        expected_nonce: u64,
        parked_at_unix_ms: u64,
        reason: MissingWire,
    ) -> Self {
        Self {
            hash,
            sender,
            claimed_nonce,
            expected_nonce,
            parked_at_unix_ms,
            tentative: None,
            wire: ParkWire::Degraded { reason },
        }
    }

    /// Attach a nonce-consumption classification (journal reload).
    #[must_use]
    pub const fn with_tentative(mut self, consumed: NonceConsumed) -> Self {
        self.tentative = Some(TentativeRecord::from_consumed(consumed));
        self
    }

    /// Why the record's wire is gone, when it is.
    #[must_use]
    pub fn degraded_reason(&self) -> Option<MissingWire> {
        match &self.wire {
            ParkWire::Complete { .. } => None,
            ParkWire::Degraded { reason } => Some(*reason),
        }
    }

    /// Rebuild the FSM frame. No clock rides the frame: liveness is finality,
    /// so the rebuild is lossless regardless of downtime. A degraded record
    /// rebuilds its identity and missing-wire reason only — there is no wire to
    /// fabricate, so the frame can never be rescued into the funnel.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when the record carries no sender (absence is never
    /// filled in with a fabricated address), or when a wire field fails to
    /// parse.
    pub fn to_parked_frame(&self) -> Result<ParkedFrame, JournalError> {
        let identity = self.identity()?;
        match &self.wire {
            ParkWire::Complete { fields } => Ok(ParkedFrame::Complete(Box::new(CompleteFrame {
                identity,
                wire: fields.decode()?,
            }))),
            ParkWire::Degraded { reason } => Ok(ParkedFrame::Degraded(DegradedFrame {
                identity,
                reason: *reason,
            })),
        }
    }

    /// The identity the record carries, when its sender survived.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when the record carries no sender: absence is a fact,
    /// and a park is never rebuilt under a borrowed address.
    fn identity(&self) -> Result<FrameIdentity, JournalError> {
        Ok(FrameIdentity {
            hash: self.hash,
            from: self.sender.ok_or_else(|| {
                JournalError::Field(String::from("park record carries no sender"))
            })?,
            claimed_nonce: self.claimed_nonce,
            expected_at_capture: self.expected_nonce,
        })
    }

    /// The original receive time, when the wire that carried it survived.
    #[must_use]
    pub fn original_received_unix_ms(&self) -> Option<u64> {
        match &self.wire {
            ParkWire::Complete { fields } => Some(fields.received_unix_ms),
            ParkWire::Degraded { .. } => None,
        }
    }
}

/// One resolution tombstone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveRecord {
    pub frame_hash: B256,
    pub resolution: Resolution,
    pub resolved_at_unix_ms: u64,
}

/// How a frame left the pending set, as the archive records it. The journal's
/// [`Resolution`] names the trigger (finalized vs not); the archive records the
/// outcome itself, because the carrying block and finality tag ride alongside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchivedResolution {
    /// The frame's own tx mined.
    Mined,
    /// A same-nonce tx consumed the slot.
    SlotTaken,
    /// The gap closed in the caller's pool view (predecessors replayable).
    RescueConsumed,
    /// A forced eviction (sender flush / operator action).
    Evicted,
}

impl ArchivedResolution {
    /// Map a journal tombstone cause to its archived outcome.
    #[must_use]
    pub const fn from_journal(resolution: Resolution) -> Self {
        match resolution {
            Resolution::MinedFinalized => Self::Mined,
            Resolution::SlotTakenFinalized => Self::SlotTaken,
            Resolution::RescueConsumed => Self::RescueConsumed,
            Resolution::Evicted => Self::Evicted,
        }
    }
}

/// One archived outcome: frame identity, how it left, any chain evidence, and
/// the wall clock.
///
/// `block`, `block_hash`, and `by` are set together from a nonce-consumption
/// classification (see [`Self::with_consumption`]), or omitted together for an
/// operational resolution. `finalized_block` is present only when finality
/// triggered the resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolutionArchiveRecord {
    #[serde(rename = "v")]
    pub version: u32,
    pub frame_hash: B256,
    pub sender: Address,
    pub claimed_nonce: u64,
    pub resolution: ArchivedResolution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_hash: Option<B256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<B256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_block: Option<u64>,
    pub resolved_unix_ms: u64,
}

impl ResolutionArchiveRecord {
    /// An outcome with no chain evidence (an operational resolution).
    #[must_use]
    pub fn new(frame: &ParkedFrame, resolution: ArchivedResolution, resolved_unix_ms: u64) -> Self {
        Self {
            version: RESOLVED_ARCHIVE_RECORD_VERSION,
            frame_hash: frame.hash(),
            sender: frame.from(),
            claimed_nonce: frame.claimed_nonce(),
            resolution,
            block: None,
            block_hash: None,
            by: None,
            finalized_block: None,
            resolved_unix_ms,
        }
    }

    /// Attach the nonce-consumption evidence that classified the frame.
    #[must_use]
    pub const fn with_consumption(mut self, consumed: NonceConsumed) -> Self {
        self.block = Some(consumed.block());
        self.block_hash = Some(consumed.block_hash());
        self.by = match consumed {
            NonceConsumed::MinedAt { .. } => None,
            NonceConsumed::SlotTakenAt { by, .. } => by,
        };
        self
    }

    /// Attach the finalized block whose finality triggered the resolution.
    #[must_use]
    pub const fn with_finalized_block(mut self, finalized_block: u64) -> Self {
        self.finalized_block = Some(finalized_block);
        self
    }
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
    /// Migrated parks whose wire did not survive ([`ParkWire::Degraded`]): the
    /// identity alone re-entered the FSM.
    pub degraded: usize,
}

/// One line's classification by the boot fold.
enum Classified {
    /// A record this binary understands; fold it semantically.
    Fold(JournalRecordPayload),
    /// A park-shaped record (any version or field state) rebuilt as a park,
    /// degraded when its wire cannot be reconstructed.
    Migrated(Box<ParkRecord>),
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
            Classified::Migrated(record) => {
                migrated += 1;
                if record.degraded_reason().is_some() {
                    degraded += 1;
                }
                pending.insert(record.hash.to_string(), *record);
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
        if let Some(record) = migrate_legacy_park(&value) {
            return Classified::Migrated(Box::new(record));
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
        JournalRecordPayload::Park(record) => {
            // The record's own wire variant is the truth: a zero-valued field
            // in a complete record is a value, never a degradation signal.
            pending.insert(record.hash.to_string(), *record);
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
    frame: Option<LegacyWireFields>,
    sender: Option<Address>,
    claimed_nonce: Option<u64>,
    expected_nonce: Option<u64>,
    parked_at_unix_ms: Option<u64>,
    hash: Option<B256>,
    nonce: Option<u64>,
    tentative: Option<serde_json::Value>,
    /// The pre-versioning wire-survival label (version 2's `fidelity`). A
    /// `degraded` label is the ONLY thing that marks a legacy wire as lost:
    /// zeroed wire fields never do, and a label this binary does not recognize
    /// fails the deserialize (corrupt policy) rather than meaning "full".
    fidelity: Option<LegacyFidelity>,
}

/// The wire-survival label older writers stamped on a park.
///
/// Only the labels this binary wrote are accepted. An unknown label is
/// malformed legacy input, not a "no degradation claim": it fails the fold's
/// deserialization, so the row follows the corruption policy (verbatim
/// sidecar, no pending record) instead of being reinterpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyFidelity {
    Full,
    Degraded,
}

/// The wire fields a legacy park might carry. All optional: whatever survives
/// is folded, and hash-plus-nonce alone still yields a degraded tracked park.
#[derive(Debug, Default, Deserialize)]
struct LegacyWireFields {
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
    raw_signed_tx: Option<String>,
}

impl LegacyWireFields {
    /// The complete wire this legacy frame carries, when every field is
    /// present. Presence is the test: a zero VALUE is a real value, so it never
    /// degrades the record.
    fn complete_fields(&self) -> Option<WireFields> {
        Some(WireFields {
            chain_id: self.chain_id?,
            to: self.to.clone(),
            value: self.value.clone()?,
            data: self.data.clone()?,
            gas: self.gas?,
            max_fee_per_gas: self.max_fee_per_gas.clone()?,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.clone()?,
            tx_type: self.tx_type?,
            received_unix_ms: self.received_unix_ms?,
            access_list: self.access_list.clone()?,
            // Raw bytes are optional evidence; their absence never degrades
            // a complete wire.
            raw_signed_tx: self.raw_signed_tx.clone(),
        })
    }
}

/// Rebuild a tracked park from a park-shaped line the current schema rejects.
///
/// The wire is [`ParkWire::Complete`] only when the line's own `fidelity` label
/// does not claim degradation AND every wire field survives. A `degraded`
/// label, a missing field, or an unparsable field migrates as identity plus
/// [`MissingWire::LegacyIncomplete`]: the label and field survival decide, never
/// a zero value. The sender survives only when the line carries one — its
/// `sender` field or a parsable `from`; a line carrying none migrates with the
/// sender absent. Returns `None` for malformed input that is never
/// reinterpreted: not even the hash and claimed nonce survive, or the wire
/// label is not one this binary knows (an unknown label is a failed fold, not a
/// full-wire claim).
fn migrate_legacy_park(value: &serde_json::Value) -> Option<ParkRecord> {
    let legacy: LegacyParkRecord = serde_json::from_value(value.clone()).ok()?;
    let frame = legacy.frame.unwrap_or_default();
    let hash = frame
        .hash
        .as_deref()
        .and_then(|raw| raw.parse::<B256>().ok())
        .or(legacy.hash)?;
    let claimed_nonce = legacy.claimed_nonce.or(frame.nonce).or(legacy.nonce)?;
    // The sender folds only from the line's own fields; a line carrying none
    // migrates with the sender absent — never a borrowed address.
    let sender = legacy.sender.or_else(|| {
        frame
            .from
            .as_deref()
            .and_then(|raw| raw.parse::<Address>().ok())
    });
    let wire = if matches!(legacy.fidelity, Some(LegacyFidelity::Degraded)) {
        ParkWire::Degraded {
            reason: MissingWire::LegacyIncomplete,
        }
    } else {
        match (frame.complete_fields(), sender) {
            (Some(fields), Some(_)) if fields.decode().is_ok() && fields.access_list.is_array() => {
                ParkWire::Complete {
                    fields: Box::new(fields),
                }
            }
            _ => ParkWire::Degraded {
                reason: MissingWire::LegacyIncomplete,
            },
        }
    };
    let tentative = legacy
        .tentative
        .and_then(|raw| serde_json::from_value::<TentativeRecord>(raw).ok());
    Some(ParkRecord {
        hash,
        sender,
        claimed_nonce,
        expected_nonce: legacy.expected_nonce.unwrap_or(claimed_nonce),
        parked_at_unix_ms: legacy.parked_at_unix_ms.unwrap_or(0),
        tentative,
        wire,
    })
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
    archive: Option<File>,
    archive_path: PathBuf,
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
        let archive_path = path.with_file_name(RESOLVED_ARCHIVE_FILE_NAME);
        // The archive shares the journal's never-abort posture: an archive that
        // cannot be opened degrades to no archiving, it never fails the journal.
        let archive = match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&archive_path)
        {
            Ok(archive) => Some(archive),
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %archive_path.display(),
                    "resolution archive unavailable - outcomes not archived"
                );
                None
            }
        };
        Ok(Self {
            file,
            path: path.to_path_buf(),
            archive,
            archive_path,
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

    /// The resolution-archive file path (sibling of the journal).
    #[must_use]
    pub fn resolution_archive_path(&self) -> &Path {
        &self.archive_path
    }

    /// Append one outcome to the resolution archive. Unlike
    /// [`Self::record_resolve`], this line is never compacted away. A journal
    /// whose archive failed to open drops the outcome; that failure was warned
    /// once at open.
    ///
    /// # Errors
    ///
    /// Serialization or I/O failure.
    pub fn record_resolution(&mut self, record: &ResolutionArchiveRecord) -> io::Result<()> {
        let Some(archive) = self.archive.as_mut() else {
            return Ok(());
        };
        let mut line = serde_json::to_vec(record)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        line.push(b'\n');
        archive.write_all(&line)
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

    fn event(hash_byte: u8, nonce: u64, received_unix_ms: u64) -> PendingTx {
        let mut raw = [0u8; 32];
        raw[0] = hash_byte;
        PendingTx {
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
            raw_signed_tx: None,
        }
    }

    fn park_line(record: &ParkRecord) -> serde_json::Value {
        serde_json::to_value(JournalRecord::new(JournalRecordPayload::Park(Box::new(
            record.clone(),
        ))))
        .expect("serialize")
    }

    #[test]
    fn complete_record_round_trips_every_wire_field() {
        let original = event(1, 12, 1_700_000_000_000);
        let record = ParkRecord::new(&original, 10, now_ms());
        assert_eq!(record.degraded_reason(), None);

        let line = park_line(&record);
        assert_eq!(
            line.get("wire").and_then(serde_json::Value::as_str),
            Some("complete"),
            "a complete row names its wire variant"
        );
        assert_eq!(
            line.get("hash").and_then(serde_json::Value::as_str),
            Some(original.hash.to_string().as_str()),
            "the identity hash is a top-level field"
        );

        let frame = record.to_parked_frame().expect("frame");
        let complete = frame.complete().expect("complete");
        assert_eq!(frame.identity().hash, original.hash);
        assert_eq!(frame.identity().from, original.from);
        assert_eq!(frame.claimed_nonce(), 12);
        assert_eq!(frame.expected_at_capture(), 10);
        assert_eq!(complete.wire.chain_id, original.chain_id);
        assert_eq!(complete.wire.gas, original.gas);
        assert_eq!(complete.wire.to, original.to);
        assert_eq!(complete.wire.received_unix_ms, original.received_unix_ms);
        assert_eq!(
            complete.to_event(),
            original,
            "the complete wire round-trips losslessly"
        );
    }

    #[test]
    fn degraded_record_round_trips_identity_and_reason() {
        let hash = B256::from([0x0b; 32]);
        let sender = Address::with_last_byte(7);
        let record = ParkRecord::degraded(
            hash,
            Some(sender),
            12,
            10,
            1_700_000_000_000,
            MissingWire::LegacyIncomplete,
        );
        assert_eq!(
            record.degraded_reason(),
            Some(MissingWire::LegacyIncomplete)
        );

        let line = park_line(&record);
        assert_eq!(
            line.get("wire").and_then(serde_json::Value::as_str),
            Some("degraded")
        );
        assert_eq!(
            line.get("reason").and_then(serde_json::Value::as_str),
            Some("legacy_incomplete")
        );
        assert!(
            line.get("fields").is_none() && line.get("frame").is_none(),
            "a degraded row carries no wire fields at all: {line}"
        );

        let frame = record.to_parked_frame().expect("frame");
        assert!(frame.is_degraded());
        assert_eq!(frame.degraded_reason(), Some(MissingWire::LegacyIncomplete));
        assert_eq!(frame.hash(), hash);
        assert_eq!(frame.from(), sender);
        assert_eq!(frame.claimed_nonce(), 12);
        assert!(
            frame.complete().is_none(),
            "no wire survives the round trip"
        );
        assert_eq!(record.original_received_unix_ms(), None);
    }

    #[test]
    fn degraded_record_survives_compaction_as_degraded() {
        let path = scratch("degraded-compact").join(JOURNAL_FILE_NAME);
        let record = ParkRecord::degraded(
            B256::from([0x0c; 32]),
            Some(Address::with_last_byte(7)),
            12,
            10,
            1,
            MissingWire::LegacyIncomplete,
        );
        compact(&path, std::slice::from_ref(&record)).expect("compact");
        let read = read_pending(&path).expect("read");
        assert_eq!(read.skipped, 0);
        assert_eq!(read.pending.len(), 1);
        assert_eq!(
            read.pending[0].degraded_reason(),
            Some(MissingWire::LegacyIncomplete)
        );
        assert_eq!(read.pending[0].claimed_nonce, 12);
        assert!(read.pending[0]
            .to_parked_frame()
            .expect("frame")
            .is_degraded());
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    /// Complete records are independent of sentinel values: a wire whose REAL
    /// fields are zero is complete, round-trips complete, and stays rescuable.
    #[test]
    fn complete_record_with_zero_wire_values_round_trips_complete() {
        let mut zeroed = event(2, 46, 0);
        zeroed.chain_id = 0;
        zeroed.gas = 0;
        zeroed.value = U256::ZERO;
        zeroed.max_fee_per_gas = 0;
        zeroed.max_priority_fee_per_gas = 0;
        zeroed.tx_type = 0;
        zeroed.data = Bytes::new();
        zeroed.to = None;
        zeroed.received_unix_ms = 0;
        let record = ParkRecord::new(&zeroed, 45, 0);
        assert_eq!(
            record.degraded_reason(),
            None,
            "zero values never degrade a record"
        );
        assert_eq!(
            park_line(&record)
                .get("wire")
                .and_then(serde_json::Value::as_str),
            Some("complete")
        );
        let frame = record.to_parked_frame().expect("frame");
        let complete = frame.complete().expect("complete");
        assert_eq!(complete.wire.gas, 0);
        assert_eq!(complete.wire.chain_id, 0);
        assert_eq!(complete.to_event(), zeroed);
    }

    /// A malformed legacy identity is never reinterpreted: the corruption
    /// policy preserves the line verbatim and keeps it out of the pending set.
    #[test]
    fn malformed_legacy_identity_lands_in_the_corrupt_sidecar() {
        let path = scratch("malformed-identity").join(JOURNAL_FILE_NAME);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let line = "{\"kind\":\"park\",\"frame\":{\"hash\":\"not-a-hash\",\"nonce\":12},\"claimed_nonce\":12}";
        std::fs::write(&path, format!("{line}\n")).expect("write");
        let read = read_pending(&path).expect("read");
        assert_eq!(read.skipped, 1, "a park with no identity is corrupt");
        assert!(read.pending.is_empty());
        assert_eq!(read.migrated, 0);
        let sidecar = std::fs::read_to_string(corrupt_sidecar_path(&path)).expect("sidecar");
        assert!(
            sidecar.contains(line),
            "the rejected line is preserved verbatim"
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    /// An unknown wire-survival label is malformed legacy input, never a "no
    /// degradation claim": the row follows the corruption policy (verbatim
    /// sidecar, no pending record) instead of migrating as a complete park.
    #[test]
    fn unknown_legacy_fidelity_label_is_corrupt_never_complete() {
        let path = scratch("unknown-fidelity").join(JOURNAL_FILE_NAME);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let hash = B256::from([0x0f; 32]);
        let sender = Address::with_last_byte(7);
        let line = format!(
            "{{\"v\":2,\"kind\":\"park\",\"frame\":{{\"hash\":\"{hash}\",\"chain_id\":1,\"from\":\"{sender}\",\"to\":null,\"value\":\"0x7b\",\"data\":\"0xabcd\",\"gas\":300000,\"max_fee_per_gas\":\"514684409\",\"max_priority_fee_per_gas\":\"1000000000\",\"nonce\":48,\"tx_type\":2,\"received_unix_ms\":1700000000000,\"access_list\":[]}},\"sender\":\"{sender}\",\"claimed_nonce\":48,\"expected_nonce\":46,\"parked_at_unix_ms\":0,\"fidelity\":\"future_or_broken\"}}"
        );
        std::fs::write(&path, format!("{line}\n")).expect("write");

        let read = read_pending(&path).expect("read");
        assert_eq!(read.skipped, 1, "an unknown label is corrupt input");
        assert_eq!(read.migrated, 0);
        assert_eq!(read.degraded, 0);
        assert!(read.pending.is_empty(), "no pending record is invented");
        let sidecar = std::fs::read_to_string(corrupt_sidecar_path(&path)).expect("sidecar");
        assert!(
            sidecar.contains(&line),
            "the rejected line is preserved verbatim"
        );

        // Negative probe: the label is the rejection, not the field content --
        // the SAME wire without the label migrates as a complete park.
        let value: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(
            migrate_legacy_park(&value).is_none(),
            "an unknown label must fail the legacy fold"
        );
        let mut unlabelled = value.as_object().expect("object").clone();
        unlabelled.remove("fidelity");
        let record = migrate_legacy_park(&serde_json::Value::Object(unlabelled))
            .expect("the unlabelled complete wire migrates");
        assert_eq!(record.degraded_reason(), None);
        assert_eq!(record.hash, hash);

        let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
    }

    /// The version-2 `degraded` label maps to the named reason. The label
    /// decides — the zeroed wire such a row wrote is never read back as real.
    #[test]
    fn legacy_degraded_label_maps_to_the_named_reason() {
        let hash = B256::from([0x0d; 32]);
        let sender = Address::with_last_byte(7);
        let value = serde_json::json!({
            "kind": "park",
            "v": 2,
            "frame": {
                "hash": hash.to_string(),
                "from": sender.to_string(),
                "to": null,
                "nonce": 46,
            },
            "sender": sender.to_string(),
            "claimed_nonce": 46,
            "expected_nonce": 45,
            "fidelity": "degraded",
        });
        let record = migrate_legacy_park(&value).expect("migrates");
        assert_eq!(
            record.degraded_reason(),
            Some(MissingWire::LegacyIncomplete)
        );
        let frame = record.to_parked_frame().expect("frame");
        assert!(frame.is_degraded());
        assert_eq!(frame.claimed_nonce(), 46);
        assert_eq!(frame.from(), sender);
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
            .record_resolve(second.hash, Resolution::MinedFinalized, now_ms())
            .expect("resolve");

        let read = read_pending(&path).expect("read");
        assert_eq!(read.skipped, 0);
        assert_eq!(read.pending.len(), 1);
        assert_eq!(read.pending[0].claimed_nonce, 12);
        assert_eq!(read.pending[0].original_received_unix_ms(), Some(1_000));

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
        assert_eq!(frame.expected_at_capture(), 48);
        assert_eq!(frame.claimed_nonce(), 50);
        assert!(frame.complete().is_some(), "the wire survived the rebuild");
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
            .record_tentative(record.hash, consumed, now_ms())
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

    /// A degraded park keeps only its identity, so the rebuild requires the
    /// sender the record carries: absence refuses — never a fabricated zero
    /// address — while a sender the record genuinely carries, including a zero
    /// address, rebuilds exactly as recorded.
    #[test]
    fn degraded_frame_refuses_absent_sender_and_preserves_a_recorded_zero() {
        let hash = B256::from([0x07; 32]);
        let record =
            ParkRecord::degraded(hash, None, 70, 68, now_ms(), MissingWire::LegacyIncomplete);

        let err = record
            .to_parked_frame()
            .expect_err("an absent sender must refuse the rebuild");
        assert!(
            matches!(err, JournalError::Field(_)),
            "the refusal rides the journal's field-error channel: {err}"
        );

        // A zero address the record genuinely carries is preserved as
        // recorded: absence and zero are different facts.
        let record = ParkRecord::degraded(
            hash,
            Some(Address::ZERO),
            70,
            68,
            now_ms(),
            MissingWire::LegacyIncomplete,
        );
        let frame = record
            .to_parked_frame()
            .expect("a recorded zero sender rebuilds");
        assert_eq!(frame.from(), Address::ZERO);
        assert!(frame.is_degraded());

        // The readable-sender case rebuilds exactly as recorded: the refusal
        // is the absence, not the degraded wire.
        let record = ParkRecord::degraded(
            hash,
            Some(Address::with_last_byte(7)),
            70,
            68,
            now_ms(),
            MissingWire::LegacyIncomplete,
        );
        let frame = record.to_parked_frame().expect("readable sender rebuilds");
        assert_eq!(frame.from(), Address::with_last_byte(7));
    }

    /// The legacy fold preserves a missing sender as absence — never the zero
    /// address — and keeps a genuinely recorded zero address as recorded.
    #[test]
    fn legacy_migration_preserves_absent_sender_and_a_recorded_zero() {
        let hash = B256::from([0x0a; 32]);
        let absent = serde_json::json!({
            "kind": "park",
            "frame": {"hash": hash.to_string(), "from": "not-an-address", "nonce": 12},
            "claimed_nonce": 12,
        });
        let record = migrate_legacy_park(&absent).expect("migrates");
        assert_eq!(
            record.degraded_reason(),
            Some(MissingWire::LegacyIncomplete)
        );
        assert_eq!(
            record.sender, None,
            "absence is preserved, never Address::ZERO"
        );
        record
            .to_parked_frame()
            .expect_err("the rebuild refuses without a sender");

        let zero = serde_json::json!({
            "kind": "park",
            "frame": {"hash": hash.to_string(), "nonce": 12},
            "sender": Address::ZERO.to_string(),
            "claimed_nonce": 12,
        });
        let record = migrate_legacy_park(&zero).expect("migrates");
        assert_eq!(
            record.sender,
            Some(Address::ZERO),
            "a recorded zero address is preserved as recorded"
        );
        let frame = record
            .to_parked_frame()
            .expect("a recorded zero sender rebuilds");
        assert_eq!(frame.from(), Address::ZERO);
    }
}
