//! Live chain evidence: the complete parent-linked header chain a historical
//! projection range advances on, and the hash-pinned read view of one exact
//! block (ADR-105 D2/D3).
//!
//! Two facts make a historical projection unit safe to persist: every block of
//! its range carries a COMPLETE header whose parent link is verified (so a
//! range with no matching logs still proves which blocks it covered), and every
//! read that supports it is pinned to the block HASH rather than a height (so a
//! same-height branch cannot serve a read for the block the unit claims).
//!
//! [`acquire_header_chain`] is the acquisition half. One header read per block
//! ([`AlloyProvider::get_block_lineage_at`] answers number + hash + parent hash
//! in one round trip), bounded in flight, and the chain is refused unless every
//! link validates. [`ProjectionRange`] is the unit those headers carry: the
//! range's headers plus the filtered logs, with every log required to agree with
//! the header it claims to have been emitted in and the unit stated in the
//! canonical `(block_number, log_index)` order.
//!
//! [`BlockReads`] is the read half. It carries ONE [`ChainAnchor`] and every
//! method inherits it — no method takes a block argument, so a caller cannot
//! ask a snapshot for another block. Reads use EIP-1898
//! (`{ blockHash, requireCanonical: true }`); a node that cannot serve that
//! request shape is a typed node-evidence failure
//! ([`ExactReadError::Unsupported`]), never a number-only fallback.
//!
//! Every entry point takes ONE [`AlloyProvider`], so the acquisition surface is
//! the same live and recorded: a cassette replay transport presents as a
//! provider, and a recorded header chain or hash-pinned read answers through
//! these same functions ([`crate::cassette_replay`]). [`resolve_terminal_anchor`]
//! is the same seam at the range's END: a run's tip or explicit bound becomes
//! one [`ChainAnchor`] before any projection work starts.

use alloy::eips::{BlockId, RpcBlockHash};
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::rpc::types::Log;
use degenbot_core::canonical_head::{AnchorLinkError, ChainAnchor};
use degenbot_core::errors::{ProviderError, ProviderResult};
use futures_util::{stream, StreamExt};
use serde_json::json;

use crate::provider::{AlloyProvider, LogFetcher, LogFilter};

/// How many header reads one acquisition keeps in flight. A header read is one
/// small `eth_getBlockByNumber` (no body, no transactions), so the bound is
/// about not flooding an endpoint, not about payload size.
const MAX_CONCURRENT_HEADER_READS: usize = 8;

/// The JSON-RPC error codes a node answers with when it rejects a request's
/// SHAPE: `-32601` method-not-found and `-32602` invalid-params. A node
/// without EIP-1898 hash-pinned reads rejects the block identifier this way.
const UNSUPPORTED_SHAPE_CODES: [i64; 2] = [-32_601, -32_602];

/// Message fragments that name a rejected EIP-1898 block IDENTIFIER, for nodes
/// that answer with a `-32000`-family code instead of a params code.
///
/// Deliberately narrow: a fragment must name the request SHAPE, never the
/// object a read was about. Generic words ("unsupported", "not supported")
/// appear in ordinary revert and fault text, so matching them would turn a
/// legitimate `ExactReadError::Failed` into a capability verdict the node never
/// stated.
const UNSUPPORTED_SHAPE_MARKERS: [&str; 3] =
    ["requirecanonical", "block identifier", "unknown parameter"];

/// The complete parent-linked header chain of one contiguous block range:
/// every height in the range, exactly once, each block naming the previous
/// block's own hash as its parent.
///
/// Construction is the only door: a `HeaderChain` that exists has had every
/// link verified, so an empty-log range can advance its cursor on the strength
/// of the chain alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderChain {
    anchors: Vec<ChainAnchor>,
}

impl HeaderChain {
    /// The chain of `anchors`, ascending by height and linked parent to child.
    ///
    /// # Errors
    ///
    /// [`HeaderRefusal::EmptyRange`] when `anchors` is empty, and one refusal
    /// per broken link: [`HeaderRefusal::CrossChainParent`] (a parent on
    /// another chain), [`HeaderRefusal::SameHeightSubstitution`] (two blocks at
    /// one height), [`HeaderRefusal::NotConsecutive`] (a gap or a rewind), and
    /// [`HeaderRefusal::BrokenParentLink`] (the child names a different parent
    /// than the block before it).
    pub fn from_anchors(anchors: Vec<ChainAnchor>) -> Result<Self, HeaderRefusal> {
        let Some(first) = anchors.first().copied() else {
            return Err(HeaderRefusal::EmptyRange);
        };
        let mut parent = first;
        for anchor in &anchors[1..] {
            if anchor.number() == parent.number() {
                return Err(HeaderRefusal::SameHeightSubstitution {
                    number: anchor.number(),
                });
            }
            anchor.links_from(&parent).map_err(|error| match error {
                AnchorLinkError::ChainMismatch {
                    head: child,
                    parent,
                } => HeaderRefusal::CrossChainParent {
                    number: anchor.number(),
                    child,
                    parent,
                },
                AnchorLinkError::NotConsecutive { head, parent } => {
                    HeaderRefusal::NotConsecutive { head, parent }
                }
                AnchorLinkError::ParentHashMismatch { expected, actual } => {
                    HeaderRefusal::BrokenParentLink {
                        number: anchor.number(),
                        expected,
                        actual,
                    }
                }
            })?;
            parent = *anchor;
        }
        Ok(Self { anchors })
    }

    /// The range's anchors, ascending by height.
    #[must_use]
    pub fn anchors(&self) -> &[ChainAnchor] {
        &self.anchors
    }

    /// How many blocks the range covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    /// Whether the range covers no blocks. A `HeaderChain` never does — this
    /// exists so the empty check is named where callers need it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// The first (lowest) height the range covers.
    ///
    /// # Panics
    ///
    /// Never: construction refuses an empty chain, so there is always a first
    /// anchor.
    #[must_use]
    #[expect(clippy::expect_used)]
    pub fn from_block(&self) -> u64 {
        self.anchors
            .first()
            .expect("a header chain covers at least one block")
            .number()
    }

    /// The last (highest) height the range covers — the terminal header's.
    ///
    /// # Panics
    ///
    /// Never: construction refuses an empty chain.
    #[must_use]
    pub fn to_block(&self) -> u64 {
        self.terminal().number()
    }

    /// The range's terminal (highest) block — the header a projection unit
    /// closes on.
    ///
    /// # Panics
    ///
    /// Never: construction refuses an empty chain.
    #[must_use]
    #[expect(clippy::expect_used)]
    pub fn terminal(&self) -> ChainAnchor {
        *self
            .anchors
            .last()
            .expect("a header chain covers at least one block")
    }

    /// The anchor this chain holds at `number`, if the range covers it.
    #[must_use]
    pub fn anchor_at(&self, number: u64) -> Option<ChainAnchor> {
        self.anchors
            .iter()
            .find(|anchor| anchor.number() == number)
            .copied()
    }

    /// The logs proven to have been emitted in blocks this chain names, in the
    /// unit's canonical `(block_number, log_index)` order.
    ///
    /// # Errors
    ///
    /// One [`ProjectionRefusal`] per log that cannot be placed: no block
    /// number, no log index, no block hash, a height this chain does not cover,
    /// or a block hash that is not the chain's hash at that height.
    pub fn place_logs(&self, logs: Vec<Log>) -> Result<Vec<Log>, ProjectionRefusal> {
        let (from, to) = (self.from_block(), self.to_block());
        let mut logs = logs;
        for log in &logs {
            let Some(number) = log.block_number else {
                return Err(ProjectionRefusal::LogWithoutBlockNumber);
            };
            if !(from..=to).contains(&number) {
                return Err(ProjectionRefusal::LogOutsideRange { number, from, to });
            }
            if log.log_index.is_none() {
                return Err(ProjectionRefusal::LogWithoutIndex { number });
            }
            let Some(block_hash) = log.block_hash else {
                return Err(ProjectionRefusal::LogWithoutBlockHash { number });
            };
            let Some(header) = self.anchor_at(number) else {
                return Err(ProjectionRefusal::LogOutsideRange { number, from, to });
            };
            if header.hash() != block_hash {
                return Err(ProjectionRefusal::LogBranchMismatch {
                    number,
                    header: header.hash(),
                    log: block_hash,
                });
            }
        }
        logs.sort_by_key(|log| (log.block_number, log.log_index));
        Ok(logs)
    }
}

/// Prove every log of `logs` was emitted in `anchor`'s own block: the
/// single-block sibling of [`HeaderChain::place_logs`].
///
/// A consumer that resolved ONE exact block (a run's range terminal, say)
/// fences the logs at that height against the resolved anchor, so a same-height
/// substitution between resolution and fetch is a refusal rather than a chunk
/// accepted on a number alone.
///
/// # Errors
///
/// [`ProjectionRefusal::LogOutsideRange`] for a log at another height, and
/// the [`HeaderChain::place_logs`] refusals for a log that cannot be placed in
/// `anchor`'s block.
pub fn place_logs_at(anchor: ChainAnchor, logs: Vec<Log>) -> Result<Vec<Log>, ProjectionRefusal> {
    HeaderChain {
        anchors: vec![anchor],
    }
    .place_logs(logs)
}

