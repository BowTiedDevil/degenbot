//! The `degenbot-strategy` plane — where concrete executable strategies are
//! composed, and the vocabulary they compose over.
//!
//! A **Strategy** is a top-level label for one kind of profit opportunity the
//! bot executes. It is a value composed over six capability **slots**:
//!
//! | Slot | Meaning |
//! |---|---|
//! | **source** | where opportunities are found |
//! | **infrastructure** | pool/account/token/path loaders |
//! | **calculation** | the solver |
//! | **encoder** | opportunity → onchain-executable payload |
//! | **simulator** | validity check of the payload |
//! | **submission** | delivery to an endpoint that can land it onchain |
//!
//! Distinct ecosystems get distinct strategies; shared reaction capabilities
//! are mixed in by composition, never by sub-traits.
//!
//! # Contract
//!
//! This crate owns the strategy plane and the concrete strategy compositions
//! built on it. Capability implementations stay in their own crates and are
//! re-exported here, never moved: the dependency direction is
//! `strategy → capability crates`, and no capability crate depends on this
//! one. Per-ecosystem strategies are distinct types that share reaction
//! components by composition.
//!
//! The re-export surface below names the seam types a strategy composes today
//! — the execution adapter seam (`degenbot-execution`) and the submission
//! surface (`degenbot-submission`). It invents no traits; it names what
//! exists.
//!
//! ## Capability seams
//!
//! From [`degenbot_execution`], the adapter seam: the [`ExecutionAdapter`]
//! trait, its Encode part ([`PayloadComposer`] + [`ComposerInputs`] +
//! [`ComposeError`]), the gate value types ([`ProbeSpecs`] + [`AssessRule`] +
//! [`FeePolicy`]), the gate verdict ([`ExecutionResult`]), and the solve-result
//! view ([`SolveResult`]).
//!
//! From [`degenbot_submission`], the submission surface: the candidate
//! ([`SubmitCandidate`]), the typed channel ([`SubmissionTarget`]), the result
//! ([`SubmitOutcome`]), and the decline reason ([`SkipReason`]).

pub use degenbot_execution::{
    AssessRule, ComposeError, ComposerInputs, ExecutionAdapter, ExecutionResult, FeePolicy,
    PayloadComposer, ProbeSpecs, SolveResult,
};
pub use degenbot_submission::{SkipReason, SubmissionTarget, SubmitCandidate, SubmitOutcome};
