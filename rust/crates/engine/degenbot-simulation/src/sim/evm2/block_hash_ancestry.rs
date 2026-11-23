//! BLOCKHASH answers proven against the solve anchor's own ancestry.
//!
//! The EVM states the window rule the interpreter enforces: a request for the
//! executing block, a future block, or a block more than
//! [`BLOCK_HASH_HISTORY`] deep is zero, and only an in-window number is a chain
//! read. [`BlockHashAncestry::block_hash`] keeps that rule here too, so a
//! database caller outside the interpreter (a replay, a probe, a second host)
//! gets the same answer without a read.
//!
//! The window is measured from the frame's EXECUTION height (`BlockEnv.number`),
//! which is not always the read anchor. The handle's main EVM executes at the
//! anchor; its next-block scratch frame executes at `anchor + 1` over state
//! pinned at the anchor, so there `BLOCKHASH(NUMBER - 1)` is the anchor's own
//! hash and is proven by the one anchor header. The proof never moves with the
//! execution clock: ancestry is always the READ anchor's, running downward, so
//! a request inside the window but ABOVE the anchor is refused
//! ([`BlockHashRefusal::AboveAnchor`]) rather than fabricated.
//!
//! The hash itself cannot be pinned by EIP-1898: a historical block hash is
//! requested by HEIGHT, and a height alone cannot say which branch it belongs
//! to, so a node is free to answer the current canonical block at that height.
//! The proof therefore comes from the anchor's side — the ancestry chain ending
//! at the exact identity the read anchor names:
//!
//! 1. ONE header-chain acquisition (`chain_evidence::acquire_header_chain`,
//!    `eth_getBlockByNumber` with no bodies) reads every height from the
//!    requested block up to the anchor. Every served header must state its
//!    requested height (the provider's lineage decode refuses one that does
//!    not) and every child must name the header below it as its parent.
//! 2. The chain's terminal header must BE the anchor: the anchor's height and
//!    the canonical hash the [`ReadAnchor`] names. A node that has moved to a
//!    replaced block at that height answers another identity, and the read is
//!    refused ([`BlockHashRefusal::AnchorMoved`]) instead of serving the
//!    replacement branch's ancestors.
//! 3. A deeper request extends the chain DOWNWARD from its lowest proven height
//!    and must link to the parent hash that header states. A replaced (or
//!    missing) block anywhere in the walk is a typed refusal
//!    ([`HeaderRefusal::BrokenParentLink`], [`HeaderRefusal::MissingHeader`]),
//!    never a zero and never a hash from another branch.
//!
//! An unresolved anchor names no identity to prove against, so an in-window
//! request fails closed ([`BlockHashRefusal::UnresolvedAnchor`]) rather than
//! serving the current chain's block at that height.
//!
//! # Evidence memory and RPC budget
//!
//! Proven headers are memoized per adapter, and the adapter is built per read
//! anchor, so a memo can never outlive the identity it was proven for or be
//! read from another branch. The window bounds it: at most the anchor plus the
//! in-window ancestors below it, and a next-block frame's depth-1 request (the
//! anchor itself) costs exactly ONE `eth_getBlockByNumber` header read. A
//! request costs one acquisition of the heights not yet proven — at most 256
//! header reads for the deepest first request (the anchor plus every in-window
//! ancestor it must cross), one read per additionally required level for a
//! later deeper request, and NONE for a repeat request or an EVM-zero answer.
//! Acquisitions keep `chain_evidence`'s bounded concurrency in flight, so a
//! deep walk is a few waves of small header reads. A deterministic refusal is
//! sticky, so a contradicted chain costs one acquisition rather than one per
//! request.

use std::collections::BTreeMap;

use alloy::primitives::B256;
use degenbot_core::errors::ProviderError;
use degenbot_rpc::chain_evidence::{acquire_header_chain, ChainEvidenceError, HeaderRefusal};
use degenbot_rpc::provider::AlloyProvider;

use ::evm2::constants::BLOCK_HASH_HISTORY;
use ::evm2::interpreter::Word;

use crate::sim::storage_memo::ReadAnchor;