/// Why a header chain was refused. Every variant is deterministic evidence
/// about the headers themselves — retrying the same read cannot change it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderRefusal {
    /// No block was named at all.
    #[error("no header names a block")]
    EmptyRange,
    /// The node holds no header at this height, or served one without its full
    /// identity: the range's header chain is incomplete and cannot advance a
    /// cursor.
    #[error("the node holds no complete header at block {number}")]
    MissingHeader {
        /// The height with no complete header.
        number: u64,
    },
    /// The child's parent lives on another chain. Two chains can share a
    /// height, and even a hash is chain-local evidence, so no cross-chain pair
    /// links.
    #[error("block {number} on chain {child} names a parent on chain {parent}")]
    CrossChainParent {
        /// The child's height.
        number: u64,
        /// The child's chain.
        child: u64,
        /// The parent's chain.
        parent: u64,
    },
    /// Two blocks claim one height: at most one block per height can be in a
    /// range, and a same-height replacement is a different identity rather than
    /// a moved one (ADR-105 D9).
    #[error("block {number} repeats a height already covered by another block")]
    SameHeightSubstitution {
        /// The repeated height.
        number: u64,
    },
    /// The heights do not advance one step: a gap, a repeat, or a rewind.
    #[error("block {head} does not advance from block {parent}")]
    NotConsecutive {
        /// The offending height.
        head: u64,
        /// The height before it.
        parent: u64,
    },
    /// The child names the next height but builds on a different block than the
    /// header before it — the branch-disagreement shape a cursor must refuse
    /// rather than treat as an ordinary advance.
    #[error("block {number} names parent {actual}, but the block before it hashes to {expected}")]
    BrokenParentLink {
        /// The child's height.
        number: u64,
        /// The preceding header's own hash.
        expected: B256,
        /// The parent hash the child claims.
        actual: B256,
    },
}

/// Why a projection unit was refused: its logs must have been emitted in the
/// exact blocks its headers name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionRefusal {
    /// A log that names no block cannot be attributed to any header.
    #[error("a log in the range names no block number")]
    LogWithoutBlockNumber,
    /// A log that names no block hash cannot be proven to belong to the header
    /// chain (a height alone cannot see a same-height branch).
    #[error("the log at block {number} names no block hash")]
    LogWithoutBlockHash {
        /// The height the log claims.
        number: u64,
    },
    /// A log that names no index has no canonical position in the unit: the
    /// `(block_number, log_index)` order the unit promises cannot place it.
    #[error("the log at block {number} names no log index")]
    LogWithoutIndex {
        /// The height the log claims.
        number: u64,
    },
    /// The log's height is outside the range its headers cover.
    #[error("the log at block {number} is outside the range {from}-{to}")]
    LogOutsideRange {
        /// The height the log claims.
        number: u64,
        /// The range's first height.
        from: u64,
        /// The range's last height.
        to: u64,
    },
    /// The log's block hash is not the header chain's hash at that height: the
    /// log was emitted in a different block than the one the unit claims.
    #[error("the log at block {number} carries hash {log}, but the header hashes to {header}")]
    LogBranchMismatch {
        /// The height the log claims.
        number: u64,
        /// The header chain's hash at that height.
        header: B256,
        /// The hash the log carries.
        log: B256,
    },
}

/// Why live chain evidence could not be acquired.
#[derive(Debug, thiserror::Error)]
pub enum ChainEvidenceError {
    /// The transport or the node failed the read.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The headers themselves refuse the range.
    #[error(transparent)]
    Header(#[from] HeaderRefusal),
    /// The logs disagree with the headers.
    #[error(transparent)]
    Projection(#[from] ProjectionRefusal),
    /// A projection range must name its span: a filter without both bounds has
    /// no header chain to carry.
    #[error("a projection range needs an explicit from_block and to_block")]
    UnboundedFilter,
    /// The node served the range's terminal block on another chain: an anchor
    /// is chain-local evidence, so a run for one chain never closes on another
    /// chain's block.
    #[error("terminal block {number} is on chain {served}, but this range is chain {expected}")]
    CrossChainTerminal {
        /// The terminal height.
        number: u64,
        /// The chain the caller's range belongs to.
        expected: u64,
        /// The chain the served header states.
        served: u64,
    },
}

/// One historical projection unit: the complete header chain of a block range
/// plus the logs filtered out of it.
///
/// The headers are the unit's completeness claim — every block of the range is
/// named, including the blocks that emitted no matching log — and the logs must
/// have been emitted in exactly those blocks.
#[derive(Debug, Clone)]
pub struct ProjectionRange {
    headers: HeaderChain,
    logs: Vec<Log>,
}

impl ProjectionRange {
    /// The unit of `headers` and `logs`.
    ///
    /// # Errors
    ///
    /// One [`ProjectionRefusal`] per log that cannot be placed in the header
    /// chain: no block number, no log index, no block hash, a height outside the
    /// range, or a block hash that is not the header's hash at that height.
    ///
    /// Accepted logs are ordered by `(block_number, log_index)` — the unit's
    /// canonical order, so a caller reads the range exactly as the chain states
    /// it regardless of how the node or a filter answered.
    pub fn new(headers: HeaderChain, logs: Vec<Log>) -> Result<Self, ProjectionRefusal> {
        let logs = headers.place_logs(logs)?;
        Ok(Self { headers, logs })
    }

    /// Acquire the unit for `filter`'s span: the complete header chain first,
    /// then the filtered logs through `fetcher`, then the agreement check.
    ///
    /// # Errors
    ///
    /// [`ChainEvidenceError::UnboundedFilter`] when the filter names no span,
    /// the header or projection refusals above, and
    /// [`ChainEvidenceError::Provider`] when a read fails.
    pub async fn acquire(
        provider: &AlloyProvider,
        fetcher: &LogFetcher,
        filter: &LogFilter,
    ) -> Result<Self, ChainEvidenceError> {
        let (Some(from_block), Some(to_block)) = (filter.from_block(), filter.to_block()) else {
            return Err(ChainEvidenceError::UnboundedFilter);
        };
        let headers = acquire_header_chain(provider, from_block, to_block).await?;
        let logs = fetcher
            .fetch_logs_chunked(
                from_block,
                to_block,
                Some(filter.address_strings()),
                Some(filter.topic_strings()),
            )
            .await?;
        Self::new(headers, logs).map_err(ChainEvidenceError::from)
    }

    /// The unit's complete header chain.
    #[must_use]
    pub const fn headers(&self) -> &HeaderChain {
        &self.headers
    }

    /// The unit's logs, sorted by `(block_number, log_index)`.
    #[must_use]
    pub fn logs(&self) -> &[Log] {
        &self.logs
    }

    /// The block span the unit covers, inclusive on both ends.
    #[must_use]
    pub fn span(&self) -> std::ops::RangeInclusive<u64> {
        self.headers.from_block()..=self.headers.to_block()
    }
}

/// Acquire the complete parent-linked header chain of `from_block..=to_block`.
///
/// Every height in the range is read (`eth_getBlockByNumber`, no body), so a
/// range that emitted no matching log still carries the blocks it covered, and
/// the chain is refused unless each header links to the one before it.
///
/// # Errors
///
/// [`HeaderRefusal::EmptyRange`] for a range that names no block,
/// [`HeaderRefusal::MissingHeader`] for a height the node answers with no
/// complete header, one refusal per broken link (see
/// [`HeaderChain::from_anchors`]), and [`ChainEvidenceError::Provider`] when a
/// read fails.
pub async fn acquire_header_chain(
    provider: &AlloyProvider,
    from_block: u64,
    to_block: u64,
) -> Result<HeaderChain, ChainEvidenceError> {
    if from_block > to_block {
        return Err(HeaderRefusal::EmptyRange.into());
    }
    let reads = stream::iter(from_block..=to_block)
        .map(|number| async move {
            provider
                .get_block_lineage_at(number)
                .await
                .map(|anchor| (number, anchor))
        })
        .buffer_unordered(MAX_CONCURRENT_HEADER_READS)
        .collect::<Vec<_>>()
        .await;
    let mut answers = Vec::with_capacity(reads.len());
    for read in reads {
        answers.push(read?);
    }
    answers.sort_by_key(|(number, _)| *number);
    let mut anchors = Vec::with_capacity(answers.len());
    for (number, anchor) in answers {
        anchors.push(anchor.ok_or(HeaderRefusal::MissingHeader { number })?);
    }
    HeaderChain::from_anchors(anchors).map_err(ChainEvidenceError::from)
}

/// Resolve a range's terminal block — the chain tip when `to_block` is `None`,
/// the caller's own bound otherwise — to the exact [`ChainAnchor`] the range
/// closes on.
///
/// BOTH arms take this one path: the height is resolved, then ONE header read
/// states the block's identity (`eth_getBlockByNumber`, no body). A tip run
/// asks the node for its head number first, so no caller advances on a height
/// whose block was never named; a bound run pays the header read alone.
///
/// # Errors
///
/// [`ChainEvidenceError::Provider`] when a read fails,
/// [`ChainEvidenceError::Header`] with [`HeaderRefusal::MissingHeader`] when the
/// node holds no complete header at that height, and
/// [`ChainEvidenceError::CrossChainTerminal`] when the served block belongs to
/// another chain than `chain_id`.
pub async fn resolve_terminal_anchor(
    provider: &AlloyProvider,
    chain_id: u64,
    to_block: Option<u64>,
) -> Result<ChainAnchor, ChainEvidenceError> {
    let number = match to_block {
        Some(number) => number,
        None => provider.get_block_number().await?,
    };
    let served = resolve_anchor_at(provider, number).await?;
    if served.chain_id() != chain_id {
        return Err(ChainEvidenceError::CrossChainTerminal {
            number,
            expected: chain_id,
            served: served.chain_id(),
        });
    }
    Ok(served)
}

/// Resolve one height to its exact [`ChainAnchor`]: ONE header read.
///
/// The served block's chain is not judged here — a caller that needs chain
/// agreement states it itself (see [`resolve_terminal_anchor`]) — so a
/// height-addressed caller (a verification block, a recorded span's substrate
/// block) reaches an exact read position without changing its own shape.
///
/// # Errors
///
/// [`ChainEvidenceError::Provider`] when the read fails and
/// [`ChainEvidenceError::Header`] with [`HeaderRefusal::MissingHeader`] when the
/// node holds no complete header at that height.
pub async fn resolve_anchor_at(
    provider: &AlloyProvider,
    number: u64,
) -> Result<ChainAnchor, ChainEvidenceError> {
    provider
        .get_block_lineage_at(number)
        .await?
        .ok_or_else(|| HeaderRefusal::MissingHeader { number }.into())
}

/// Why one hash-pinned read failed.
///
/// The vocabulary separates "this node cannot serve an exact read" from "this
/// node failed a read", because only the first is a statement about the node's
/// evidence capability (ADR-105 D11) — and neither degrades to a number-only
/// read.
#[derive(Debug, thiserror::Error)]
pub enum ExactReadError {
    /// The node has no EIP-1898 hash-pinned read capability for this method:
    /// its answer rejects the request SHAPE, so the snapshot cannot be served
    /// exactly.
    #[error("{method} cannot serve a hash-pinned read at block {block_hash}: {cause}")]
    Unsupported {
        /// The JSON-RPC method whose shape was rejected.
        method: &'static str,
        /// The block the read was pinned to.
        block_hash: B256,
        /// The node's refusal.
        cause: ProviderError,
    },
    /// The node failed the read for another reason (transport, revert, pruned
    /// state, ...).
    #[error("{method} failed at block {block_hash}: {cause}")]
    Failed {
        /// The JSON-RPC method that failed.
        method: &'static str,
        /// The block the read was pinned to.
        block_hash: B256,
        /// The failure.
        cause: ProviderError,
    },
}

/// The read view of ONE exact block: every method inherits the anchor, and no
/// method accepts a block argument.
///
/// Reads are EIP-1898 hash-pinned (`{ blockHash, requireCanonical: true }`), so
/// the node either serves the exact block the snapshot names or refuses — no
/// path in this type re-resolves the anchor by height.
pub struct BlockReads<'a> {
    provider: &'a AlloyProvider,
    anchor: ChainAnchor,
}

impl<'a> BlockReads<'a> {
    /// The read view of `anchor` over `provider`.
    #[must_use]
    pub const fn at(provider: &'a AlloyProvider, anchor: ChainAnchor) -> Self {
        Self { provider, anchor }
    }

