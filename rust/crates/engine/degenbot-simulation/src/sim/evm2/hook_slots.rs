//! The durable per-hook touched-slot SET store (ADR-090 H2).
//!
//! A black-box V4 leg quote ([`super::v4_black_box`]) executes arbitrary hook
//! bytecode, so the storage a hook reads is not derivable from the pool key:
//! the only computable V4 words are the pool's own state. What IS observable is
//! the set of slots one quote touched — the frame's settled touched-set carries
//! every `(address, slot)` the run read or wrote, and the pool key names the
//! address the hook's own storage lives at.
//!
//! This store is that observation, kept once per hook: `hook -> {slot -> last
//! observed}`. The prewarm ([`crate::sim::prefetch`]) joins the learned slots
//! into the anchor's ONE batch, so a steady-state re-quote reads hook storage
//! warm instead of paying the serial RPC fallback one slot at a time.
//!
//! # Values never come from here
//!
//! The store holds slot IDENTITIES, never values. Every prefetch member is read
//! from the node at the fan-out's own anchor, so a hook whose storage moved
//! costs one wasted batch member, never a wrong read: hook state itself can
//! never be event-maintained (a hook writes its storage invisibly to the event
//! stream), which is exactly why the set is cached and the value is not.
//!
//! # Merge, and the cap
//!
//! Sets MERGE (union) over observations: a hook's code path can be amount- or
//! direction-dependent, so a later quote may touch slots an earlier one did
//! not. [`MAX_HOOK_SLOTS`] bounds one hook's set — a request that would exceed
//! it evicts the least recently observed slot (the merge compares the
//! persisted recency order against this run's, so a restart does not scramble
//! it), ties broken on the slot value so eviction is deterministic. A pathological hook therefore keeps the slots it
//! actually keeps touching, and one batch can never be flooded by one hook
//! ([`crate::sim::prefetch::MAX_PREFETCH_SLOTS`] bounds the batch).
//!
//! # Persistence
//!
//! The [`DurableArtifactStore`](super::aot_store) pattern: a stable directory
//! ([`default_hook_slot_dir`]), one file per `(hook, spec)` key written through
//! a temporary sibling and a rename so a reader never sees a partial record,
//! and content-verified loads. A mismatched content hash, an unreadable file,
//! an unparseable record, a foreign hook/spec, or an impossible set size is a
//! MISS — an empty set, never an error and never a panic. A damaged store
//! therefore degrades to re-learning: the next quote's capture writes the set
//! again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ::evm2::SpecId;
use alloy::primitives::{keccak256, Address, B256, U256};
use parking_lot::Mutex;

/// The subdirectory of `persistence.state_dir` holding the learned sets.
pub const HOOK_SLOT_DIR_NAME: &str = "hook-slot-sets";

/// The most slots one hook's set may hold. A stateful hook's read set is a
/// handful of words (its own config, a counter, a per-direction accumulator);
/// 128 is far past every observed hook and still under a quarter of one prefetch
/// batch, so a pathological hook cannot crowd out the path derivation.
pub const MAX_HOOK_SLOTS: usize = 128;

/// The record schema version. A record with any other version is a miss, so a
/// future on-disk change degrades to re-learning instead of a misread.
const FORMAT_VERSION: u32 = 1;

/// The per-hook record suffix.
const RECORD_SUFFIX: &str = ".json";

/// A hook's slot-set key: the hook contract a pool key names, under the engine
/// spec its quote frame executes at (a spec upgrade can change which storage a
/// hook's code paths touch — e.g. one gated behind a fork).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HookSlotKey {
    /// The hook contract — the pool key's `hooks` field.
    pub hook: Address,
    /// The engine spec the quote frame runs under.
    pub spec: SpecId,
}

impl HookSlotKey {
    /// The key for `hook` under `spec`.
    #[must_use]
    pub fn new(hook: Address, spec: SpecId) -> Self {
        Self { hook, spec }
    }

    /// The stable file stem for the key (the loader never parses it back; the
    /// record's own fields are authoritative).
    fn file_stem(&self) -> String {
        format!(
            "{}-s{}",
            alloy::hex::encode(self.hook),
            u32::from(self.spec)
        )
    }
}