/// Why a BLOCKHASH read could not be answered from the anchor's ancestry.
///
/// Every variant is deterministic evidence about the requested position: the
/// same read cannot be answered by retrying it, so the refusal is sticky.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BlockHashRefusal {
    /// The read anchor resolved no block hash, so no ancestry ends at it.
    #[error(
        "block {requested} needs the anchor's ancestry, but the read anchor at block {anchor} names no canonical hash"
    )]
    UnresolvedAnchor {
        /// The read anchor's height.
        anchor: u64,
        /// The requested historical height.
        requested: u64,
    },
    /// The node serves another block at the anchor's height.
    #[error(
        "the read anchor's block {number} ({hash}) is not the block the node serves at that height"
    )]
    AnchorMoved {
        /// The anchor's height.
        number: u64,
        /// The canonical hash the anchor names.
        hash: B256,
    },
    /// The request is inside the execution window but above the READ anchor:
    /// the anchor's ancestry runs downward, so it cannot prove a later block.
    #[error(
        "block {requested} is above the read anchor {anchor}, so the anchor's ancestry cannot prove it"
    )]
    AboveAnchor {
        /// The read anchor's height.
        anchor: u64,
        /// The requested height.
        requested: u64,
    },
    /// The header chain the ancestry requires contradicted itself.
    #[error(transparent)]
    Evidence(ChainEvidenceError),
}

/// Why a BLOCKHASH read failed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BlockHashError {
    /// The chain evidence contradicts the ancestry.
    #[error(transparent)]
    Refusal(#[from] BlockHashRefusal),
    /// The read itself failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// One proven ancestor: its own hash and the parent its header names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProvenHeader {
    hash: B256,
    parent_hash: B256,
}

/// The ancestry evidence a [`BlockHashAncestry`] has proven so far.
#[derive(Debug)]
enum AncestryState {
    /// No ancestry evidence read yet.
    Unproven,
    /// The contiguous chain from the anchor down to `floor`, every link proven.
    Proven {
        floor: u64,
        headers: BTreeMap<u64, ProvenHeader>,
    },
    /// A deterministic contradiction, sticky for the rest of the adapter's life.
    Refused(BlockHashRefusal),
}

/// The BLOCKHASH resolver for one read anchor executing at one height.
///
/// One instance belongs to one database adapter, which is built per read
/// anchor, so the proven ancestry and its memo are scoped to the exact identity
/// the reads are pinned to. The EXECUTION height is separate from that anchor:
/// the EVM's current/future/window rule is a fact about the block the frame
/// runs in (`BlockEnv.number`), while every read is pinned to the anchor's
/// state. The handle's main EVM executes at its anchor (`Self::new`); its
/// next-block scratch frame executes at `anchor + 1` over that same state
/// (`Self::executing`).
#[derive(Debug)]
pub struct BlockHashAncestry {
    anchor: ReadAnchor,
    execution: u64,
    state: AncestryState,
}

impl BlockHashAncestry {
    /// A resolver for a frame EXECUTING at `anchor.number()` over the anchor's
    /// own state — the main per-block EVM.
    #[must_use]
    pub const fn new(anchor: ReadAnchor) -> Self {
        Self::executing(anchor, anchor.number())
    }

    /// A resolver for a frame executing at `execution` over state pinned at
    /// `anchor` — the next-block scratch frame, whose `BLOCKHASH(NUMBER - 1)`
    /// is the anchor's own hash.
    ///
    /// `execution` is the EVM's own clock and `anchor` the read position; the
    /// two are never inferred from each other. A request is in the EVM window
    /// when it is 1..=256 blocks below `execution`, and the anchor's ancestry
    /// can prove it only when it is at or below `anchor.number()`.
    #[must_use]
    pub const fn executing(anchor: ReadAnchor, execution: u64) -> Self {
        Self {
            anchor,
            execution,
            state: AncestryState::Unproven,
        }
    }

    /// The exact read anchor this ancestry is proven against.
    #[must_use]
    pub const fn anchor(&self) -> ReadAnchor {
        self.anchor
    }

    /// The height the frame executes at — the EVM's `BlockEnv.number`.
    #[must_use]
    pub const fn execution(&self) -> u64 {
        self.execution
    }

    /// The EVM's BLOCKHASH answer for `number`: zero outside the window the
    /// EXECUTION height defines, the proven ancestor's hash inside it.
    ///
    /// # Errors
    ///
    /// [`BlockHashError::Refusal`] when the anchor names no hash, when the node
    /// serves another block at the anchor's height, when the request is above
    /// the read anchor, or when the ancestry's header links contradict each
    /// other; [`BlockHashError::Provider`] when a header read fails.
    pub async fn block_hash(
        &mut self,
        provider: &AlloyProvider,
        number: Word,
    ) -> Result<B256, BlockHashError> {
        let Ok(number) = u64::try_from(number) else {
            return Ok(B256::ZERO);
        };
        let Some(depth) = self.execution.checked_sub(number) else {
            return Ok(B256::ZERO);
        };
        if depth == 0 || depth > BLOCK_HASH_HISTORY {
            return Ok(B256::ZERO);
        }
        self.prove(provider, number).await
    }