    /// The exact block this view reads at.
    #[must_use]
    pub const fn anchor(&self) -> ChainAnchor {
        self.anchor
    }

    /// The EIP-1898 identifier every account, storage, code, and call read
    /// pins: the snapshot's own hash, required to still be canonical.
    ///
    /// The account, storage, and code reads hand it to the transport's own
    /// `BlockId` lane, so they inherit the single request construction each of
    /// those operations owns. `eth_call` states the identifier in its own
    /// params because the recorded corpus carries the `data` field on the
    /// pinned lane (the typed request builder emits `input`), so moving it onto
    /// that builder would rewrite the recorded wire.
    fn pinned_id(&self) -> BlockId {
        BlockId::Hash(RpcBlockHash::from_hash(self.anchor.hash(), Some(true)))
    }

    /// The EIP-1898 identifier as the raw params value `eth_call` carries.
    fn pinned_block(&self) -> serde_json::Value {
        json!({ "blockHash": format!("{}", self.anchor.hash()), "requireCanonical": true })
    }

    /// An address in the wire form the recorded corpus carries (lowercase hex).
    fn wire_address(address: Address) -> String {
        format!("0x{}", alloy::hex::encode(address.as_slice()))
    }

    /// `eth_call` at the snapshot's block.
    ///
    /// # Errors
    ///
    /// [`ExactReadError::Unsupported`] when the node rejects the hash-pinned
    /// request shape, [`ExactReadError::Failed`] otherwise.
    pub async fn call(&self, to: Address, data: Bytes) -> Result<Bytes, ExactReadError> {
        self.pinned_call(to, data).await
    }

    /// The `eth_call` body both the exact method and the [`ContractRead`]
    /// bridge share: one implementation, one pin.
    async fn pinned_call(&self, to: Address, data: Bytes) -> Result<Bytes, ExactReadError> {
        let params = json!([
            { "to": Self::wire_address(to), "data": format!("{data}") },
            self.pinned_block()
        ]);
        self.read("eth_call", params).await
    }

    /// `eth_getCode` at the snapshot's block.
    ///
    /// # Errors
    ///
    /// [`ExactReadError::Unsupported`] when the node rejects the hash-pinned
    /// request shape, [`ExactReadError::Failed`] otherwise.
    pub async fn code_at(&self, address: Address) -> Result<Bytes, ExactReadError> {
        self.provider
            .get_code(&address, Some(self.pinned_id()))
            .await
            .map_err(|cause| self.classify("eth_getCode", cause))
    }

    /// `eth_getStorageAt` at the snapshot's block: the storage word `slot`.
    ///
    /// # Errors
    ///
    /// [`ExactReadError::Unsupported`] when the node rejects the hash-pinned
    /// request shape, [`ExactReadError::Failed`] otherwise.
    pub async fn storage_at(&self, address: Address, slot: U256) -> Result<U256, ExactReadError> {
        let word = self
            .provider
            .get_storage_at(&address, slot, Some(self.pinned_id()))
            .await
            .map_err(|cause| self.classify("eth_getStorageAt", cause))?;
        Ok(U256::from_be_bytes(word.0))
    }

    /// Run a batch of read-only calls at the snapshot's block through ONE
    /// pinned Multicall3 `aggregate3` (the [`crate::multicall3::multicall3_batch`]
    /// shape, one envelope per chunk), and answer no request for an empty list.
    ///
    /// A reverted sub-call is a `MulticallOutcome::Reverted` — an outcome, not
    /// an error — and a node without the hash-pinned shape refuses the batch
    /// itself rather than degrading to number-addressed reads.
    ///
    /// # Errors
    ///
    /// [`ExactReadError::Unsupported`] when the node rejects the hash-pinned
    /// request shape, [`ExactReadError::Failed`] when a read failed.
    pub async fn batch(
        &self,
        calls: &[(Address, Bytes)],
    ) -> Result<Vec<crate::multicall3::MulticallOutcome>, ExactReadError> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        crate::multicall3::multicall3_batch_pinned(self, calls).await
    }

    /// `eth_getBalance` at the snapshot's block.
    ///
    /// # Errors
    ///
    /// [`ExactReadError::Unsupported`] when the node rejects the hash-pinned
    /// request shape, [`ExactReadError::Failed`] otherwise.
    pub async fn balance_at(&self, address: Address) -> Result<U256, ExactReadError> {
        self.provider
            .get_balance(&address, Some(self.pinned_id()))
            .await
            .map_err(|cause| self.classify("eth_getBalance", cause))
    }

    /// One read through `method`, classified into the capability vocabulary.
    async fn read<T>(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<T, ExactReadError>
    where
        T: serde::de::DeserializeOwned + std::fmt::Debug + Send + Sync + Unpin + 'static,
    {
        self.provider
            .request_raw(method, params, "hash-pinned read failed")
            .await
            .map_err(|cause| self.classify(method, cause))
    }

    /// Name a failed read: a node that rejects the request SHAPE has no
    /// hash-pinned capability, which is a statement about its evidence, not a
    /// transport fault.
    fn classify(&self, method: &'static str, cause: ProviderError) -> ExactReadError {
        let block_hash = self.anchor.hash();
        if rejects_hash_pinned_shape(&cause) {
            ExactReadError::Unsupported {
                method,
                block_hash,
                cause,
            }
        } else {
            ExactReadError::Failed {
                method,
                block_hash,
                cause,
            }
        }
    }
}

/// The exact view as the shared [`ContractRead`] primitive: the ERC-20
/// metadata and account reads that family declares run over an exact position
/// unchanged, so there is ONE read implementation and two pin policies.
///
/// A caller's block lane is a cross-check, never a pin: the view's own height is
/// accepted, and any other height is refused rather than silently re-pinned.
impl crate::abi::ContractRead for BlockReads<'_> {
    async fn call(&self, to: Address, data: Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
        if let Some(number) = block {
            if number != self.anchor.number() {
                return Err(ProviderError::InvalidParams {
                    message: format!(
                        "the exact read view at block {} cannot serve a read stated for block {number}",
                        self.anchor.number()
                    ),
                });
            }
        }
        self.pinned_call(to, data)
            .await
            .map_err(ProviderError::from)
    }
}

