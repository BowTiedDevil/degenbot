//! The built-in `cmd_executor` execution adapter owned by a strategy session.

use alloy::primitives::{Bytes, U256};

use degenbot_execution::SolveResult;
use degenbot_executor::composers::{
    config_for_options, encode_execute_call, ComposerInputs, EncodeContext, EncodeOptions,
    EncodeRequest, HopInfo, PathInfo,
};
use degenbot_executor::grammar_shape::{derive_all_v2_detailed, derive_shape_detailed, Derive};

/// A routine refusal from the built-in command encoder.
///
/// These labels are the stable JSONL boundary consumed by the backrun trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmdExecutorDecline {
    /// The path, solve-result view, or per-hop amounts do not form a composable shape.
    UnsupportedHopShape,
    /// An amount cannot fit the executor command wire format.
    AmountExceedsUint96,
    /// The command grammar has no producer for this path.
    CommandStream,
    /// The executor call or its configuration cannot be encoded.
    ExecuteCall,
    /// A V4 hop names a `PoolManager` other than the session context.
    MixedPoolManagers,
}

impl CmdExecutorDecline {
    /// The stable caller-facing JSONL refusal label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::UnsupportedHopShape => "unsupported_hop_shape",
            Self::AmountExceedsUint96 => "amount_exceeds_uint96",
            Self::CommandStream => "encoding_failed:cmd_stream",
            Self::ExecuteCall => "encoding_failed:execute_call",
            Self::MixedPoolManagers => "mixed_pool_managers",
        }
    }
}

/// A fatal command derivation rejection.
///
/// The grammar-specific validator error is intentionally not part of this
/// production adapter's public contract. Callers must treat this outcome as a
/// process-fatal invariant failure (ADR-030), never as a routine skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmdExecutorRejection {
    /// The ledger validator rejected a plan that the grammar built.
    LedgerValidation,
}

/// The stable result of one built-in command-executor composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdExecutorOutcome {
    /// The ABI-encoded `execute(bytes,uint256)` call.
    Encoded(Bytes),
    /// The path was routinely declined.
    Declined(CmdExecutorDecline),
    /// The grammar built a stream that violated a ledger invariant.
    Rejected(CmdExecutorRejection),
}

/// Session-owned adapter for the built-in `cmd_executor` contract.
///
/// The deployment context is captured once per strategy session. Every call
/// supplies its own path, solve result, and explicit command-encoding policy,
/// so callers can safely recompose different bribe policies from the same
/// adapter without mutating shared state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdExecutorAdapter {
    context: EncodeContext,
}

impl CmdExecutorAdapter {
    /// Capture the session-scoped executor, `PoolManager`, and WETH addresses.
    #[must_use]
    pub const fn new(context: EncodeContext) -> Self {
        Self { context }
    }

    /// Return the session-scoped encode context.
    #[must_use]
    pub const fn context(&self) -> &EncodeContext {
        &self.context
    }

    /// Compose one solved path under an explicit per-call encode policy.
    #[must_use]
    pub fn compose(
        &self,
        path: &PathInfo,
        result: &SolveResult,
        opts: EncodeOptions,
    ) -> CmdExecutorOutcome {
        let Some(inputs) = self.validated_inputs(path, result) else {
            let decline = self.classify_invalid(path, result);
            return CmdExecutorOutcome::Declined(decline);
        };

        let request = EncodeRequest::new(
            path.clone(),
            inputs.optimal_input,
            inputs.hop_outputs.clone(),
            inputs.consumed_inputs.clone(),
            opts,
        );
        let composer_inputs = ComposerInputs {
            executor_address: self.context.executor,
            pool_manager_address: self.context.pool_manager,
            weth_address: self.context.weth,
            optimal_input: request.optimal_input,
            hop_outputs: &request.hop_outputs,
            consumed_inputs: &request.consumed_inputs,
            opts: request.opts,
        };
        let derived = if path.hops.iter().all(|hop| matches!(hop, HopInfo::V2(_))) {
            derive_all_v2_detailed(&request.path, &composer_inputs)
        } else {
            derive_shape_detailed(&request.path, &composer_inputs)
        };
        let commands = match derived {
            Derive::Encoded(commands) => commands,
            Derive::Declined => {
                return CmdExecutorOutcome::Declined(CmdExecutorDecline::CommandStream)
            }
            Derive::Rejected(_) => {
                return CmdExecutorOutcome::Rejected(CmdExecutorRejection::LedgerValidation);
            }
        };

        let Ok(config) = config_for_options(opts, U256::ZERO) else {
            return CmdExecutorOutcome::Declined(CmdExecutorDecline::ExecuteCall);
        };
        let Ok(call) = encode_execute_call(self.context.executor, &commands, config) else {
            return CmdExecutorOutcome::Declined(CmdExecutorDecline::ExecuteCall);
        };
        CmdExecutorOutcome::Encoded(Bytes::from(call.data))
    }

    fn validated_inputs(&self, path: &PathInfo, result: &SolveResult) -> Option<CmdInputs> {
        let hop_count = path.hops.len();
        if hop_count < 2
            || result.hop_count != hop_count
            || result.hop_outputs.len() != hop_count
            || result.consumed_inputs.len() != hop_count
            || result.hop_descriptors.len() != hop_count
            || result
                .hop_descriptors
                .iter()
                .zip(&path.hops)
                .any(|(descriptor, hop)| {
                    *descriptor
                        != degenbot_execution::solve_result::HopDescriptor::from_hop_info(hop)
                })
        {
            return None;
        }

        let optimal_input = command_amount(result.optimal_input)?;
        let hop_outputs = result
            .hop_outputs
            .iter()
            .copied()
            .map(command_amount)
            .collect::<Option<Vec<_>>>()?;
        let consumed_inputs = result
            .consumed_inputs
            .iter()
            .copied()
            .map(command_amount)
            .collect::<Option<Vec<_>>>()?;

        if path.hops.iter().any(|hop| {
            matches!(hop, HopInfo::V4(v4) if v4.pool_manager_address != self.context.pool_manager)
        }) {
            return None;
        }

        Some(CmdInputs {
            optimal_input,
            hop_outputs,
            consumed_inputs,
        })
    }

    fn classify_invalid(&self, path: &PathInfo, result: &SolveResult) -> CmdExecutorDecline {
        if path.hops.iter().any(|hop| {
            matches!(hop, HopInfo::V4(v4) if v4.pool_manager_address != self.context.pool_manager)
        }) {
            CmdExecutorDecline::MixedPoolManagers
        } else if result.optimal_input >= U256::from(1u128 << 96)
            || result
                .hop_outputs
                .iter()
                .chain(&result.consumed_inputs)
                .any(|value| *value >= U256::from(1u128 << 96))
        {
            CmdExecutorDecline::AmountExceedsUint96
        } else {
            CmdExecutorDecline::UnsupportedHopShape
        }
    }
}

struct CmdInputs {
    optimal_input: u128,
    hop_outputs: Vec<u128>,
    consumed_inputs: Vec<u128>,
}

fn command_amount(value: U256) -> Option<u128> {
    let value = u128::try_from(value).ok()?;
    (value < (1u128 << 96)).then_some(value)
}
