//! The canonical-head fact, the exact block identity behind it, and the one
//! sink that carries the head.
//!
//! A head is a *chain* fact, not an engine epoch: it is the identity a caller
//! fences a read against (block number AND hash, plus the parent hash its
//! lineage is verified by). The pump learns it for free — the alloy header it
//! already holds carries all three — so this type exists to move that identity
//! across crate boundaries without a second header read and without either side
//! learning the other's storage.
//!
//! [`ChainAnchor`] is that identity stated in full (ADR-105 D1): the chain, the
//! height, the block's own hash, and the parent hash. It is the one vocabulary
//! every exact-anchor consumer converges on — a canonical head names the tip,
//! an RPC lineage names a read's block, a simulation read anchor fences a
//! fan-out — and it is deliberately dependency-neutral: no provider, no
//! storage, no runtime. Transaction, log, and trace-call positions
//! ([`TxRef`], [`LogRef`], [`CallRef`]) qualify themselves by the same anchor,
//! so a cache or a work item that keys on any of them can never confuse two
//! blocks at one height. A [`LogRef`] carries the receipt's BLOCK-WIDE
//! `logIndex`; a [`CallRef`] carries the transaction plus the trace's
//! `traceAddress`, whose empty form is the valid root call — a call is named
//! independently of the logs it emits. Header metadata (timestamp, gas, fees)
//! stays outside: it describes the block's execution, not its identity.
//!
//! [`CanonicalHeadSink`] is deliberately the smallest possible surface: one
//! method, no return value, no async. A consumer that must do real work on a
//! head owns the spawning; the producer (the block pump's header arm) must never
//! await storage work on the block clock.

use alloy::primitives::B256;

/// One exact block identity: the chain, the height, the block's own canonical
/// hash, and the parent hash its lineage is verified by.
///
/// The fields are private on purpose: every [`ChainAnchor`] in the system was
/// built through [`Self::new`], so a zero chain id, a zero block hash, or a
/// zero parent hash can never masquerade as a resolved identity — missing
/// facts are a construction refusal, never a default (ADR-105 D1). The zero
/// hash marks "no block was named"; it is a real value only for a genesis
/// parent, and this engine never fences a read at genesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChainAnchor {
    /// The chain the block belongs to (EIP-155 id). One anchor is one chain:
    /// identical (number, hash) pairs on different chains are different
    /// identities.
    chain_id: u64,
    /// The block's height.
    number: u64,
    /// The block's own canonical hash — half of the exact anchor a read
    /// fences against.
    hash: B256,
    /// The hash of the block this one builds on, which is what makes a
    /// lineage link verifiable rather than merely consecutive.
    parent_hash: B256,
}

/// Why a [`ChainAnchor`] could not be constructed: a required identity field
/// was missing (its zero value), never a guessed or defaulted one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChainAnchorError {
    /// `chain_id` 0 — no chain was named.
    #[error("missing chain id: chain 0 is not an identity")]
    MissingChainId,
    /// The zero block hash — no block was named.
    #[error("missing block hash")]
    MissingBlockHash,
    /// The zero parent hash — the block's lineage is unnamed.
    #[error("missing parent hash")]
    MissingParentHash,
}

impl ChainAnchor {
    /// The anchor of the block at `number` on `chain_id`, identified by `hash`
    /// and linked to `parent_hash`.
    ///
    /// # Errors
    ///
    /// [`ChainAnchorError::MissingChainId`] when `chain_id` is 0,
    /// [`ChainAnchorError::MissingBlockHash`] when `hash` is zero, and
    /// [`ChainAnchorError::MissingParentHash`] when `parent_hash` is zero —
    /// an identity with a missing member is refused, never zero-filled.
    pub fn new(
        chain_id: u64,
        number: u64,
        hash: B256,
        parent_hash: B256,
    ) -> Result<Self, ChainAnchorError> {
        if chain_id == 0 {
            return Err(ChainAnchorError::MissingChainId);
        }
        if hash.is_zero() {
            return Err(ChainAnchorError::MissingBlockHash);
        }
        if parent_hash.is_zero() {
            return Err(ChainAnchorError::MissingParentHash);
        }
        Ok(Self {
            chain_id,
            number,
            hash,
            parent_hash,
        })
    }

    /// The chain this block belongs to.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The block's height.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// The block's own canonical hash.
    #[must_use]
    pub const fn hash(&self) -> B256 {
        self.hash
    }

