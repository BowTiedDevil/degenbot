//! The real write-coverage fixture behind the offline real-coverage replay:
//! one captured stretch of canonical blocks — each block's exact-hash identity,
//! its REAL parent hash, and the exact write identities (the final-transition
//! slots) its block access list carries over the corpus's addresses. Capture
//! and replay are separate phases: capture dials the live node once, and the
//! measured replay runs offline against the captured file.
//!
//! # Why a fixture
//!
//! The recorded corpus carries reads only, so write coverage is the modeled
//! chain's one free variable. This fixture pins that variable to the real
//! chain: the replay's block lists carry EXACTLY the captured identities (the
//! production fetch/decode/seed pipeline sees the same write set the live
//! `bal_carry_probe` advanced through), while the candidate order, schedule,
//! prices, and read values stay the modeled replay's. Values are deliberately
//! NOT captured: the replay serves one shared modeled value chain across both
//! coverage modes (the shared `modeled_chain` value derivation), so the
//! comparison isolates write coverage alone.
//!
//! # Format (dev-only; no wire/ABI/FFI contract)
//!
//! One JSON file, versioned by its `format` tag:
//!
//! ```text
//! { "format": "bal-real-coverage-v1",
//!   "first_block": 26375033, "last_block": 26375053,
//!   "tracked_addresses": ["0x...", ...],
//!   "blocks": [ { "number": 26375033,
//!                 "hash": "0x...", "parent_hash": "0x...",
//!                 "writes": [["0x...", "0x..."], ...],
//!                 "other_writes": 1234 }, ... ] }
//! ```
//!
//! `writes` are the final-transition identities `(address, slot)` over the
//! tracked addresses, sorted; `other_writes` counts the list's writes outside
//! them (the carry's write policy skips those anyway, so the replay's applied
//! set is exactly the live probe's — the count keeps the capture auditable).
//! The raw block access list responses are multi-megabyte and are never
//! stored.
//!
//! # Regeneration
//!
//! ```sh
//! just bal-real-coverage-capture
//! ```
//!
//! (Development-only: dials the declared archive endpoint; the measured replay
//! needs no network.) Loading fails loud on an unknown format tag, a gap in
//! the block range, or a parent hash that is not the previous block's hash —
//! the carry's lineage check is never handed a fixture that lies about its
//! linkage.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

use degenbot_rpc::provider::AlloyProvider;
use degenbot_simulation::sim::evm2::bal_prefetch::{fetch_bal_seed, BalPlan, BalSeed};

/// One tracked read identity.
pub type Key = (Address, U256);

/// The fixture file's format tag: a captured file with any other tag is not
/// this format, and a replay never guesses.
pub const FIXTURE_FORMAT: &str = "bal-real-coverage-v1";

/// One captured block: its exact-hash identity, its real parent hash, and the
/// exact write identities its list carries over the tracked addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealBlock {
    /// The block's canonical number.
    pub number: u64,
    /// The block's canonical hash — the exact hash the list was fetched at.
    pub hash: B256,
    /// The block header's parent hash (the previous block's hash).
    pub parent_hash: B256,
    /// The final-transition identities `(address, slot)`, sorted.
    pub writes: Vec<Key>,
    /// The list's writes outside the tracked addresses — never applied by the
    /// carry's write policy, recorded so the capture is auditable.
    pub other_writes: usize,
}

