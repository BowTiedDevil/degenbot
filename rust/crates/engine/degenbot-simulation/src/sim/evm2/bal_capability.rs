//! The startup BAL capability probe: proof, before anything is armed, that the
//! node can SERVE a canonical block access list and this tree can fully DECODE
//! it.
//!
//! # Why a probe at all
//!
//! The opt-in delta-dispatch session (`simulation.delta_dispatch`) turns every
//! accepted head into one BAL acquisition. A node that serves neither official
//! EIP-7928 representation (`debug_getRawBlockAccessList`,
//! `eth_getBlockAccessList`) does not fail loudly on its own: each head's
//! acquisition is best-effort and reports `bal-unreadable`, so the opt-in would
//! quietly collapse to the legacy lane one failed request per head — an operator
//! would be paying for a flag that does nothing, with the evidence buried in a
//! diagnostic label. The probe converts that into a construction refusal: an ON
//! session arms no pipeline and processes no candidate until a real canonical
//! head's list has been fetched AND decoded.
//!
//! # What it asks for, and what it proves
//!
//! [`probe_bal_capability`] reads the current canonical height
//! (`eth_blockNumber`), resolves that height's EXACT `(number, hash)` lineage
//! (`eth_getBlockByNumber`, never a mutable tag), and then calls the shared
//! acquisition [`fetch_bal_seed`] at that exact hash with the default plan. The
//! decode is the acquisition's, so a successful raw OR official-JSON answer that
//! decodes as one complete EIP-7928 list is the proof; a list that is merely
//! well-formed RPC but undecodable is not.
//!
//! The probe is capability-only: it installs no generation, seeds no memo, and
//! changes no retention or relevance. The seed it decoded is discarded.
//!
//! # Negotiation rides the session
//!
//! [`fetch_bal_seed`] goes through the provider's sticky raw-first negotiation, so
//! an eth-only node pays ONE raw refusal here and every later head preparation
//! asks the JSON method directly. A raw-only node and a dual-endpoint node both
//! stay on raw. A failure of the representation actually chosen is never retried
//! through the other one — a real block failure must not masquerade as a
//! method-absence fallback.
//!
//! # Failure is loud and actionable
//!
//! Every failure is a [`BalCapabilityError`] naming the operation, the block
//! number/hash where known, and the underlying cause, and it carries the
//! remediation its OWN stage can act on:
//!
//! - A **lineage** failure (unreadable height, unreadable header, or a height
//!   whose header is absent/incomplete) never reached the BAL methods, so its
//!   message points at node health and canonical-header availability. Telling
//!   that operator to enable EIP-7928 support would send them to the wrong
//!   setting.
//! - An **acquisition/decode** failure (both methods unsupported, a `null`
//!   answer, a transport error mid-fetch, a malformed list) names both official
//!   methods and offers the two real ways out: enable EIP-7928 support on the
//!   endpoint, or disable `simulation.delta_dispatch`.

use alloy::primitives::B256;
use degenbot_core::op_info;
use degenbot_rpc::provider::AlloyProvider;

use super::bal_prefetch::{fetch_bal_seed, BalPlan, BalSkip};

/// The remediation a LINEAGE failure carries: the probe never got as far as a BAL
/// method, so the operator is directed at the node's own health and canonical
/// headers rather than at EIP-7928 support they may already have.
const LINEAGE_REMEDIATION: &str = "check the endpoint's health and canonical-header availability \
     (eth_blockNumber and eth_getBlockByNumber must answer for the endpoint's own reported head)";

/// The remediation an ACQUISITION/DECODE failure carries: the two ways out an
/// operator has when the node cannot serve a list this build can decode.
const BAL_REMEDIATION: &str = "enable EIP-7928 block access list RPC support on the endpoint \
     (debug_getRawBlockAccessList or eth_getBlockAccessList), or disable \
     simulation.delta_dispatch";

/// What one successful probe proved: the exact block whose access list was
/// fetched and fully decoded — the current canonical head for
/// [`probe_bal_capability`], the named pin for [`probe_bal_capability_at`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalCapability {
    /// The block's height.
    pub number: u64,
    /// The block's own canonical hash — the hash the list was asked at.
    pub hash: B256,
}

