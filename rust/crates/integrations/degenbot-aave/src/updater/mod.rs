//! The Aave V3 updater chunk-loop — transactional apply of decoded Aave events
//! under one `rusqlite::Transaction`.
//!
//! See the crate-root docs for the chunk-atomicity invariant + the two-writer
//! hazard this structure fixes. This module is the **transactional core**:
//! pure, synchronous, fixture-testable, NO RPC, NO `pyo3`, NO `database_path`,
//! NO `open_for_writes`.

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
    CanonicalDelta, ConfigurationEffect, ContractRole, DeltaEffect, GenerationActivation,
    GenerationEntity, GenerationRef, PositionSubject, ProjectionError, ProjectionView,
};
pub use run::{
    activate_aave_market, activate_aave_market_on_conn, apply_aave_chunk_writes_on_conn,
    deactivate_aave_market, reset_market_to_cold_boot, run_aave_update, run_aave_update_on_db,
    AaveChunkProgress, AaveUpdateReport, ActivatedMarket, CanonicalDeltaWriteReport,
    ChunkWriteReport, EvidenceAcquisitionError, MarketResetFacts, NoProgress, ProgressSink,
    RunError, RunSpec, VerifyPolicy, ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK,
};
pub use transaction_processor::{process_transaction, ProcessTxError};