/// The wire shape of the captured file — plain strings at the boundary, so the
/// file format is independent of any primitive's serde representation.
#[derive(Debug, Serialize, Deserialize)]
struct FixtureFile {
    format: String,
    first_block: u64,
    last_block: u64,
    tracked_addresses: Vec<String>,
    blocks: Vec<BlockFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BlockFile {
    number: u64,
    hash: String,
    parent_hash: String,
    writes: Vec<(String, String)>,
    other_writes: usize,
}

/// The captured stretch: every block of `first..=last` with its real identity,
/// real parent linkage, and exact write identities.
#[derive(Debug)]
pub struct RealCoverage {
    first: u64,
    last: u64,
    tracked_addresses: Vec<Address>,
    blocks: BTreeMap<u64, RealBlock>,
}

impl RealCoverage {
    /// Fold one captured stretch into a fixture, refusing a range with a gap or
    /// parent linkage that does not hold.
    ///
    /// # Errors
    ///
    /// An empty block list, a gap or duplicate in the range, or a block whose
    /// parent hash is not the previous block's hash is a broken capture.
    pub fn from_blocks(
        tracked_addresses: Vec<Address>,
        mut blocks: Vec<RealBlock>,
    ) -> Result<Self, String> {
        if blocks.is_empty() {
            return Err("the fixture holds no captured block".to_string());
        }
        blocks.sort_unstable_by_key(|block| block.number);
        let first = blocks[0].number;
        let last = blocks[blocks.len() - 1].number;
        let mut by_number: BTreeMap<u64, RealBlock> = BTreeMap::new();
        for block in blocks {
            let number = block.number;
            if by_number.insert(number, block).is_some() {
                return Err(format!("the fixture captures block {number} twice"));
            }
        }
        for number in first..=last {
            if !by_number.contains_key(&number) {
                return Err(format!(
                    "the fixture skips block {number} of {first}..={last}"
                ));
            }
        }
        for number in first + 1..=last {
            let block = &by_number[&number];
            let parent = &by_number[&(number - 1)];
            if block.parent_hash != parent.hash {
                return Err(format!(
                    "the fixture's block {number} names parent {} but block {} hashes {}",
                    block.parent_hash,
                    number - 1,
                    parent.hash
                ));
            }
        }
        Ok(Self {
            first,
            last,
            tracked_addresses,
            blocks: by_number,
        })
    }