    /// The hash of the block this one builds on.
    #[must_use]
    pub const fn parent_hash(&self) -> B256 {
        self.parent_hash
    }

    /// Whether `self` is the ordinary forward advance of `parent`: the next
    /// height ON THE SAME CHAIN, building on `parent`'s own hash.
    ///
    /// # Errors
    ///
    /// [`AnchorLinkError::ChainMismatch`] first: a parent on another chain
    /// names no predecessor of this block, whatever the heights and hashes say
    /// (two chains can share a height and even a hash is chain-local evidence).
    /// [`AnchorLinkError::ParentHashMismatch`] when `self` names the next
    /// height but builds on some other block — the same-height reorg shape,
    /// where a caller must NOT treat the move as an ordinary advance.
    /// [`AnchorLinkError::NotConsecutive`] when the heights do not even name a
    /// single step (a gap, a repeat, or a rewind).
    pub fn links_from(&self, parent: &ChainAnchor) -> Result<(), AnchorLinkError> {
        if self.chain_id != parent.chain_id {
            return Err(AnchorLinkError::ChainMismatch {
                head: self.chain_id,
                parent: parent.chain_id,
            });
        }
        // `checked_add`: a parent at the top of the height range has no next
        // height, so it answers `NotConsecutive` rather than wrapping to 0 and
        // accepting a genesis-height head as its successor.
        if parent.number.checked_add(1) != Some(self.number) {
            return Err(AnchorLinkError::NotConsecutive {
                head: self.number,
                parent: parent.number,
            });
        }
        if self.parent_hash != parent.hash {
            return Err(AnchorLinkError::ParentHashMismatch {
                expected: parent.hash,
                actual: self.parent_hash,
            });
        }
        Ok(())
    }
}

/// Why [`ChainAnchor::links_from`] refused the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnchorLinkError {
    /// The two anchors name different chains. Checked FIRST: a parent on
    /// another chain is no predecessor, so no height or hash claim across
    /// chains is ever evaluated.
    #[error("head on chain {head} cannot link from a parent on chain {parent}")]
    ChainMismatch {
        /// The chain the head claims.
        head: u64,
        /// The chain the parent claims.
        parent: u64,
    },
    /// The head's height is not `parent`'s next one: a gap, a repeat, or a
    /// rewind. No parent-hash claim is implied.
    #[error("head {head} is not the next height after parent {parent}")]
    NotConsecutive {
        /// The head's height.
        head: u64,
        /// The parent's height.
        parent: u64,
    },
    /// The head names the next height but builds on a different block than
    /// the parent's own hash — the same-height branch shape a caller must
    /// distinguish from an ordinary forward advance (ADR-105 D9).
    #[error("head's parent hash {actual} does not match the parent's own hash {expected}")]
    ParentHashMismatch {
        /// The hash the parent block actually has.
        expected: B256,
        /// The parent hash the head claims.
        actual: B256,
    },
}

/// One transaction's exact position: the [`ChainAnchor`] it executed in, its
/// own hash, and its index in the block.
///
/// Execution metadata (status, gas, receipt fields) stays outside — a `TxRef`
/// is identity, not outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxRef {
    anchor: ChainAnchor,
    hash: B256,
    index: u64,
}

/// Why a [`TxRef`] could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TxRefError {
    /// The zero transaction hash — no transaction was named.
    #[error("missing transaction hash")]
    MissingHash,
}

impl TxRef {
    /// The transaction at `index` of the block `anchor` names, identified by
    /// `hash`.
    ///
    /// # Errors
    ///
    /// [`TxRefError::MissingHash`] when `hash` is zero — a transaction
    /// reference without its own identity is refused, never zero-filled.
    pub fn new(anchor: ChainAnchor, hash: B256, index: u64) -> Result<Self, TxRefError> {
        if hash.is_zero() {
            return Err(TxRefError::MissingHash);
        }
        Ok(Self {
            anchor,
            hash,
            index,
        })
    }

    /// The anchor the transaction executed in.
    #[must_use]
    pub const fn anchor(&self) -> &ChainAnchor {
        &self.anchor
    }

    /// The transaction's own hash.
    #[must_use]
    pub const fn hash(&self) -> B256 {
        self.hash
    }

    /// The transaction's index in the block.
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }
}

