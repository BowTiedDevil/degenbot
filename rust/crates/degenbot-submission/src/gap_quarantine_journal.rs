//! Durable parking journal for the gap quarantine.
//!
//! The quarantine FSM is in-memory, so a process restart loses every parked
//! frame. This module persists parks and their resolutions as JSON Lines under
//! the typed `persistence.state_dir` root (default `~/.config/degenbot/state`)
//! — OUTSIDE the per-session run directories, because the journal must outlive
//! the session that wrote it. On the next boot the still-pending parks re-enter
//! the live FSM exactly as they left it (tracked or tentative); nothing is
//! dropped for age.
//!
//! # Persistence model
//!
//! Append-on-change with resolution tombstones, compacted once at boot:
//!
//! - A park appends `{"kind":"park", ...}` carrying the full feed event plus
//!   the claimed/expected nonce and the wall clock.
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
//!   still-pending parks — this is the compaction pass, and it also discards a
//!   corrupt tail.
//!
//! A corrupt line is skipped with a warning and counted; the fold continues
//! with the rest, and the boot compaction heals the file. Losing a park line
//! this way is safe: the frame was never admitted and the chain probe on the
//! next park re-derives state.

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
    /// so the rebuild is lossless regardless of downtime.
    ///
    /// # Errors
    ///
    /// [`JournalError`] when a hex field fails to parse.
    pub fn to_parked_frame(&self) -> Result<ParkedFrame, JournalError> {
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

/// One journal line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalRecord {
    Park(Box<ParkRecord>),
    Tentative(Box<TentativeEntryRecord>),
    Resolve(ResolveRecord),
}

/// What a load found.
#[derive(Debug, Default)]
pub struct PendingRead {
    /// Still-pending parks, ordered by frame hash.
    pub pending: Vec<ParkRecord>,
    /// Unparseable lines skipped (each produced a warning at the call site).
    pub skipped: usize,
}

/// The still-pending parks after folding the journal at `path`. A missing file
/// is an empty journal; a corrupt line is counted in `skipped`, never fatal.
///
/// # Errors
///
/// I/O errors other than `NotFound`.
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
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalRecord>(line) {
            Ok(JournalRecord::Park(record)) => {
                pending.insert(record.frame.hash.clone(), *record);
            }
            Ok(JournalRecord::Tentative(record)) => {
                if let Some(park) = pending.get_mut(&record.frame_hash.to_string()) {
                    park.tentative = Some(record.consumed);
                }
            }
            Ok(JournalRecord::Resolve(record)) => {
                pending.remove(&record.frame_hash.to_string());
            }
            Err(_) => skipped += 1,
        }
    }
    Ok(PendingRead {
        pending: pending.into_values().collect(),
        skipped,
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
        let line = JournalRecord::Park(Box::new(record.clone()));
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
        self.append(&JournalRecord::Park(Box::new(record.clone())))
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
        self.append(&JournalRecord::Resolve(ResolveRecord {
            frame_hash,
            resolution,
            resolved_at_unix_ms,
        }))
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
        self.append(&JournalRecord::Tentative(Box::new(TentativeEntryRecord {
            frame_hash,
            consumed: TentativeRecord::from_consumed(consumed),
            entered_at_unix_ms,
        })))
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