/// What one merge did — the learning progress, surfaced rather than assumed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookSlotMerge {
    /// Slots the union added that the set did not already hold (`0` = a
    /// re-quote of an already-learned hook).
    pub added: usize,
    /// The set's size after the merge.
    pub total: usize,
    /// Slots the cap refused, evicting the least recently observed member.
    pub evicted: usize,
    /// Whether the merged set reached the disk (false for a merge that added
    /// nothing, and for an in-memory store).
    pub persisted: bool,
}

/// The per-hook rows plus the lazily-loaded bookkeeping.
#[derive(Default)]
struct Inner {
    /// `hook -> {slot -> the observation ordinal that last touched it}`.
    rows: HashMap<Address, BTreeMap<U256, u64>>,
    /// Hooks whose record the disk has already answered for (a miss is an
    /// answer too, so a hook with no record is not re-stat'ed every query).
    loaded: HashSet<Address>,
    /// The monotone observation ordinal (recency for the cap's eviction; not
    /// an anchor — a set outlives the block that taught it).
    clock: u64,
}

/// The durable per-hook slot-set store (see the module docs).
pub struct HookSlotStore {
    /// The record directory; `None` for an in-memory store.
    dir: Option<PathBuf>,
    spec: SpecId,
    inner: Mutex<Inner>,
}

impl HookSlotStore {
    /// A store that learns for the process's lifetime and persists nothing —
    /// the lab/test stance.
    #[must_use]
    pub fn in_memory(spec: SpecId) -> Self {
        Self {
            dir: None,
            spec,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// A store rooted at `dir` (created if absent). Records load lazily, one
    /// per hook on its first query, so a large store costs nothing until a
    /// hook is actually asked about.
    ///
    /// # Errors
    ///
    /// Fails only when the directory cannot be created — every later read or
    /// write degrades to the in-memory row ([`Self::open`] is the boot-safe
    /// form).
    pub fn new(dir: impl Into<PathBuf>, spec: SpecId) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir: Some(dir),
            spec,
            inner: Mutex::new(Inner::default()),
        })
    }

    /// A store rooted at `dir`, degrading to an in-memory store when the
    /// directory cannot be created: a bot never fails to boot over a slot
    /// cache.
    #[must_use]
    pub fn open(dir: impl Into<PathBuf>, spec: SpecId) -> Self {
        Self::new(dir, spec).unwrap_or_else(|_| Self::in_memory(spec))
    }

    /// The record directory (`None` for an in-memory store).
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The engine spec this store's sets are keyed under.
    #[must_use]
    pub fn spec(&self) -> SpecId {
        self.spec
    }

    /// The learned set for `hook`, ascending. An absent, damaged, or
    /// foreign-keyed record is an empty set (and is re-learned by the next
    /// capture).
    #[must_use]
    pub fn slots_for(&self, hook: Address) -> Vec<U256> {
        let mut inner = self.inner.lock();
        self.ensure_loaded(&mut inner, hook);
        inner
            .rows
            .get(&hook)
            .map_or_else(Vec::new, |row| row.keys().copied().collect())
    }

    /// The number of hooks this store holds a non-empty set for (the
    /// diagnostic seam, not a per-fan-out read).
    #[must_use]
    pub fn hook_count(&self) -> usize {
        self.inner
            .lock()
            .rows
            .values()
            .filter(|row| !row.is_empty())
            .count()
    }

    /// The number of slots this store holds across every hook.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.inner.lock().rows.values().map(BTreeMap::len).sum()
    }

    /// Union `slots` (the ones a quote of `hook` touched) into `hook`'s set,
    /// persisting the record when the set grew.
    ///
    /// The merge runs against the on-disk set when this process has not read it
    /// yet, so a restart cannot truncate a learned set to the slots one quote
    /// happened to touch. A recency-only refresh is NOT written: the persisted
    /// order is the order as of the last growth, which is what the cap's
    /// eviction compares against.
    #[must_use]
    pub fn record(&self, hook: Address, slots: impl IntoIterator<Item = U256>) -> HookSlotMerge {
        if hook.is_zero() {
            // A pool key with no hook names no storage set.
            return HookSlotMerge::default();
        }
        let (added, evicted, total) = {
            let mut inner = self.inner.lock();
            self.ensure_loaded(&mut inner, hook);
            inner.clock = inner.clock.wrapping_add(1);
            let observed = inner.clock;
            let row = inner.rows.entry(hook).or_default();
            let mut added = 0usize;
            let mut evicted = 0usize;
            for slot in slots {
                if let std::collections::btree_map::Entry::Occupied(mut known) = row.entry(slot) {
                    // A known slot only refreshes its recency.
                    known.insert(observed);
                    continue;
                }
                if row.len() >= MAX_HOOK_SLOTS {
                    let coldest = row
                        .iter()
                        .min_by_key(|(slot, last)| (**last, **slot))
                        .map(|(slot, _)| *slot);
                    if let Some(coldest) = coldest {
                        row.remove(&coldest);
                        evicted += 1;
                    }
                }
                row.insert(slot, observed);
                added += 1;
            }
            (added, evicted, row.len())
        };
        let persisted = added > 0 && self.persist(hook);
        HookSlotMerge {
            added,
            total,
            evicted,
            persisted,
        }
    }

    /// Read `hook`'s record once (a miss is remembered, so a hook with no
    /// record costs one `open` per process, not one per query).
    fn ensure_loaded(&self, inner: &mut Inner, hook: Address) {
        if hook.is_zero() || !inner.loaded.insert(hook) {
            return;
        }
        let Some(dir) = self.dir.as_ref() else {
            return;
        };
        if let Some(row) = read_row(dir, &HookSlotKey::new(hook, self.spec)) {
            // Recency ordinals are process-local, so a loaded row's newest
            // observation must still rank BELOW anything this process observes
            // — otherwise the first eviction after a restart could drop the
            // slot that was just touched instead of the one that has been cold
            // since the previous run.
            inner.clock = inner.clock.max(row.values().copied().max().unwrap_or(0));
            inner.rows.insert(hook, row);
        }
    }

    /// Write `hook`'s set as one record. Never fails the caller: an unwritable
    /// store leaves the in-memory set intact and re-learns next run.
    fn persist(&self, hook: Address) -> bool {
        let Some(dir) = self.dir.as_ref() else {
            return false;
        };
        let row = {
            let inner = self.inner.lock();
            inner.rows.get(&hook).cloned().unwrap_or_default()
        };
        if row.is_empty() {
            return false;
        }
        let key = HookSlotKey::new(hook, self.spec);
        let bytes = encode_record(&key, &row);
        write_atomic(
            &dir.join(format!("{}{RECORD_SUFFIX}", key.file_stem())),
            &bytes,
        )
        .is_ok()
    }
}