/// The exact-read failure as the shared provider failure, for the read family
/// that declares `ProviderError` only: a refused capability stays a refusal of
/// the request SHAPE (`-32602`), and a failed read keeps its own cause.
impl From<ExactReadError> for ProviderError {
    fn from(error: ExactReadError) -> Self {
        match error {
            ExactReadError::Unsupported {
                method,
                block_hash,
                cause,
            } => Self::RpcError {
                code: -32_602,
                message: format!(
                    "{method} cannot serve a hash-pinned read at block {block_hash}: {cause}"
                ),
            },
            ExactReadError::Failed { cause, .. } => cause,
        }
    }
}

impl crate::multicall3::ExactContractRead for BlockReads<'_> {
    fn anchor(&self) -> ChainAnchor {
        self.anchor
    }

    async fn call_exact(&self, to: Address, data: Bytes) -> Result<Bytes, ExactReadError> {
        self.call(to, data).await
    }
}

/// Why a block's parent read position could not be named.
///
/// A parent position is an EXACT identity, not a height: the node states the
/// parent's own header, and that header must be the block its child names as its
/// parent. A same-height substitution between the two reads is a refusal, never a
/// silently re-read parent.
#[derive(Debug, thiserror::Error)]
pub enum ParentAnchorError {
    /// A genesis block has no predecessor.
    #[error("block 0 has no parent")]
    Genesis,
    /// The node holds no complete header at the parent height.
    #[error("the node holds no complete header at block {number}")]
    MissingHeader {
        /// The parent height.
        number: u64,
    },
    /// The parent height's block is not the block the child names: the
    /// same-height substitution a branch-local read position must refuse.
    #[error("block {number} hashes to {served}, but its child names {expected} as its parent")]
    BranchMismatch {
        /// The parent height.
        number: u64,
        /// The hash the child states as its parent.
        expected: B256,
        /// The hash the node serves at that height.
        served: B256,
    },
    /// The parent header names another chain than its child.
    #[error("parent block {number} is on chain {served}, but its child is on chain {expected}")]
    CrossChainParent {
        /// The parent height.
        number: u64,
        /// The child's chain.
        expected: u64,
        /// The chain the served parent header states.
        served: u64,
    },
    /// The transport or the node failed the header read.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Resolve `anchor`'s parent to its own exact [`ChainAnchor`]: the read position
/// of an INTENTIONAL pre-block observation (a `balanceOf` at the block before the
/// transaction that acts on it).
///
/// One header read at the parent height, then the agreement check that makes the
/// position exact: the served block must hash to `anchor.parent_hash()` on
/// `anchor`'s chain. A node that has since moved to another branch at that height
/// is refused ([`ParentAnchorError::BranchMismatch`]) rather than read from.
///
/// # Errors
///
/// [`ParentAnchorError::Genesis`] for a block with no predecessor (no read is
/// issued), [`ParentAnchorError::MissingHeader`] when the node states no parent
/// header, [`ParentAnchorError::CrossChainParent`] or
/// [`ParentAnchorError::BranchMismatch`] when the served parent contradicts the
/// child, and [`ParentAnchorError::Provider`] when the read fails.
pub async fn resolve_parent_anchor(
    provider: &AlloyProvider,
    anchor: ChainAnchor,
) -> Result<ChainAnchor, ParentAnchorError> {
    let number = anchor
        .number()
        .checked_sub(1)
        .ok_or(ParentAnchorError::Genesis)?;
    let served = provider
        .get_block_lineage_at(number)
        .await?
        .ok_or(ParentAnchorError::MissingHeader { number })?;
    if served.chain_id() != anchor.chain_id() {
        return Err(ParentAnchorError::CrossChainParent {
            number,
            expected: anchor.chain_id(),
            served: served.chain_id(),
        });
    }
    if served.hash() != anchor.parent_hash() {
        return Err(ParentAnchorError::BranchMismatch {
            number,
            expected: anchor.parent_hash(),
            served: served.hash(),
        });
    }
    Ok(served)
}

/// Whether the node's failure rejects the EIP-1898 request SHAPE (no
/// hash-pinned read capability) rather than a particular read.
fn rejects_hash_pinned_shape(cause: &ProviderError) -> bool {
    let ProviderError::RpcError { code, message } = cause else {
        return false;
    };
    if UNSUPPORTED_SHAPE_CODES.contains(code) {
        return true;
    }
    // The message markers are the node's own statement about the request, so
    // they are read only in the `-32000` family a node uses for its execution
    // and block errors. The reserved JSON-RPC band (-32600..-32699) is a
    // protocol-level answer — an internal error is not a capability verdict —
    // and a transport or harness that echoes the request into its own
    // diagnostic must not turn a missing fixture into one.
    if !(-32_099..=-32_000).contains(code) {
        return false;
    }
    let lower = message.to_lowercase();
    UNSUPPORTED_SHAPE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    use alloy::providers::RootProvider;
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::json_rpc::{
        ErrorPayload, RequestPacket, Response, ResponsePacket, ResponsePayload, SerializedRequest,
    };
    use alloy::transports::{TransportError, TransportFut};
    use serde_json::Value;
    use std::collections::BTreeMap;

    use crate::multicall3::{MulticallOutcome, MULTICALL3_ADDRESS};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    const CHAIN: u64 = 1;

    fn hash(tag: u8) -> B256 {
        B256::new([tag; 32])
    }

    fn anchor(number: u64, hash_tag: u8, parent_tag: u8) -> ChainAnchor {
        ChainAnchor::new(CHAIN, number, hash(hash_tag), hash(parent_tag))
            .expect("the fixture anchor carries every identity field")
    }

    fn header_json(number: u64, hash_tag: u8, parent_tag: u8) -> Value {
        json!({
            "number": format!("0x{number:x}"),
            "hash": format!("{}", hash(hash_tag)),
            "parentHash": format!("{}", hash(parent_tag)),
        })
    }

    fn log_value(number: u64, block_hash: B256, index: u64) -> Value {
        json!({
            "address": "0x1111111111111111111111111111111111111111",
            "topics": [],
            "data": "0x",
            "blockNumber": format!("0x{number:x}"),
            "blockHash": format!("{block_hash}"),
            "transactionHash": format!("0x{:064x}", index + 1),
            "transactionIndex": "0x0",
            "logIndex": format!("0x{index:x}"),
            "removed": false,
        })
    }

    fn log_at(number: u64, block_hash: B256, index: u64) -> Log {
        serde_json::from_value(log_value(number, block_hash, index))
            .expect("a complete fixture log")
    }

    fn log_missing_block_hash(number: u64, index: u64) -> Log {
        serde_json::from_value(json!({
            "address": "0x1111111111111111111111111111111111111111",
            "topics": [],
            "data": "0x",
            "blockNumber": format!("0x{number:x}"),
            "transactionHash": format!("0x{:064x}", index + 1),
            "transactionIndex": "0x0",
            "logIndex": format!("0x{index:x}"),
            "removed": false,
        }))
        .expect("a fixture log without its block hash")
    }

    fn log_missing_index(number: u64, block_hash: B256) -> Log {
        serde_json::from_value(json!({
            "address": "0x1111111111111111111111111111111111111111",
            "topics": [],
            "data": "0x",
            "blockNumber": format!("0x{number:x}"),
            "blockHash": format!("{block_hash}"),
            "transactionHash": format!("0x{:064x}", number),
            "transactionIndex": "0x0",
            "removed": false,
        }))
        .expect("a fixture log without its index")
    }

    fn log_missing_block_number(block_hash: B256, index: u64) -> Log {
        serde_json::from_value(json!({
            "address": "0x1111111111111111111111111111111111111111",
            "topics": [],
            "data": "0x",
            "blockHash": format!("{block_hash}"),
            "transactionHash": format!("0x{:064x}", index + 1),
            "transactionIndex": "0x0",
            "logIndex": format!("0x{index:x}"),
            "removed": false,
        }))
        .expect("a fixture log without its block number")
    }

    /// The node one acquisition or one read view talks to, in memory: headers
    /// by height, one logs answer, one answer per read method, and the switch
    /// that makes it refuse every hash-pinned request shape.
    #[derive(Clone)]
    struct EvidenceStub {
        state: Arc<Mutex<StubState>>,
        packets: Arc<Mutex<Vec<(String, String)>>>,
    }

    #[derive(Clone, Default)]
    struct StubState {
        headers: BTreeMap<u64, Value>,
        logs: Value,
        reads: BTreeMap<&'static str, Value>,
        failures: BTreeMap<&'static str, (i64, String)>,
        /// The Multicall3 address's own answer (an `aggregate3` envelope), so a
        /// batch leg is observable apart from the per-target reads.
        batch: Option<Result<Value, (i64, String)>>,
        /// Whether a hash-pinned read must name a hash the stub holds as a
        /// canonical header — a node that refuses a non-canonical block hash.
        canonical_only: bool,
        reject_hash_pinned: bool,
        /// The chain `eth_chainId` serves; `None` is the fixture's chain 1.
        chain: Option<u64>,
    }

    impl EvidenceStub {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(StubState {
                    headers: BTreeMap::new(),
                    logs: Value::Array(Vec::new()),
                    reads: BTreeMap::new(),
                    failures: BTreeMap::new(),
                    batch: None,
                    canonical_only: false,
                    reject_hash_pinned: false,
                    chain: None,
                })),
                packets: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn header(self, number: u64, hash_tag: u8, parent_tag: u8) -> Self {
            self.state
                .lock()
                .unwrap()
                .headers
                .insert(number, header_json(number, hash_tag, parent_tag));
            self
        }

        fn logs(self, logs: Value) -> Self {
            self.state.lock().unwrap().logs = logs;
            self
        }

        fn read(self, method: &'static str, result: Value) -> Self {
            self.state.lock().unwrap().reads.insert(method, result);
            self
        }

        fn failing(self, method: &'static str, code: i64, message: &str) -> Self {
            self.state
                .lock()
                .unwrap()
                .failures
                .insert(method, (code, message.to_string()));
            self
        }

        fn rejecting_hash_pinned(self) -> Self {
            self.state.lock().unwrap().reject_hash_pinned = true;
            self
        }

        /// Serve this `aggregate3` envelope from the Multicall3 address.
        fn batch(self, envelope: Value) -> Self {
            self.state.lock().unwrap().batch = Some(Ok(envelope));
            self
        }

        /// Refuse any hash-pinned read whose hash is not one of the headers the
        /// stub holds: the same-height branch the node no longer serves.
        fn canonical_only(self) -> Self {
            self.state.lock().unwrap().canonical_only = true;
            self
        }

        /// Serve this chain id from `eth_chainId`: an endpoint bound to another
        /// chain than the caller's market.
        fn chain(self, chain: u64) -> Self {
            self.state.lock().unwrap().chain = Some(chain);
            self
        }

        fn packets(&self) -> Vec<(String, String)> {
            self.packets.lock().unwrap().clone()
        }

        fn forget_packets(&self) {
            self.packets.lock().unwrap().clear();
        }

        fn methods(&self) -> Vec<String> {
            self.packets()
                .into_iter()
                .map(|(method, _)| method)
                .collect()
        }

        fn raw_provider(&self) -> AlloyProvider {
            let client = ClientBuilder::default().transport(self.clone(), true);
            let root: RootProvider = RootProvider::new(client);
            AlloyProvider::from_provider(Arc::new(root))
        }

        /// A provider bound to chain 1 with the binding's own traffic forgotten,
        /// so every packet the test records belongs to the call under test.
        async fn provider(&self) -> AlloyProvider {
            let provider = self.raw_provider();
            provider
                .bind_to_chain(CHAIN)
                .await
                .expect("the stub serves chain 1");
            self.forget_packets();
            provider
        }
    }