impl BalCapability {
    /// The capability `number`/`hash` proved.
    #[must_use]
    pub const fn new(number: u64, hash: B256) -> Self {
        Self { number, hash }
    }
}

/// Why an ON session's startup capability check failed — every variant a
/// construction refusal, none of them a silent OFF.
///
/// The first three variants are LINEAGE failures: they never reached a BAL
/// method, so they carry node-health/canonical-header remediation. Only
/// [`BalCapabilityError::Unusable`] means the endpoint could not serve — or this
/// build could not decode — a list, and only it names the EIP-7928 remediation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BalCapabilityError {
    /// The current canonical block number could not be read.
    #[error("the BAL startup capability probe could not read the current canonical block number: {cause}; {}", LINEAGE_REMEDIATION)]
    HeadNumber { cause: String },

    /// The resolved height's exact `(number, hash)` lineage could not be read.
    #[error("the BAL startup capability probe could not read block {number}'s canonical lineage (hash and parent hash): {cause}; {}", LINEAGE_REMEDIATION)]
    Lineage { number: u64, cause: String },

    /// The reported height has no COMPLETE canonical lineage: the node answered
    /// no block at that number, or answered a header missing its `hash` or
    /// `parentHash`.
    #[error("the BAL startup capability probe found no complete canonical lineage at the reported head number {number} (no block at that number, or a header missing its hash or parentHash), so there is no exact hash to prove capability at; {}", LINEAGE_REMEDIATION)]
    UnknownHead { number: u64 },

    /// The list at the exact head hash could not be acquired or decoded.
    #[error("the BAL startup capability probe could not acquire and decode the block access list of block {number} ({hash:#x}) through debug_getRawBlockAccessList or eth_getBlockAccessList: {cause}; {}", BAL_REMEDIATION)]
    Unusable {
        number: u64,
        hash: B256,
        cause: BalSkip,
    },
}

/// Prove that the endpoint can serve the CURRENT canonical head's block access
/// list and that this tree fully decodes it — the startup check an arming ON
/// session runs before it hands the pipeline a driver.
///
/// # Errors
///
/// [`BalCapabilityError`] for every failure: an unreadable height
/// ([`BalCapabilityError::HeadNumber`]), an unreadable lineage
/// ([`BalCapabilityError::Lineage`]), a height with no COMPLETE canonical lineage
/// ([`BalCapabilityError::UnknownHead`] — no block, or a header missing its hash
/// or parent hash), or an acquisition/decode failure at the resolved hash
/// ([`BalCapabilityError::Unusable`]).
pub async fn probe_bal_capability(
    provider: &AlloyProvider,
) -> Result<BalCapability, BalCapabilityError> {
    let number =
        provider
            .get_block_number()
            .await
            .map_err(|error| BalCapabilityError::HeadNumber {
                cause: error.to_string(),
            })?;
    let lineage = provider
        .get_block_lineage_at(number)
        .await
        .map_err(|error| BalCapabilityError::Lineage {
            number,
            cause: error.to_string(),
        })?
        .ok_or(BalCapabilityError::UnknownHead { number })?;
    probe_bal_capability_at(provider, lineage.number, lineage.hash).await
}

/// Prove capability at ONE named block — the explicit-hash seam, for a caller
/// that already knows the exact `(number, hash)` it wants proved (a fixed
/// historical pin, a replay, a live check).
///
/// It reads no head: the acquisition asks the shared [`fetch_bal_seed`] at
/// `hash` and nothing else.
///
/// # Errors
///
/// [`BalCapabilityError::Unusable`] when the list at `hash` cannot be acquired or
/// decoded.
pub async fn probe_bal_capability_at(
    provider: &AlloyProvider,
    number: u64,
    hash: B256,
) -> Result<BalCapability, BalCapabilityError> {
    let seed = fetch_bal_seed(provider, hash, &BalPlan::default())
        .await
        .map_err(|cause| BalCapabilityError::Unusable {
            number,
            hash,
            cause,
        })?;
    op_info!(
        domain = sim,
        block = number,
        hash = %hash,
        seed_writes = seed.writes.len(),
        seed_reads = seed.reads.len(),
        "the BAL startup capability probe decoded the requested block's block access list"
    );
    Ok(BalCapability::new(number, hash))
}