/// The production store: [`default_hook_slot_dir`], keyed under the production
/// frame spec, degrading to an in-memory store when the directory cannot be
/// created.
#[must_use]
pub fn open_production_store() -> Arc<HookSlotStore> {
    Arc::new(HookSlotStore::open(
        default_hook_slot_dir(),
        super::handle::EVM2_PRODUCTION_SPEC,
    ))
}

/// The durable record directory: `<persistence.state_dir>/hook-slot-sets`.
///
/// The configured root expands exactly as every other state-rooted path does:
/// the built-in default rebases onto an absolute `$XDG_STATE_HOME`, an
/// operator-supplied value expands its leading `~` against `HOME`.
#[must_use]
pub fn default_hook_slot_dir() -> PathBuf {
    let configured = &degenbot_config::holder::config().persistence.state_dir;
    let raw = configured.to_string_lossy();
    let expanded = if *configured == degenbot_config::BotConfig::default().persistence.state_dir {
        degenbot_config::expand_state_path(&raw)
    } else {
        degenbot_config::expand_tilde_path(&raw)
    };
    expanded.join(HOOK_SLOT_DIR_NAME)
}

/// The on-disk record: the key, the set (ascending), and the hash that binds
/// them.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SlotRecord {
    slot: U256,
    last_seen: u64,
}

/// See [`SlotRecord`]; `spec` is the frame spec, `content_hash` the verifier.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SlotFile {
    format_version: u32,
    hook: Address,
    spec: u32,
    content_hash: B256,
    slots: Vec<SlotRecord>,
}