/// One log's exact position: the [`TxRef`] that emitted it and the log's
/// BLOCK-WIDE `logIndex` — the JSON-RPC receipt's `logIndex`, which counts
/// every log in the block in execution order, NOT the log's position within
/// its own transaction.
///
/// The index is block-wide because that is the identity the chain and its
/// receipts state: two logs of one transaction and two logs of different
/// transactions all share one ordering, so `(anchor, index)` names one log and
/// `(tx, index)` names one log of that transaction. Re-indexing within the
/// transaction — what a receipt-relative enumeration would produce — names a
/// DIFFERENT log and is not this type.
///
/// There is no fallible arm: a block-wide log index is a total `u64` position,
/// and the identity a log cannot exist without — its transaction — is a full
/// [`TxRef`] the compiler will not let a caller omit. A source that lacks a
/// log index or a transaction hash has no [`LogRef`] to build at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogRef {
    tx: TxRef,
    index: u64,
}

impl LogRef {
    /// The log at `index` — its BLOCK-WIDE JSON-RPC `logIndex` — of the
    /// transaction `tx` names.
    #[must_use]
    pub const fn new(tx: TxRef, index: u64) -> Self {
        Self { tx, index }
    }

    /// The transaction the log was emitted by.
    #[must_use]
    pub const fn tx(&self) -> &TxRef {
        &self.tx
    }

    /// The log's BLOCK-WIDE JSON-RPC `logIndex` — never an index within the
    /// transaction.
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }
}

/// One trace-backed call's position: the [`TxRef`] whose execution tree holds
/// it and the call path that reaches it from the transaction's root call.
///
/// A call is identified INDEPENDENTLY of any log (ADR-105 D2, D5): ownership
/// evidence names a call, and a call's own logs are found from it, never the
/// reverse. The call path is the trace's `traceAddress` — the child indices
/// from the transaction's root call down to this one — and an EMPTY path is the
/// valid ROOT call, which executes the transaction's top-level frame and can
/// emit logs of its own. Call paths therefore exist only for trace-backed
/// evidence, and construction is total: every transaction has a root call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallRef {
    tx: TxRef,
    call_path: Vec<u64>,
}

impl CallRef {
    /// The call at `call_path` inside `tx`'s execution tree. An empty path is
    /// the transaction's ROOT call.
    #[must_use]
    pub fn new(tx: TxRef, call_path: Vec<u64>) -> Self {
        Self { tx, call_path }
    }

    /// The transaction whose execution tree holds this call.
    #[must_use]
    pub const fn tx(&self) -> &TxRef {
        &self.tx
    }

    /// The call path from the transaction's root call to this call; empty names
    /// the root call itself.
    #[must_use]
    pub fn call_path(&self) -> &[u64] {
        &self.call_path
    }

    /// Whether this call IS the transaction's root call.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.call_path.is_empty()
    }
}