    /// Load one captured file.
    ///
    /// # Errors
    ///
    /// An unreadable file, bytes that are not this format, or a file whose
    /// blocks fail [`Self::from_blocks`]' checks.
    pub fn load(path: &str) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("read the real-coverage fixture {path}: {error}"))?;
        let file: FixtureFile = serde_json::from_str(&text)
            .map_err(|error| format!("parse the real-coverage fixture {path}: {error}"))?;
        if file.format != FIXTURE_FORMAT {
            return Err(format!(
                "the real-coverage fixture {path} is format {}, not {FIXTURE_FORMAT}",
                file.format
            ));
        }
        let tracked_addresses = file
            .tracked_addresses
            .iter()
            .map(|address| {
                address
                    .parse::<Address>()
                    .map_err(|error| format!("tracked address {address}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let blocks =
            file.blocks
                .iter()
                .map(|block| {
                    let number = block.number;
                    let parse_address = |value: &String| {
                        value.parse::<Address>().map_err(|error| {
                            format!("block {number} write address {value}: {error}")
                        })
                    };
                    let parse_slot = |value: &String| {
                        value
                            .parse::<U256>()
                            .map_err(|error| format!("block {number} write slot {value}: {error}"))
                    };
                    let writes = block
                        .writes
                        .iter()
                        .map(|(address, slot)| Ok((parse_address(address)?, parse_slot(slot)?)))
                        .collect::<Result<Vec<_>, String>>()?;
                    Ok(RealBlock {
                        number,
                        hash: block.hash.parse::<B256>().map_err(|error| {
                            format!("block {number} hash {}: {error}", block.hash)
                        })?,
                        parent_hash: block.parent_hash.parse::<B256>().map_err(|error| {
                            format!("block {number} parent hash {}: {error}", block.parent_hash)
                        })?,
                        writes,
                        other_writes: block.other_writes,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
        Self::from_blocks(tracked_addresses, blocks)
    }

    /// Write the captured file in the documented format.
    ///
    /// # Errors
    ///
    /// A path the process cannot write is a capture failure, never a silent
    /// truncation.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let file = FixtureFile {
            format: FIXTURE_FORMAT.to_string(),
            first_block: self.first,
            last_block: self.last,
            tracked_addresses: self
                .tracked_addresses
                .iter()
                .map(|address| format!("{address:#x}"))
                .collect(),
            blocks: self
                .blocks
                .values()
                .map(|block| BlockFile {
                    number: block.number,
                    hash: format!("{:#x}", block.hash),
                    parent_hash: format!("{:#x}", block.parent_hash),
                    writes: block
                        .writes
                        .iter()
                        .map(|(address, slot)| (format!("{address:#x}"), format!("{slot:#x}")))
                        .collect(),
                    other_writes: block.other_writes,
                })
                .collect(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|error| format!("render the real-coverage fixture: {error}"))?;
        std::fs::write(path, text)
            .map_err(|error| format!("write the real-coverage fixture {path}: {error}"))
    }

    /// The captured stretch's first block.
    #[must_use]
    pub const fn first(&self) -> u64 {
        self.first
    }

    /// The captured stretch's last block.
    #[must_use]
    pub const fn last(&self) -> u64 {
        self.last
    }

    /// One captured block.
    #[must_use]
    pub fn block(&self, number: u64) -> Option<&RealBlock> {
        self.blocks.get(&number)
    }

    /// The block's canonical hash — what the exact-hash list request and the
    /// anchors are keyed by in real coverage.
    ///
    /// # Errors
    ///
    /// A number outside the captured stretch has no captured identity.
    pub fn hash(&self, number: u64) -> Result<B256, String> {
        self.blocks
            .get(&number)
            .map(|block| block.hash)
            .ok_or_else(|| {
                format!(
                    "the fixture captures no block {number} ({}..={})",
                    self.first, self.last
                )
            })
    }

    /// The block header's parent hash — the linkage the carry's lineage check
    /// validates.
    ///
    /// # Errors
    ///
    /// A number outside the captured stretch has no captured linkage.
    pub fn parent_hash(&self, number: u64) -> Result<B256, String> {
        self.blocks
            .get(&number)
            .map(|block| block.parent_hash)
            .ok_or_else(|| {
                format!(
                    "the fixture captures no block {number} ({}..={})",
                    self.first, self.last
                )
            })
    }

    /// The block's exact write identities over the tracked addresses.
    #[must_use]
    pub fn writes_at(&self, number: u64) -> &[Key] {
        self.blocks
            .get(&number)
            .map_or(&[], |block| block.writes.as_slice())
    }

    /// Whether the captured list writes this identity at `number`.
    #[must_use]
    pub fn is_write(&self, number: u64, address: Address, slot: U256) -> bool {
        self.blocks.get(&number).is_some_and(|block| {
            block
                .writes
                .binary_search_by_key(&(address, slot), |w| *w)
                .is_ok()
        })
    }

    /// How many write identities the capture kept across the stretch.
    #[must_use]
    pub fn write_count(&self) -> usize {
        self.blocks.values().map(|block| block.writes.len()).sum()
    }
}

/// Fold one decoded list into the tracked identities and the count left
/// outside them: the capture's filter, in one place.
#[must_use]
pub fn fold_writes(seed: &BalSeed, tracked: &BTreeSet<Address>) -> (Vec<Key>, usize) {
    let mut writes: Vec<Key> = Vec::new();
    let mut other_writes = 0usize;
    for &(address, slot, _) in &seed.writes {
        if tracked.contains(&address) {
            writes.push((address, slot));
        } else {
            other_writes += 1;
        }
    }
    writes.sort_unstable();
    writes.dedup();
    (writes, other_writes)
}

/// Capture the stretch `first..=last` from the live node: each block's real
/// header identity and parent hash, and each exact-hash list's write identities
/// over `tracked`, acquired and folded through the production path
/// ([`fetch_bal_seed`]) exactly as the replay's pipeline will read them — so a
/// node serving only the official JSON BAL endpoint captures the same
/// fixture.
///
/// # Errors
///
/// A missing block, a failed list fetch, bytes the production decoder rejects,
/// or linkage that does not hold is a capture failure, never a partial
/// fixture.
pub async fn capture(
    provider: &AlloyProvider,
    first: u64,
    last: u64,
    tracked: &BTreeSet<Address>,
) -> Result<RealCoverage, String> {
    if first > last {
        return Err(format!("the capture range {first}..={last} is empty"));
    }
    let mut blocks = Vec::new();
    for number in first..=last {
        let block = provider
            .get_block(number)
            .await
            .map_err(|error| format!("get_block({number}): {error}"))?
            .ok_or_else(|| format!("no canonical block at {number}"))?;
        let seed = fetch_bal_seed(provider, block.header.hash, &BalPlan::default())
            .await
            .map_err(|error| format!("list at {number}: {error}"))?;
        let (writes, other_writes) = fold_writes(&seed, tracked);
        blocks.push(RealBlock {
            number,
            hash: block.header.hash,
            parent_hash: block.header.inner.parent_hash,
            writes,
            other_writes,
        });
    }
    let tracked_addresses = tracked.iter().copied().collect();
    RealCoverage::from_blocks(tracked_addresses, blocks)
}