/// The canonical bytes a record's `content_hash` covers — every field that
/// shapes the set, in a fixed order, independent of the JSON encoding.
fn content_hash(hook: Address, spec: u32, row: &BTreeMap<U256, u64>) -> B256 {
    let mut bytes = Vec::with_capacity(4 + 20 + 4 + row.len() * 40);
    bytes.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    bytes.extend_from_slice(hook.as_slice());
    bytes.extend_from_slice(&spec.to_be_bytes());
    for (slot, last_seen) in row {
        let word: [u8; 32] = slot.to_be_bytes();
        bytes.extend_from_slice(&word);
        bytes.extend_from_slice(&last_seen.to_be_bytes());
    }
    keccak256(bytes)
}

/// The encoded record for `key`'s set.
fn encode_record(key: &HookSlotKey, row: &BTreeMap<U256, u64>) -> Vec<u8> {
    let spec = u32::from(key.spec);
    let file = SlotFile {
        format_version: FORMAT_VERSION,
        hook: key.hook,
        spec,
        content_hash: content_hash(key.hook, spec, row),
        slots: row
            .iter()
            .map(|(slot, last_seen)| SlotRecord {
                slot: *slot,
                last_seen: *last_seen,
            })
            .collect(),
    };
    // An encoding failure is impossible for this shape (no maps, no non-finite
    // numbers); an empty record is the safe answer if it ever happened.
    serde_json::to_vec(&file).unwrap_or_default()
}

/// Reads and verifies one record. Every failure class is `None` — a miss.
fn read_row(dir: &Path, key: &HookSlotKey) -> Option<BTreeMap<U256, u64>> {
    let bytes = fs::read(dir.join(format!("{}{RECORD_SUFFIX}", key.file_stem()))).ok()?;
    let file: SlotFile = serde_json::from_slice(&bytes).ok()?;
    let spec = u32::from(key.spec);
    if file.format_version != FORMAT_VERSION || file.hook != key.hook || file.spec != spec {
        return None;
    }
    let mut row = BTreeMap::new();
    for record in file.slots {
        row.insert(record.slot, record.last_seen);
    }
    // A set larger than the cap cannot be one this store wrote; a duplicate
    // slot collapses into the map and breaks the hash below.
    if row.len() > MAX_HOOK_SLOTS {
        return None;
    }
    if content_hash(file.hook, file.spec, &row) != file.content_hash {
        return None;
    }
    Some(row)
}

/// Writes `bytes` to `path` through a temporary sibling and a rename, so a
/// concurrent reader observes the old record or the new one, never a partial
/// one. (The [`aot_store`](super::aot_store) idiom; that module is
/// `evm2-jit`-gated, so the two writers cannot share one helper here.)
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temporary_sibling(path);
    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    if let Err(error) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// A private, unique sibling path for the atomic write.