/// What a session installs to be told about every accepted canonical head.
///
/// A sink is a *consumer* of the chain fact, never a producer of it: it is
/// called synchronously from the header arm and must therefore not block on
/// provider work. Uninstalled is a hard no-op at the producer (there is no
/// placeholder implementation to forget), which is what makes an opted-out
/// session structurally request-free rather than branch-free.
pub trait CanonicalHeadSink: Send + Sync + 'static {
    /// One accepted canonical head arrived: the [`ChainAnchor`] of the accepted
    /// tip, never a second block type.
    fn on_canonical_head(&self, head: ChainAnchor);
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};

    const CHAIN: u64 = 1;

    fn anchor(number: u64, tag: u8) -> ChainAnchor {
        ChainAnchor::new(
            CHAIN,
            number,
            B256::new([tag; 32]),
            B256::new([tag - 1; 32]),
        )
        .expect("the fixture anchor carries every identity field")
    }

    fn hash_of(value: &ChainAnchor) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        Hash::hash(value, &mut hasher);
        hasher.finish()
    }

    /// The reorg fence at identity level: two blocks at one height with
    /// different hashes are two different anchors — unequal, differently
    /// hashed, and never one exact-anchor cache entry (ADR-105 D1, D6).
    #[test]
    fn same_height_blocks_with_different_hashes_are_distinct_identities() {
        let abandoned = anchor(100, 0xaa);
        let replacement = anchor(100, 0xbb);

        assert_ne!(
            abandoned, replacement,
            "a same-height replacement is a different identity, not a moved height"
        );
        assert_ne!(
            hash_of(&abandoned),
            hash_of(&replacement),
            "an exact-anchor-keyed cache cannot collapse the two into one entry"
        );

        let mut cache: HashMap<ChainAnchor, &'static str> = HashMap::new();
        cache.insert(abandoned, "abandoned");
        assert_eq!(
            cache.get(&replacement),
            None,
            "the replacement cannot read \
            the abandoned block's entry: same height, different identity"
        );
    }

    /// Missing identity is a construction refusal, never a zero default: a
    /// chain id, a block hash, a parent hash, and a transaction hash each
    /// refuses on its own (ADR-105 D1). A call path is not identity that can be
    /// missing — the empty `traceAddress` IS the valid root call.
    #[test]
    fn missing_identity_fails_construction_instead_of_defaulting_to_zero() {
        let hash = B256::new([0xaa; 32]);
        let parent = B256::new([0x99; 32]);

        assert_eq!(
            ChainAnchor::new(0, 100, hash, parent),
            Err(ChainAnchorError::MissingChainId),
            "chain 0 is not an identity"
        );
        assert_eq!(
            ChainAnchor::new(CHAIN, 100, B256::ZERO, parent),
            Err(ChainAnchorError::MissingBlockHash),
            "a block without its own hash has no identity"
        );
        assert_eq!(
            ChainAnchor::new(CHAIN, 100, hash, B256::ZERO),
            Err(ChainAnchorError::MissingParentHash),
            "a block without its parent hash has an unprovable lineage"
        );

        let anchor = anchor(100, 0xaa);
        assert_eq!(
            TxRef::new(anchor, B256::ZERO, 0),
            Err(TxRefError::MissingHash),
            "a transaction reference without its own hash is not a position"
        );
    }

    /// The root call of a transaction is a REAL call reference: an empty
    /// `traceAddress` is the top-level frame, and it can emit logs of its own, so
    /// construction is total and no receipt may reject a root-emitted log
    /// (ADR-105 D2).
    #[test]
    fn the_root_call_is_a_valid_call_reference() {
        let anchor = anchor(100, 0xaa);
        let tx = TxRef::new(anchor, B256::new([0xcc; 32]), 0).unwrap();

        let root = CallRef::new(tx, Vec::new());
        assert!(root.is_root(), "an empty traceAddress IS the root call");
        assert_eq!(root.tx(), &tx, "the root call still names its transaction");
        assert!(root.call_path().is_empty());

        // The log a ROOT frame emits is a position like any other: it is
        // qualified by the root call and carries a block-wide log index.
        let log = LogRef::new(tx, 0);
        assert_eq!(log.tx(), &tx);
        assert_eq!(root.tx().anchor(), log.tx().anchor());
    }

    /// A nested call names its own path, and two different paths inside ONE
    /// transaction are different calls — a transaction-wide claim is a
    /// [`TxRef`], never a [`CallRef`].
    #[test]
    fn a_nested_call_is_identified_by_its_trace_path() {
        let anchor = anchor(100, 0xaa);
        let tx = TxRef::new(anchor, B256::new([0xcc; 32]), 1).unwrap();

        let first = CallRef::new(tx, vec![0]);
        let second = CallRef::new(tx, vec![1]);
        let deeper = CallRef::new(tx, vec![0, 2]);

        assert_ne!(
            first, second,
            "two children of one call are different calls"
        );
        assert_ne!(first, deeper, "a descendant is not its parent");
        assert!(
            !first.is_root() && !deeper.is_root(),
            "only the empty path is the root"
        );
        assert_eq!(deeper.call_path(), [0, 2]);
        assert_eq!(
            first.tx(),
            deeper.tx(),
            "both calls live in the same transaction's tree"
        );
    }

    /// A log position cannot exist without its transaction: the `TxRef` is a
    /// required member, so log identity is total by construction — there is no
    /// zero-defaulted log to build.
    #[test]
    fn a_log_identity_is_qualified_by_its_full_transaction_identity() {
        let anchor = anchor(100, 0xaa);
        let tx = TxRef::new(anchor, B256::new([0xcc; 32]), 3).unwrap();
        let log = LogRef::new(tx, 7);

        assert_eq!(log.tx(), &tx);
        assert_eq!(log.tx().anchor(), &anchor);
        assert_eq!(log.index(), 7);

        // `index` is the receipt's BLOCK-WIDE `logIndex`, so two logs of ONE
        // transaction keep the indices the block gave them — never 0 and 1 as a
        // transaction-relative enumeration would.
        let later = LogRef::new(tx, 9);
        assert_ne!(log.index(), later.index());
        assert_ne!(log, later, "the two logs are different positions");
        assert_eq!(
            (log.index(), later.index()),
            (7, 9),
            "the indices are the block's own, not a per-transaction count"
        );
    }

    /// Parent-hash disagreement is its own verdict, distinguishable from an
    /// ordinary forward advance (ADR-105 D9): the next height building on some
    /// other block is a `ParentHashMismatch`, never a `NotConsecutive`.
    #[test]
    fn a_parent_hash_disagreement_is_not_an_ordinary_forward_advance() {
        let parent = anchor(100, 0xaa);

        let child =
            ChainAnchor::new(CHAIN, 101, B256::new([0xbb; 32]), B256::new([0xaa; 32])).unwrap();
        assert_eq!(
            child.links_from(&parent),
            Ok(()),
            "the ordinary advance \
            is the next height on the parent's own hash"
        );

        let branch =
            ChainAnchor::new(CHAIN, 101, B256::new([0xcc; 32]), B256::new([0xdd; 32])).unwrap();
        assert_eq!(
            branch.links_from(&parent),
            Err(AnchorLinkError::ParentHashMismatch {
                expected: parent.hash(),
                actual: B256::new([0xdd; 32]),
            }),
            "the same height step onto a foreign parent is the disagreement shape"
        );
        assert_ne!(
            branch.links_from(&parent),
            Err(AnchorLinkError::NotConsecutive {
                head: 101,
                parent: 100,
            }),
            "a consecutive-height disagreement must not be reported as a gap"
        );

        let gap = anchor(103, 0xee);
        assert!(matches!(
            gap.links_from(&parent),
            Err(AnchorLinkError::NotConsecutive { .. })
        ));
    }

    /// A parent on another chain names no predecessor, whatever the heights and
    /// hashes say: the chain check precedes the height and parent-hash
    /// evaluations, so no cross-chain link is ever reported as an ordinary
    /// advance or as a same-height disagreement (ADR-105 D1).
    #[test]
    fn a_cross_chain_parent_can_never_link() {
        let parent = anchor(100, 0xaa);

        // Byte-identical height, hash, AND parent hash — only the chain differs.
        let foreign =
            ChainAnchor::new(2, parent.number() + 1, B256::new([0xbb; 32]), parent.hash()).unwrap();
        assert_eq!(
            foreign.links_from(&parent),
            Err(AnchorLinkError::ChainMismatch { head: 2, parent: 1 }),
            "a child on chain 2 cannot link from a chain-1 parent"
        );
        assert_ne!(
            foreign.links_from(&parent),
            Err(AnchorLinkError::ParentHashMismatch {
                expected: parent.hash(),
                actual: parent.hash(),
            }),
            "the mismatch is the CHAIN, not the parent hash"
        );

        // The chain verdict wins even when the height and parent hash disagree
        // too: the cross-chain fact is the first thing refused.
        let foreign_gap = ChainAnchor::new(
            2,
            parent.number() + 5,
            B256::new([0xcc; 32]),
            B256::new([0xdd; 32]),
        )
        .unwrap();
        assert!(matches!(
            foreign_gap.links_from(&parent),
            Err(AnchorLinkError::ChainMismatch { head: 2, parent: 1 })
        ));

        // The same height and parent hash ON the parent's chain is the ordinary
        // advance: the chain is what the foreign anchors above were refused for.
        let same_chain = ChainAnchor::new(
            CHAIN,
            parent.number() + 1,
            B256::new([0xbb; 32]),
            parent.hash(),
        )
        .unwrap();
        assert_eq!(same_chain.links_from(&parent), Ok(()));
    }

    /// A parent at the top of the height range has no next height: the check
    /// answers `NotConsecutive` instead of wrapping to height 0 and accepting a
    /// genesis-height head.
    #[test]
    fn the_height_check_does_not_wrap_at_the_top_of_the_range() {
        let top = anchor(u64::MAX, 0xaa);
        let wrapped = ChainAnchor::new(CHAIN, 0, B256::new([0xbb; 32]), top.hash()).unwrap();

        assert!(
            matches!(
                wrapped.links_from(&top),
                Err(AnchorLinkError::NotConsecutive {
                    head: 0,
                    parent: u64::MAX
                })
            ),
            "no height follows u64::MAX, so nothing links from it"
        );
    }
}
