//! The Aave V3 updater chunk loop, in two phases with one durability point.
//!
//! - **Resolution** (`run::resolver`, `config_dispatch`,
//!   `transaction_processor`): exact provider reads and pure reduction against
//!   the chunk's `ChunkSeed` and `ProjectionView`. It holds no DB transaction —
//!   its relational starting point is the synchronously loaded seed — and it
//!   owns the reduction's per-transaction provider reads; acquisition and
//!   verification own their own documented reads.
//! - **The chunk write** (`run::process`, `run::apply`, `projection`): the
//!   chunk's ONE `rusqlite::Transaction` over the reduction's deltas, journal,
//!   checkpoint, and cursor. Synchronous and RPC-free.
//!
//! The chunk-atomicity invariant and the two-writer hazard this structure fixes
//! are the crate-root docs'. Nothing here owns `pyo3`, `database_path`, or
//! `open_for_writes`.

pub mod aave_fetch;
pub mod config_dispatch;
pub mod evidence;
pub mod gho_processor;
pub mod operations;
pub mod operations_parser;
pub mod processors;
pub mod projection;
pub mod run;
pub mod transaction_processor;
pub mod verify;

#[cfg(test)]
pub(crate) mod test_chain;

pub use evidence::{
    bind_transaction_evidence, EvidenceRefusal, EvidenceRequirement, TransactionEvidence,
};
pub use gho_processor::{
    GhoProcessorError, GhoScaledTokenBurnResult, GhoScaledTokenMintResult, GhoUserOperation,
    UnifiedGhoProcessor,
};
pub use processors::{
    ProcessorError, RayDivMode, RoundingStrategy, ScaledTokenBurnResult, ScaledTokenEventData,
    ScaledTokenMintResult, ScaledTokenProcessor,
};
pub use projection::{
    checkpoint_due, classify_discount_prepass, journal_entries, journal_entry, market_projection,
    CanonicalDelta, CheckpointState, ConfigurationEffect, ContractRole, DeltaEffect, DiscountPath,
    DiscountPrepassInputs, GenerationActivation, GenerationEntity, GenerationRef, MarketProjection,
    PositionSubject, ProjectionError, ProjectionView, CHECKPOINT_ENCODING_VERSION,
    DELTA_ENCODING_VERSION,
};
pub use run::{
    activate_aave_market, activate_aave_market_on_conn, apply_aave_chunk_writes_on_conn,
    deactivate_aave_market, fetch_activation_metadata_at, reset_market_to_cold_boot,
    run_aave_update, run_aave_update_on_db, AaveChunkProgress, AaveUpdateReport, ActivatedMarket,
    ActivationMetadata, ActivationMetadataError, CanonicalDeltaWriteReport, ChunkWriteReport,
    EvidenceAcquisitionError, MarketResetFacts, NoProgress, ProgressSink, RunError, RunSpec,
    VerifyPolicy, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
pub use transaction_processor::ProcessTxError;
pub use verify::VerificationEvidenceError;