fn temporary_sibling(path: &Path) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.tmp-{}-{sequence}", std::process::id()))
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use ::evm2::SpecId;
    use alloy::primitives::{address, Address, U256};

    use super::{HookSlotStore, MAX_HOOK_SLOTS};

    const HOOK: Address = address!("322dcEc4958C14e021A9F1cD49DF11b9457968cC");
    const OTHER_HOOK: Address = address!("00000000000000000000000000000000000000AA");
    const SPEC: SpecId = SpecId::OSAKA;

    /// A unique scratch directory removed on drop.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "degenbot-hook-slots-{}-{sequence}-{label}",
                std::process::id()
            )))
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record_path(dir: &ScratchDir, hook: Address, spec: SpecId) -> PathBuf {
        dir.path().join(format!(
            "{}-s{}.json",
            alloy::hex::encode(hook),
            u32::from(spec)
        ))
    }

    fn slots(values: impl IntoIterator<Item = u64>) -> Vec<U256> {
        values.into_iter().map(U256::from).collect()
    }

    fn store(dir: &ScratchDir) -> HookSlotStore {
        HookSlotStore::new(dir.path(), SPEC).expect("the scratch store opens")
    }

    /// A learned set survives a store instance: the record's whole reason.
    #[test]
    fn a_learned_set_reloads_from_the_disk() {
        let dir = ScratchDir::new("reload");
        let first = store(&dir);
        let merge = first.record(HOOK, slots([3, 7]));
        assert_eq!(merge.added, 2);
        assert_eq!(merge.total, 2);
        assert!(merge.persisted, "a grown set reaches the disk");

        let reopened = store(&dir);
        assert_eq!(reopened.slots_for(HOOK), slots([3, 7]));
        assert_eq!(reopened.slot_count(), 2);
    }

    /// Sets MERGE: a hook's code path can be amount/direction dependent, so a
    /// later observation adds to the set instead of replacing it — including
    /// across process restarts, where the merge must run against the DISK set.
    #[test]
    fn observations_union_including_across_a_restart() {
        let dir = ScratchDir::new("union");
        let first = store(&dir);
        let _ = first.record(HOOK, slots([3]));
        let _ = first.record(HOOK, slots([7, 11]));
        assert_eq!(first.slots_for(HOOK), slots([3, 7, 11]));

        // A fresh process merges INTO the persisted set rather than replacing it.
        let restarted = store(&dir);
        let merge = restarted.record(HOOK, slots([13]));
        assert_eq!(merge.added, 1);
        assert_eq!(merge.total, 4);
        assert_eq!(store(&dir).slots_for(HOOK), slots([3, 7, 11, 13]));
    }

    /// A slot the set already holds is not new material: no growth, no write.
    #[test]
    fn a_repeat_of_a_known_slot_is_not_a_change() {
        let dir = ScratchDir::new("repeat");
        let store = store(&dir);
        assert_eq!(store.record(HOOK, slots([5, 9])).added, 2);
        let repeat = store.record(HOOK, slots([5, 9]));
        assert_eq!(repeat.added, 0);
        assert_eq!(repeat.total, 2);
        assert!(!repeat.persisted, "an unchanged set is not rewritten");
    }

    /// The cap holds, and it holds the RECENT set: the least recently observed
    /// slot is the one that goes, so a pathological hook keeps what it keeps
    /// touching.
    #[test]
    fn the_cap_evicts_the_least_recently_observed_slot() {
        let dir = ScratchDir::new("cap");
        let store = store(&dir);
        let full = store.record(HOOK, (0..MAX_HOOK_SLOTS as u64).map(U256::from));
        assert_eq!(full.total, MAX_HOOK_SLOTS);
        assert_eq!(full.evicted, 0);

        // Re-observe the first slot, then add one more: the cap evicts slot 1
        // (the oldest observation), never slot 0.
        let _ = store.record(HOOK, slots([0]));
        let overflow = store.record(HOOK, slots([9_999]));
        assert_eq!(overflow.total, MAX_HOOK_SLOTS);
        assert_eq!(overflow.evicted, 1);
        assert_eq!(overflow.added, 1);

        let set = store.slots_for(HOOK);
        assert!(
            set.contains(&U256::from(0u64)),
            "the re-observed slot stays"
        );
        assert!(!set.contains(&U256::from(1u64)), "the coldest slot went");
        assert!(set.contains(&U256::from(9_999u64)), "the new slot landed");
        assert_eq!(store.slots_for(HOOK).len(), MAX_HOOK_SLOTS);
    }

    /// Recency ordinals survive a restart in ORDER: a reopened store's fresh
    /// observation ranks above everything the persisted set already knew, so the
    /// cap keeps evicting the genuinely coldest slot instead of the one just
    /// learned.
    #[test]
    fn the_caps_recency_order_survives_a_restart() {
        let dir = ScratchDir::new("restart-cap");
        let first = store(&dir);
        let _ = first.record(HOOK, (0..MAX_HOOK_SLOTS as u64).map(U256::from));
        drop(first);

        let restarted = store(&dir);
        // The first post-restart addition evicts the oldest persisted slot (0).
        let first_add = restarted.record(HOOK, slots([9_999]));
        assert_eq!(first_add.evicted, 1);
        assert!(!restarted.slots_for(HOOK).contains(&U256::ZERO));

        // The second must evict the NEXT oldest (1), not the slot just learned.
        let second_add = restarted.record(HOOK, slots([10_000]));
        assert_eq!(second_add.evicted, 1);
        let set = restarted.slots_for(HOOK);
        assert!(set.contains(&U256::from(9_999u64)), "the fresh slot stays");
        assert!(set.contains(&U256::from(10_000u64)));
        assert!(
            !set.contains(&U256::from(1u64)),
            "the coldest persisted slot went"
        );
        assert_eq!(set.len(), MAX_HOOK_SLOTS);
    }

    /// A damaged record degrades to re-learning: the load is an empty set (no
    /// error, no panic) and the next capture writes a fresh record.
    #[test]
    fn a_corrupt_record_degrades_to_relearning() {
        let dir = ScratchDir::new("corrupt");
        let _ = store(&dir).record(HOOK, slots([4, 8]));
        fs::write(record_path(&dir, HOOK, SPEC), b"{ not a record")
            .expect("the record is corrupted");

        let damaged = store(&dir);
        assert!(
            damaged.slots_for(HOOK).is_empty(),
            "a corrupt record is a miss"
        );

        let relearned = damaged.record(HOOK, slots([4, 8, 12]));
        assert_eq!(relearned.added, 3);
        assert!(relearned.persisted);
        assert_eq!(store(&dir).slots_for(HOOK), slots([4, 8, 12]));
    }

    /// A record whose bytes do not hash to its `content_hash` is a miss — the
    /// tamper it catches is a truncated or spliced write, not a malicious one.
    #[test]
    fn a_content_hash_mismatch_is_a_miss() {
        let dir = ScratchDir::new("tamper");
        let _ = store(&dir).record(HOOK, slots([2, 6]));
        let path = record_path(&dir, HOOK, SPEC);

        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("the record reads"))
                .expect("the record parses");
        value["slots"][0]["slot"] =
            serde_json::json!("0x00000000000000000000000000000000000000000000000000000000000000ff");
        fs::write(&path, serde_json::to_vec(&value).expect("re-encode"))
            .expect("the record is overwritten");

        assert!(store(&dir).slots_for(HOOK).is_empty());
    }

    /// A truncated (empty) record is a miss, never a panic.
    #[test]
    fn a_truncated_record_is_a_miss() {
        let dir = ScratchDir::new("truncated");
        let _ = store(&dir).record(HOOK, slots([1]));
        fs::write(record_path(&dir, HOOK, SPEC), b"").expect("the record is truncated");
        assert!(store(&dir).slots_for(HOOK).is_empty());
    }

    /// The key is `(hook, spec)`: another spec's record is not this spec's set.
    #[test]
    fn a_foreign_spec_is_not_this_set() {
        let dir = ScratchDir::new("spec");
        let _ = store(&dir).record(HOOK, slots([1, 2]));

        let other_spec = HookSlotStore::new(dir.path(), SpecId::CANCUN).expect("opens");
        assert!(
            other_spec.slots_for(HOOK).is_empty(),
            "a set learned under another spec is a different key"
        );

        // The original spec still sees its own set.
        assert_eq!(store(&dir).slots_for(HOOK), slots([1, 2]));
    }

    /// Hooks are independent rows, and a pool key with no hook names no set.
    #[test]
    fn hooks_are_independent_and_the_zero_address_learns_nothing() {
        let dir = ScratchDir::new("independent");
        let store = store(&dir);
        let _ = store.record(HOOK, slots([1, 2]));
        let _ = store.record(OTHER_HOOK, slots([9]));
        assert_eq!(store.slots_for(HOOK), slots([1, 2]));
        assert_eq!(store.slots_for(OTHER_HOOK), slots([9]));

        let nothing = store.record(Address::ZERO, slots([1, 2]));
        assert_eq!(nothing, super::HookSlotMerge::default());
        assert_eq!(store.hook_count(), 2);
    }

    /// The in-memory stance learns without a directory and persists nothing.
    #[test]
    fn an_in_memory_store_keeps_no_record() {
        let store = HookSlotStore::in_memory(SPEC);
        let merge = store.record(HOOK, slots([5]));
        assert_eq!(merge.added, 1);
        assert!(!merge.persisted);
        assert!(store.dir().is_none());
        assert_eq!(store.slots_for(HOOK), slots([5]));
    }

    /// An unwritable directory degrades to an in-memory store instead of
    /// failing the open — the boot-safe stance.
    #[test]
    fn an_unwritable_directory_degrades_to_memory() {
        let doomed = ScratchDir::new("unwritable");
        // A FILE where the store wants a directory: create_dir_all fails.
        fs::write(doomed.path(), b"not a directory").expect("the blocker is written");
        let store = HookSlotStore::open(doomed.path(), SPEC);
        assert!(store.dir().is_none(), "the open degraded");
        assert_eq!(store.record(HOOK, slots([6])).added, 1);
        assert_eq!(store.slots_for(HOOK), slots([6]));
    }
}