    /// One request's answer, or the transport-level refusal the stub injects.
    fn answer(
        state: &StubState,
        req: &SerializedRequest,
    ) -> Result<ResponsePayload, TransportError> {
        let params: Value = req
            .params()
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or(Value::Null);
        let success = |value: Value| ResponsePayload::Success(to_raw_value(&value).unwrap());
        let payload = match req.method() {
            "eth_chainId" => success(json!(format!("0x{:x}", state.chain.unwrap_or(1)))),
            "eth_blockNumber" => success(json!("0x64")),
            "eth_getBlockByNumber" => {
                let number = params
                    .get(0)
                    .and_then(Value::as_str)
                    .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok());
                match number.and_then(|number| state.headers.get(&number)) {
                    Some(header) => success(header.clone()),
                    None => success(Value::Null),
                }
            }
            "eth_getLogs" => success(state.logs.clone()),
            "eth_call" | "eth_getCode" | "eth_getStorageAt" | "eth_getBalance" => {
                if state.reject_hash_pinned {
                    return Ok(ResponsePayload::Failure(ErrorPayload {
                        code: -32_602,
                        message: "invalid params: unsupported block identifier".into(),
                        data: None,
                    }));
                }
                if state.canonical_only {
                    if let Some(named) = pinned_hash(&params) {
                        let held = state.headers.values().any(|header| {
                            header.get("hash").and_then(Value::as_str) == Some(named.as_str())
                        });
                        if !held {
                            return Ok(ResponsePayload::Failure(ErrorPayload {
                                code: -32_000,
                                message: format!("block hash {named} not found").into(),
                                data: None,
                            }));
                        }
                    }
                }
                if let Some((code, message)) = state.failures.get(req.method()) {
                    return Ok(ResponsePayload::Failure(ErrorPayload {
                        code: *code,
                        message: message.clone().into(),
                        data: None,
                    }));
                }
                if req.method() == "eth_call" && calls_multicall3(&params) {
                    return Ok(match &state.batch {
                        Some(Ok(envelope)) => success(envelope.clone()),
                        Some(Err((code, message))) => ResponsePayload::Failure(ErrorPayload {
                            code: *code,
                            message: message.clone().into(),
                            data: None,
                        }),
                        None => success(Value::Null),
                    });
                }
                match state.reads.get(req.method()) {
                    Some(result) => success(result.clone()),
                    None => success(Value::Null),
                }
            }
            other => {
                return Err(TransportError::local_usage_str(other));
            }
        };
        Ok(payload)
    }

    use serde_json::value::to_raw_value;

    /// The block hash an EIP-1898 request names, when it names one.
    fn pinned_hash(params: &Value) -> Option<String> {
        params
            .as_array()?
            .iter()
            .find_map(|arg| arg.get("blockHash").and_then(Value::as_str))
            .map(str::to_string)
    }

    /// Whether this `eth_call` addresses Multicall3.
    fn calls_multicall3(params: &Value) -> bool {
        let multicall = format!("{MULTICALL3_ADDRESS}");
        params
            .as_array()
            .and_then(|argv| argv.first())
            .and_then(|call| call.get("to"))
            .and_then(Value::as_str)
            .is_some_and(|to| to.eq_ignore_ascii_case(&multicall))
    }

    /// One 32-byte word holding `value`.
    fn word(value: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[31] = value;
        out
    }

    /// An `aggregate3` return envelope: one `(bool,bytes)` row per call.
    fn aggregate3_return(rows: &[(bool, &[u8])]) -> Value {
        use alloy::dyn_abi::DynSolValue;
        let values: Vec<DynSolValue> = rows
            .iter()
            .map(|(success, data)| {
                DynSolValue::Tuple(vec![
                    DynSolValue::Bool(*success),
                    DynSolValue::Bytes(data.to_vec()),
                ])
            })
            .collect();
        json!(format!(
            "0x{}",
            alloy::hex::encode(DynSolValue::Array(values).abi_encode())
        ))
    }

    impl tower::Service<RequestPacket> for EvidenceStub {
        type Response = ResponsePacket;
        type Error = TransportError;
        type Future = TransportFut<'static>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: RequestPacket) -> Self::Future {
            let stub = self.clone();
            Box::pin(async move {
                let respond = |req: &SerializedRequest| -> Result<Response, TransportError> {
                    stub.packets.lock().unwrap().push((
                        req.method().to_string(),
                        req.params()
                            .map_or_else(String::new, |params| params.get().to_string()),
                    ));
                    let state = stub.state.lock().unwrap().clone();
                    Ok(Response {
                        id: req.id().clone(),
                        payload: answer(&state, req)?,
                    })
                };
                match req {
                    RequestPacket::Single(single) => Ok(ResponsePacket::Single(respond(&single)?)),
                    RequestPacket::Batch(batch) => {
                        let mut answers = Vec::with_capacity(batch.len());
                        for single in &batch {
                            answers.push(respond(single)?);
                        }
                        Ok(ResponsePacket::Batch(answers))
                    }
                }
            })
        }
    }

    fn chain_of(range: std::ops::RangeInclusive<u64>) -> HeaderChain {
        let anchors = range
            .map(|number| anchor(number, (number % 200) as u8, ((number - 1) % 200) as u8))
            .collect::<Vec<_>>();
        HeaderChain::from_anchors(anchors).expect("a consecutive fixture chain")
    }

    #[test]
    fn a_complete_consecutive_chain_names_its_whole_span() {
        let chain = chain_of(100..=102);
        assert_eq!(chain.len(), 3);
        assert!(!chain.is_empty());
        assert_eq!(chain.from_block(), 100);
        assert_eq!(chain.to_block(), 102);
        assert_eq!(chain.terminal(), anchor(102, 102, 101));
        assert_eq!(chain.anchor_at(101), Some(anchor(101, 101, 100)));
        assert_eq!(chain.anchor_at(103), None);
    }

    #[test]
    fn an_empty_header_set_names_no_range() {
        assert_eq!(
            HeaderChain::from_anchors(Vec::new()),
            Err(HeaderRefusal::EmptyRange)
        );
    }

    #[test]
    fn a_same_height_hash_substitution_is_refused() {
        let refusal =
            HeaderChain::from_anchors(vec![anchor(100, 0xa1, 0xa0), anchor(100, 0xb1, 0xa0)])
                .expect_err("two blocks at one height are not a chain");
        assert_eq!(
            refusal,
            HeaderRefusal::SameHeightSubstitution { number: 100 }
        );
    }

    #[test]
    fn a_cross_chain_parent_is_refused() {
        let other_chain = ChainAnchor::new(2, 101, hash(0xa2), hash(0xa1))
            .expect("the fixture anchor carries every identity field");
        let refusal = HeaderChain::from_anchors(vec![anchor(100, 0xa1, 0xa0), other_chain])
            .expect_err("a parent on another chain is no predecessor");
        assert_eq!(
            refusal,
            HeaderRefusal::CrossChainParent {
                number: 101,
                child: 2,
                parent: 1,
            }
        );
    }

    #[test]
    fn a_broken_parent_link_is_refused() {
        let refusal =
            HeaderChain::from_anchors(vec![anchor(100, 0xa1, 0xa0), anchor(101, 0xa2, 0xb0)])
                .expect_err("a child naming another parent is a different branch");
        assert_eq!(
            refusal,
            HeaderRefusal::BrokenParentLink {
                number: 101,
                expected: hash(0xa1),
                actual: hash(0xb0),
            }
        );
    }

    #[test]
    fn a_gap_in_the_chain_is_refused() {
        let refusal =
            HeaderChain::from_anchors(vec![anchor(100, 0xa1, 0xa0), anchor(102, 0xa3, 0xa1)])
                .expect_err("a range covers every height in it");
        assert_eq!(
            refusal,
            HeaderRefusal::NotConsecutive {
                head: 102,
                parent: 100,
            }
        );
    }

    #[tokio::test]
    async fn an_empty_log_range_still_carries_its_complete_header_chain() {
        let stub = EvidenceStub::new()
            .header(100, 0xa1, 0xa0)
            .header(101, 0xa2, 0xa1)
            .header(102, 0xa3, 0xa2);
        let provider = stub.provider().await;
        let fetcher = LogFetcher::new(Arc::new(provider.clone()), 1_000);
        let filter = LogFilter::new(100, 102, None, None).expect("a valid fixture filter");

        let unit = ProjectionRange::acquire(&provider, &fetcher, &filter)
            .await
            .expect("a range with no matching logs is still a complete unit");

        assert_eq!(unit.span(), 100..=102);
        assert_eq!(unit.headers().len(), 3, "every block of the range is named");
        assert_eq!(unit.headers().terminal(), anchor(102, 0xa3, 0xa2));
        assert!(unit.logs().is_empty());
        assert_eq!(
            stub.methods()
                .iter()
                .filter(|method| method.as_str() == "eth_getBlockByNumber")
                .count(),
            3,
            "one header read per block, including the blocks that emitted no log"
        );
    }

    #[tokio::test]
    async fn a_missing_terminal_header_refuses_the_range() {
        let stub = EvidenceStub::new()
            .header(100, 0xa1, 0xa0)
            .header(101, 0xa2, 0xa1);
        let provider = stub.provider().await;
        let fetcher = LogFetcher::new(Arc::new(provider.clone()), 1_000);
        let filter = LogFilter::new(100, 102, None, None).expect("a valid fixture filter");

        let refusal = ProjectionRange::acquire(&provider, &fetcher, &filter)
            .await
            .expect_err("an incomplete header chain cannot advance a cursor");

        assert!(
            matches!(
                refusal,
                ChainEvidenceError::Header(HeaderRefusal::MissingHeader { number: 102 })
            ),
            "got {refusal:?}"
        );
    }

    #[tokio::test]
    async fn a_log_bearing_range_agrees_with_its_headers() {
        let stub = EvidenceStub::new()
            .header(100, 0xa1, 0xa0)
            .header(101, 0xa2, 0xa1)
            .header(102, 0xa3, 0xa2)
            .logs(json!([log_value(101, hash(0xa2), 0)]));
        let provider = stub.provider().await;
        let fetcher = LogFetcher::new(Arc::new(provider.clone()), 1_000);
        let filter = LogFilter::new(100, 102, None, None).expect("a valid fixture filter");

        let unit = ProjectionRange::acquire(&provider, &fetcher, &filter)
            .await
            .expect("the log was emitted in a block the headers name");

        assert_eq!(unit.logs().len(), 1);
        assert_eq!(unit.logs()[0].block_number, Some(101));
        assert_eq!(unit.headers().anchor_at(101), Some(anchor(101, 0xa2, 0xa1)));
    }

    #[tokio::test]
    async fn a_header_branch_the_node_serves_broken_refuses_the_range() {
        let stub = EvidenceStub::new()
            .header(100, 0xa1, 0xa0)
            .header(101, 0xa2, 0xa1)
            .header(102, 0xa3, 0xb0);
        let provider = stub.provider().await;
        let fetcher = LogFetcher::new(Arc::new(provider.clone()), 1_000);
        let filter = LogFilter::new(100, 102, None, None).expect("a valid fixture filter");

        let refusal = ProjectionRange::acquire(&provider, &fetcher, &filter)
            .await
            .expect_err("a broken link is not an ordinary advance");

        assert!(
            matches!(
                refusal,
                ChainEvidenceError::Header(HeaderRefusal::BrokenParentLink { number: 102, .. })
            ),
            "got {refusal:?}"
        );
    }

    #[test]
    fn a_log_from_another_branch_refuses_the_unit() {
        let refusal = ProjectionRange::new(chain_of(100..=102), vec![log_at(101, hash(0xff), 0)])
            .expect_err("a log from another branch is not evidence of this range");
        assert_eq!(
            refusal,
            ProjectionRefusal::LogBranchMismatch {
                number: 101,
                header: hash(101),
                log: hash(0xff),
            }
        );
    }

    #[test]
    fn a_log_without_its_block_identity_refuses_the_unit() {
        let chain = chain_of(100..=102);
        assert_eq!(
            ProjectionRange::new(chain.clone(), vec![log_missing_block_hash(101, 0)])
                .expect_err("a hashless log names no branch"),
            ProjectionRefusal::LogWithoutBlockHash { number: 101 }
        );
        assert_eq!(
            ProjectionRange::new(chain, vec![log_missing_block_number(hash(100), 0)])
                .expect_err("a numberless log names no block"),
            ProjectionRefusal::LogWithoutBlockNumber
        );
    }

    #[test]
    fn unsorted_logs_are_ordered_by_block_then_index() {
        let unit = ProjectionRange::new(
            chain_of(100..=102),
            vec![
                log_at(102, hash(102), 1),
                log_at(100, hash(100), 0),
                log_at(100, hash(100), 3),
            ],
        )
        .expect("every log names a block the headers cover");

        assert_eq!(
            unit.logs()
                .iter()
                .map(|log| (log.block_number, log.log_index))
                .collect::<Vec<_>>(),
            vec![
                (Some(100), Some(0)),
                (Some(100), Some(3)),
                (Some(102), Some(1))
            ],
            "the unit states its canonical (block_number, log_index) order"
        );
    }

    #[test]
    fn a_log_without_its_index_refuses_the_unit() {
        assert_eq!(
            ProjectionRange::new(chain_of(100..=102), vec![log_missing_index(101, hash(101))])
                .expect_err("a log with no index has no canonical position"),
            ProjectionRefusal::LogWithoutIndex { number: 101 }
        );
    }

    #[test]
    fn a_log_outside_the_range_refuses_the_unit() {
        let refusal = ProjectionRange::new(chain_of(100..=102), vec![log_at(103, hash(103), 0)])
            .expect_err("a log outside the span is not covered by its headers");
        assert_eq!(
            refusal,
            ProjectionRefusal::LogOutsideRange {
                number: 103,
                from: 100,
                to: 102,
            }
        );
    }

    #[tokio::test]
    async fn an_execution_revert_naming_unsupported_is_a_failed_read() {
        let stub = EvidenceStub::new().failing(
            "eth_call",
            -32_000,
            "execution reverted: unsupported collateral asset",
        );
        let provider = stub.provider().await;

        let error = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0))
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect_err("a revert is a failed read, never a capability verdict");

        assert!(
            matches!(error, ExactReadError::Failed { .. }),
            "a revert whose message happens to say \"unsupported\" is still a revert: {error:?}"
        );
    }

    #[tokio::test]
    async fn a_generic_node_fault_naming_unsupported_is_a_failed_read() {
        let stub =
            EvidenceStub::new().failing("eth_call", -32_000, "unsupported asset for the market");
        let provider = stub.provider().await;

        let error = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0))
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect_err("a generic fault is a failed read, not a capability verdict");

        assert!(
            matches!(error, ExactReadError::Failed { .. }),
            "a fault that names \"unsupported\" without rejecting the request shape is not \
             a capability verdict: {error:?}"
        );
    }

    #[tokio::test]
    async fn a_shape_marker_without_a_params_code_is_an_unsupported_capability() {
        let stub = EvidenceStub::new().failing(
            "eth_call",
            -32_000,
            "invalid block identifier: requireCanonical is not accepted",
        );
        let provider = stub.provider().await;

        let error = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0))
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect_err("a node that rejects the EIP-1898 identifier shape has no capability");

        assert!(
            matches!(
                error,
                ExactReadError::Unsupported {
                    method: "eth_call",
                    ..
                }
            ),
            "a shape rejection still classifies as a capability verdict: {error:?}"
        );
    }

    #[tokio::test]
    async fn an_echoed_request_shape_in_a_protocol_error_is_a_failed_read() {
        let stub = EvidenceStub::new().failing(
            "eth_call",
            -32_603,
            "internal error: no recorded entry for {\"requireCanonical\":true}",
        );
        let provider = stub.provider().await;

        let error = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0))
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect_err("an internal error states no capability verdict");

        assert!(
            matches!(error, ExactReadError::Failed { .. }),
            "a protocol-band error that merely echoes the request shape is not a capability \
             verdict: {error:?}"
        );
    }

    #[tokio::test]
    async fn every_snapshot_read_is_pinned_to_its_own_block_hash() {
        let stub = EvidenceStub::new()
            .read("eth_call", json!("0x1234"))
            .read("eth_getCode", json!("0x6080"))
            .read(
                "eth_getStorageAt",
                json!("0x000000000000000000000000000000000000000000000000000000000000000c"),
            )
            .read("eth_getBalance", json!("0x2a"));
        let provider = stub.provider().await;
        let pinned = anchor(100, 0xa1, 0xa0);
        let reads = BlockReads::at(&provider, pinned);

        let call = reads
            .call(Address::repeat_byte(0x11), Bytes::from(vec![0xde, 0xad]))
            .await
            .expect("the stub answers the call");
        assert_eq!(call, Bytes::from(vec![0x12, 0x34]));
        assert_eq!(
            reads
                .code_at(Address::repeat_byte(0x11))
                .await
                .expect("the stub answers the code read"),
            Bytes::from(vec![0x60, 0x80])
        );
        assert_eq!(
            reads
                .storage_at(Address::repeat_byte(0x11), U256::ZERO)
                .await
                .expect("the stub answers the storage read"),
            U256::from(12u64)
        );
        assert_eq!(
            reads
                .balance_at(Address::repeat_byte(0x11))
                .await
                .expect("the stub answers the balance read"),
            U256::from(42u64)
        );

        let packets = stub.packets();
        assert_eq!(packets.len(), 4, "one request per read, no fallback read");
        for (method, params) in &packets {
            let parsed: Value = serde_json::from_str(params).expect("the stub records JSON params");
            let pinned_block = parsed
                .as_array()
                .and_then(|params| params.last())
                .expect("every read names its block last");
            assert_eq!(
                pinned_block.get("blockHash").and_then(Value::as_str),
                Some(format!("{}", pinned.hash()).as_str()),
                "{method} must pin the snapshot's hash"
            );
            assert_eq!(
                pinned_block
                    .get("requireCanonical")
                    .and_then(Value::as_bool),
                Some(true),
                "{method} must require the pinned block to still be canonical"
            );
            assert!(
                pinned_block.as_str().is_none(),
                "{method} must never name its block by number"
            );
        }
    }

    /// Every account, storage, and code read states the identifier in the
    /// recorded corpus's own form — the lowercase address, the slot as a
    /// minimal quantity, then the `{blockHash, requireCanonical}` pin — because
    /// the transport states it once for every read family.
    #[tokio::test]
    async fn the_snapshot_reads_state_the_recorded_wire_params() {
        let stub = EvidenceStub::new()
            .read("eth_getCode", json!("0x6080"))
            .read(
                "eth_getStorageAt",
                json!("0x000000000000000000000000000000000000000000000000000000000000000c"),
            )
            .read("eth_getBalance", json!("0x2a"));
        let provider = stub.provider().await;
        let pinned = anchor(100, 0xa1, 0xa0);
        let reads = BlockReads::at(&provider, pinned);
        let address = Address::repeat_byte(0x11);

        reads
            .code_at(address)
            .await
            .expect("the stub serves the code read");
        reads
            .storage_at(address, U256::ZERO)
            .await
            .expect("the stub serves the storage read");
        reads
            .balance_at(address)
            .await
            .expect("the stub serves the balance read");

        let pin = json!({ "blockHash": format!("{}", pinned.hash()), "requireCanonical": true });
        let wire = format!("0x{}", alloy::hex::encode(address.as_slice()));
        let packets: Vec<(String, Value)> = stub
            .packets()
            .into_iter()
            .map(|(method, params)| {
                (
                    method,
                    serde_json::from_str(&params).expect("the stub records JSON params"),
                )
            })
            .collect();
        assert_eq!(
            packets,
            vec![
                (
                    "eth_getCode".to_string(),
                    json!([wire.clone(), pin.clone()])
                ),
                (
                    "eth_getStorageAt".to_string(),
                    json!([wire.clone(), "0x0", pin.clone()])
                ),
                ("eth_getBalance".to_string(), json!([wire, pin])),
            ],
            "the reads share the one request construction, pinned by hash"
        );
    }

    /// A node that rejects the EIP-1898 shape has no account, storage, or code
    /// read capability either: the same unsupported-shape classification, named
    /// per method, and never a number-only fallback.
    #[tokio::test]
    async fn every_snapshot_read_refuses_a_node_without_the_hash_pinned_shape() {
        let stub = EvidenceStub::new().rejecting_hash_pinned();
        let provider = stub.provider().await;
        let reads = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0));
        let address = Address::repeat_byte(0x11);

        let code = reads
            .code_at(address)
            .await
            .expect_err("a node without hash-pinned reads cannot serve the code read");
        assert!(
            matches!(
                code,
                ExactReadError::Unsupported {
                    method: "eth_getCode",
                    ..
                }
            ),
            "got {code:?}"
        );
        let storage = reads
            .storage_at(address, U256::ZERO)
            .await
            .expect_err("a node without hash-pinned reads cannot serve the storage read");
        assert!(
            matches!(
                storage,
                ExactReadError::Unsupported {
                    method: "eth_getStorageAt",
                    ..
                }
            ),
            "got {storage:?}"
        );
        let balance = reads
            .balance_at(address)
            .await
            .expect_err("a node without hash-pinned reads cannot serve the balance read");
        assert!(
            matches!(
                balance,
                ExactReadError::Unsupported {
                    method: "eth_getBalance",
                    ..
                }
            ),
            "got {balance:?}"
        );
        assert_eq!(
            stub.methods(),
            vec![
                "eth_getCode".to_string(),
                "eth_getStorageAt".to_string(),
                "eth_getBalance".to_string()
            ],
            "an unsupported capability is never retried as a number-only read"
        );
    }

    #[tokio::test]
    async fn a_node_without_hash_pinned_reads_is_a_typed_evidence_failure() {
        let stub = EvidenceStub::new().rejecting_hash_pinned();
        let provider = stub.provider().await;
        let reads = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0));

        let error = reads
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect_err("a node without the capability cannot serve an exact read");

        assert!(
            matches!(
                error,
                ExactReadError::Unsupported {
                    method: "eth_call",
                    ..
                }
            ),
            "got {error:?}"
        );
        assert_eq!(
            stub.methods(),
            vec!["eth_call".to_string()],
            "an unsupported capability is never retried as a number-only read"
        );
    }

    #[test]
    fn a_header_chain_places_only_logs_it_emitted() {
        let chain = chain_of(100..=102);
        let placed = chain
            .place_logs(vec![log_at(102, hash(102), 1), log_at(100, hash(100), 0)])
            .expect("both logs name blocks the chain covers");
        assert_eq!(
            placed
                .iter()
                .map(|log| log.block_number)
                .collect::<Vec<_>>(),
            vec![Some(100), Some(102)],
            "placement states the canonical order"
        );
        assert_eq!(
            chain
                .place_logs(vec![log_at(103, hash(103), 0)])
                .expect_err("a log outside the chain is not placed by it"),
            ProjectionRefusal::LogOutsideRange {
                number: 103,
                from: 100,
                to: 102,
            }
        );
    }

    #[tokio::test]
    async fn a_tip_run_resolves_its_terminal_block_to_an_exact_anchor() {
        // eth_blockNumber serves 0x64 = 100, and the node holds that header.
        let stub = EvidenceStub::new().header(100, 0xa1, 0xa0);
        let provider = stub.provider().await;

        let terminal = resolve_terminal_anchor(&provider, CHAIN, None)
            .await
            .expect("the tip resolves to the header the node serves");

        assert_eq!(terminal, anchor(100, 0xa1, 0xa0));
        assert_eq!(
            stub.methods(),
            vec![
                "eth_blockNumber".to_string(),
                "eth_getBlockByNumber".to_string()
            ],
            "the tip arms one number read then one header read, never a bare number"
        );
    }

    #[tokio::test]
    async fn an_explicit_terminal_block_resolves_through_the_same_path() {
        let stub = EvidenceStub::new().header(100, 0xa1, 0xa0);
        let provider = stub.provider().await;

        let terminal = resolve_terminal_anchor(&provider, CHAIN, Some(100))
            .await
            .expect("an explicit bound resolves to its header");

        assert_eq!(terminal, anchor(100, 0xa1, 0xa0));
        assert_eq!(
            stub.methods(),
            vec!["eth_getBlockByNumber".to_string()],
            "a bound run arms the header read only, and never asks the tip"
        );
    }

    #[tokio::test]
    async fn a_missing_terminal_header_refuses_the_terminal_anchor() {
        let stub = EvidenceStub::new();
        let provider = stub.provider().await;

        let refusal = resolve_terminal_anchor(&provider, CHAIN, Some(102))
            .await
            .expect_err("a bound with no complete header has no exact terminal");

        assert!(
            matches!(
                refusal,
                ChainEvidenceError::Header(HeaderRefusal::MissingHeader { number: 102 })
            ),
            "got {refusal:?}"
        );
    }

    #[tokio::test]
    async fn a_terminal_header_on_another_chain_refuses_the_terminal_anchor() {
        let stub = EvidenceStub::new().chain(2).header(101, 0xa2, 0xa1);
        let provider = stub.raw_provider();

        let refusal = resolve_terminal_anchor(&provider, CHAIN, Some(101))
            .await
            .expect_err("a terminal block on another chain is not this run's terminal");

        assert!(
            matches!(
                refusal,
                ChainEvidenceError::CrossChainTerminal {
                    number: 101,
                    expected: CHAIN,
                    served: 2,
                }
            ),
            "got {refusal:?}"
        );
    }

    // ── the hash-pinned batch (Multicall3 over one exact anchor) ─────────

    /// Two sub-calls ride ONE `aggregate3` `eth_call`, and the request names the
    /// snapshot's hash: a batch is one read position, exactly like a single call.
    #[tokio::test]
    async fn a_batch_pins_every_sub_call_to_the_snapshot_hash_in_one_request() {
        let stub =
            EvidenceStub::new().batch(aggregate3_return(&[(true, &word(42)), (true, &word(7))]));
        let provider = stub.provider().await;
        let pinned = anchor(100, 0xa1, 0xa0);
        let reads = BlockReads::at(&provider, pinned);
        let target = Address::repeat_byte(0x11);

        let outcomes = reads
            .batch(&[
                (target, Bytes::from(vec![0xaa])),
                (target, Bytes::from(vec![0xbb])),
            ])
            .await
            .expect("the stub answers the pinned batch");

        assert_eq!(
            outcomes,
            vec![
                MulticallOutcome::Returned(Bytes::from(word(42).to_vec())),
                MulticallOutcome::Returned(Bytes::from(word(7).to_vec())),
            ]
        );
        let packets = stub.packets();
        assert_eq!(
            packets.len(),
            1,
            "one aggregate3 round trip for the whole batch"
        );
        let params: Value = serde_json::from_str(&packets[0].1).expect("the stub records JSON");
        let call = &params[0];
        assert!(
            call.get("to")
                .and_then(Value::as_str)
                .is_some_and(|to| to.eq_ignore_ascii_case(&format!("{MULTICALL3_ADDRESS}"))),
            "the batch is one call to Multicall3: {call}"
        );
        let calldata = call
            .get("data")
            .and_then(Value::as_str)
            .expect("the batch names its calldata");
        assert!(
            calldata.starts_with("0x134dd343"),
            "the batch carries the aggregate3 selector: {calldata}"
        );
        let pin = &params[1];
        assert_eq!(
            pin.get("blockHash").and_then(Value::as_str),
            Some(format!("{}", pinned.hash()).as_str()),
            "the batch pins the snapshot's own hash"
        );
        assert_eq!(
            pin.get("requireCanonical").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            pin.as_str().is_none(),
            "a batch never names its block by number"
        );
    }

    /// A one-call batch is still ONE `aggregate3` envelope — the shape the
    /// number-pinned batch has always had, so the verification fan-out keeps its
    /// exact request set and only its pin changes.
    #[tokio::test]
    async fn a_one_call_batch_is_one_pinned_aggregate3_envelope() {
        let stub = EvidenceStub::new().batch(aggregate3_return(&[(true, &word(12))]));
        let provider = stub.provider().await;
        let pinned = anchor(100, 0xa1, 0xa0);
        let target = Address::repeat_byte(0x22);

        let outcomes = BlockReads::at(&provider, pinned)
            .batch(&[(target, Bytes::from(vec![0xcc]))])
            .await
            .expect("the stub answers the single-call batch");

        assert_eq!(
            outcomes,
            vec![MulticallOutcome::Returned(Bytes::from(word(12).to_vec()))]
        );
        let packets = stub.packets();
        assert_eq!(packets.len(), 1);
        let params: Value = serde_json::from_str(&packets[0].1).expect("the stub records JSON");
        assert!(
            params[0]
                .get("to")
                .and_then(Value::as_str)
                .is_some_and(|to| to.eq_ignore_ascii_case(&format!("{MULTICALL3_ADDRESS}"))),
            "a one-call batch still addresses Multicall3: {}",
            params[0]
        );
        assert_eq!(
            params[1].get("blockHash").and_then(Value::as_str),
            Some(format!("{}", pinned.hash()).as_str())
        );
        // An empty batch issues nothing at all.
        stub.forget_packets();
        let empty = BlockReads::at(&provider, pinned)
            .batch(&[])
            .await
            .expect("an empty batch is no request");
        assert!(empty.is_empty());
        assert!(stub.packets().is_empty());
    }

    /// A node without the hash-pinned shape refuses the batch's OWN request: the
    /// capability is absent, so no sequential number-only sub-call is issued.
    #[tokio::test]
    async fn a_batch_without_the_capability_never_degrades_to_number_only_reads() {
        let stub = EvidenceStub::new().rejecting_hash_pinned();
        let provider = stub.provider().await;
        let target = Address::repeat_byte(0x11);

        let error = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0))
            .batch(&[
                (target, Bytes::from(vec![0xaa])),
                (target, Bytes::from(vec![0xbb])),
            ])
            .await
            .expect_err("a node without the capability cannot serve an exact batch");

        assert!(
            matches!(
                error,
                ExactReadError::Unsupported {
                    method: "eth_call",
                    ..
                }
            ),
            "got {error:?}"
        );
        assert_eq!(
            stub.methods(),
            vec!["eth_call".to_string()],
            "the refusal is the batch's own request — no per-call retry exists"
        );
    }

    /// A reverted sub-call is an outcome of the pinned batch, exactly as it is of
    /// the number-pinned one: the caller decides what a revert means.
    #[tokio::test]
    async fn a_batch_classifies_a_reverted_sub_call_as_an_outcome() {
        let stub = EvidenceStub::new().batch(aggregate3_return(&[(true, &word(9)), (false, &[])]));
        let provider = stub.provider().await;
        let target = Address::repeat_byte(0x11);

        let outcomes = BlockReads::at(&provider, anchor(100, 0xa1, 0xa0))
            .batch(&[
                (target, Bytes::from(vec![0xaa])),
                (target, Bytes::from(vec![0xbb])),
            ])
            .await
            .expect("a reverted sub-call does not fail the batch");

        assert_eq!(
            outcomes,
            vec![
                MulticallOutcome::Returned(Bytes::from(word(9).to_vec())),
                MulticallOutcome::Reverted(Bytes::new()),
            ]
        );
    }

    // ── the parent read position ──────────────────────────────────────────

    /// The parent of an anchored block is named by ONE header read, and the
    /// header must be the block the child claims as its parent.
    #[tokio::test]
    async fn a_parent_anchor_names_the_parent_block_exactly() {
        let stub = EvidenceStub::new()
            .header(99, 0xa0, 0x9f)
            .header(100, 0xa1, 0xa0);
        let provider = stub.provider().await;

        let parent = resolve_parent_anchor(&provider, anchor(100, 0xa1, 0xa0))
            .await
            .expect("the stub states the parent header");

        assert_eq!(parent, anchor(99, 0xa0, 0x9f));
        assert_eq!(
            stub.methods(),
            vec!["eth_getBlockByNumber".to_string()],
            "one header read names the parent position"
        );
    }

    /// A parent height whose block is not the child's stated parent is a
    /// same-height substitution: refused, never re-read as the parent.
    #[tokio::test]
    async fn a_parent_anchor_refuses_a_same_height_substitution() {
        let stub = EvidenceStub::new()
            .header(99, 0xb0, 0x9f)
            .header(100, 0xa1, 0xa0);
        let provider = stub.provider().await;

        let refusal = resolve_parent_anchor(&provider, anchor(100, 0xa1, 0xa0))
            .await
            .expect_err("the served parent is not the child's parent");

        assert!(
            matches!(
                refusal,
                ParentAnchorError::BranchMismatch {
                    number: 99,
                    expected,
                    served,
                } if expected == hash(0xa0) && served == hash(0xb0)
            ),
            "got {refusal:?}"
        );
    }

    /// A genesis anchor has no parent read position, and no read is issued to
    /// find that out.
    #[tokio::test]
    async fn a_genesis_anchor_has_no_parent_position() {
        let stub = EvidenceStub::new();
        let provider = stub.provider().await;

        let refusal = resolve_parent_anchor(&provider, anchor(0, 0xa1, 0xa0))
            .await
            .expect_err("genesis has no parent");

        assert!(
            matches!(refusal, ParentAnchorError::Genesis),
            "got {refusal:?}"
        );
        assert!(
            stub.methods().is_empty(),
            "no read is issued for a genesis anchor"
        );
    }

    /// A missing parent header cannot name the parent position.
    #[tokio::test]
    async fn a_missing_parent_header_refuses_the_parent_anchor() {
        let stub = EvidenceStub::new().header(100, 0xa1, 0xa0);
        let provider = stub.provider().await;

        let refusal = resolve_parent_anchor(&provider, anchor(100, 0xa1, 0xa0))
            .await
            .expect_err("the node holds no parent header");

        assert!(
            matches!(refusal, ParentAnchorError::MissingHeader { number: 99 }),
            "got {refusal:?}"
        );
    }

    /// A same-height reorg abandons the hash an anchored read names: the node
    /// refuses the pin, so the abandoned branch can never serve the read.
    #[tokio::test]
    async fn a_same_height_branch_cannot_serve_a_pinned_read() {
        let stub = EvidenceStub::new()
            .canonical_only()
            .header(100, 0xb1, 0xa0)
            .read("eth_call", json!("0x1234"));
        let provider = stub.provider().await;

        let abandoned = anchor(100, 0xa1, 0xa0);
        let error = BlockReads::at(&provider, abandoned)
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect_err("the abandoned hash is no longer canonical");
        assert!(
            matches!(error, ExactReadError::Failed { .. }),
            "an abandoned pin is a failed read, never a served one: {error:?}"
        );

        let served = BlockReads::at(&provider, anchor(100, 0xb1, 0xa0))
            .call(Address::repeat_byte(0x11), Bytes::new())
            .await
            .expect("the canonical branch still answers");
        assert_eq!(served, Bytes::from(vec![0x12, 0x34]));
    }
}
