//! Pins the `degenbot-strategy` re-export surface: every capability seam a
//! strategy composes must be nameable through the crate root.

use degenbot_strategy::{
    AssessRule, ComposeError, ComposerInputs, DiscoveryHandles, ExecutionAdapter, ExecutionResult,
    FeePolicy, PayloadComposer, ProbeSpecs, ProvisionCell, SkipReason, SolveResult, StrategyCell,
    StrategyKit, SubmissionTarget, SubmitCandidate, SubmitOutcome,
};

#[test]
fn capability_seams_are_reexported() {
    let seams = [
        std::any::type_name::<ProbeSpecs>(),
        std::any::type_name::<AssessRule>(),
        std::any::type_name::<FeePolicy>(),
        std::any::type_name::<ExecutionResult>(),
        std::any::type_name::<ComposerInputs<'static>>(),
        std::any::type_name::<SolveResult>(),
        std::any::type_name::<ComposeError>(),
        std::any::type_name::<SubmitCandidate>(),
        std::any::type_name::<StrategyKit>(),
        std::any::type_name::<StrategyCell>(),
        std::any::type_name::<ProvisionCell>(),
        std::any::type_name::<DiscoveryHandles>(),
        std::any::type_name::<SubmissionTarget>(),
        std::any::type_name::<SubmitOutcome>(),
        std::any::type_name::<SkipReason>(),
        std::any::type_name::<&dyn ExecutionAdapter>(),
        std::any::type_name::<&dyn PayloadComposer>(),
    ];
    assert!(
        seams.iter().all(|name| name.contains("degenbot_")),
        "unexpected seam type path: {seams:?}"
    );
}