    async fn prove(
        &mut self,
        provider: &AlloyProvider,
        number: u64,
    ) -> Result<B256, BlockHashError> {
        let floor = match &self.state {
            AncestryState::Refused(refusal) => return Err(refusal.clone().into()),
            AncestryState::Proven { floor, headers } => {
                if let Some(proven) = headers.get(&number) {
                    return Ok(proven.hash);
                }
                Some(*floor)
            }
            AncestryState::Unproven => None,
        };
        if number > self.anchor.number() {
            return Err(self.refuse(BlockHashRefusal::AboveAnchor {
                anchor: self.anchor.number(),
                requested: number,
            }));
        }
        match floor {
            Some(floor) if floor > number => self.extend(provider, number, floor).await,
            Some(_) => Err(BlockHashRefusal::Evidence(
                HeaderRefusal::MissingHeader { number }.into(),
            )
            .into()),
            None => self.seed(provider, number).await,
        }
    }

    async fn seed(
        &mut self,
        provider: &AlloyProvider,
        number: u64,
    ) -> Result<B256, BlockHashError> {
        let height = self.anchor.number();
        let Some(anchor_hash) = self.anchor.hash() else {
            return Err(self.refuse(BlockHashRefusal::UnresolvedAnchor {
                anchor: height,
                requested: number,
            }));
        };
        let chain = match acquire_header_chain(provider, number, height).await {
            Ok(chain) => chain,
            Err(error) => return Err(self.classify(error)),
        };
        if chain.terminal().hash() != anchor_hash {
            return Err(self.refuse(BlockHashRefusal::AnchorMoved {
                number: height,
                hash: anchor_hash,
            }));
        }
        let Some(proven) = chain.anchor_at(number).map(|anchor| anchor.hash()) else {
            return Err(
                self.refuse(BlockHashRefusal::Evidence(ChainEvidenceError::Header(
                    HeaderRefusal::MissingHeader { number },
                ))),
            );
        };
        let headers = chain
            .anchors()
            .iter()
            .map(|anchor| {
                (
                    anchor.number(),
                    ProvenHeader {
                        hash: anchor.hash(),
                        parent_hash: anchor.parent_hash(),
                    },
                )
            })
            .collect();
        self.state = AncestryState::Proven {
            floor: number,
            headers,
        };
        Ok(proven)
    }

    async fn extend(
        &mut self,
        provider: &AlloyProvider,
        number: u64,
        floor: u64,
    ) -> Result<B256, BlockHashError> {
        let expected = match &self.state {
            AncestryState::Proven { headers, .. } => {
                headers.get(&floor).map(|proven| proven.parent_hash)
            }
            _ => None,
        };
        let Some(expected) = expected else {
            return Err(
                self.refuse(BlockHashRefusal::Evidence(ChainEvidenceError::Header(
                    HeaderRefusal::MissingHeader { number: floor },
                ))),
            );
        };
        let top = floor.saturating_sub(1);
        let chain = match acquire_header_chain(provider, number, top).await {
            Ok(chain) => chain,
            Err(error) => return Err(self.classify(error)),
        };
        let served = chain.terminal().hash();
        if served != expected {
            return Err(
                self.refuse(BlockHashRefusal::Evidence(ChainEvidenceError::Header(
                    HeaderRefusal::BrokenParentLink {
                        number: floor,
                        expected: served,
                        actual: expected,
                    },
                ))),
            );
        }
        let Some(proven) = chain.anchor_at(number).map(|anchor| anchor.hash()) else {
            return Err(
                self.refuse(BlockHashRefusal::Evidence(ChainEvidenceError::Header(
                    HeaderRefusal::MissingHeader { number },
                ))),
            );
        };
        if let AncestryState::Proven { floor, headers } = &mut self.state {
            *floor = number;
            headers.extend(chain.anchors().iter().map(|anchor| {
                (
                    anchor.number(),
                    ProvenHeader {
                        hash: anchor.hash(),
                        parent_hash: anchor.parent_hash(),
                    },
                )
            }));
        }
        Ok(proven)
    }

    fn refuse(&mut self, refusal: BlockHashRefusal) -> BlockHashError {
        self.state = AncestryState::Refused(refusal.clone());
        BlockHashError::Refusal(refusal)
    }

    fn classify(&mut self, error: ChainEvidenceError) -> BlockHashError {
        match error {
            ChainEvidenceError::Provider(error) => BlockHashError::Provider(error),
            evidence @ (ChainEvidenceError::Header(_)
            | ChainEvidenceError::Projection(_)
            | ChainEvidenceError::UnboundedFilter
            | ChainEvidenceError::CrossChainTerminal { .. }) => {
                let refusal = BlockHashRefusal::Evidence(evidence);
                self.state = AncestryState::Refused(refusal.clone());
                BlockHashError::Refusal(refusal)
            }
        }
    }
}
