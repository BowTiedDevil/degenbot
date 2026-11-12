//! The `cmd_executor/2` package's call encoding and stream bound.

use std::sync::LazyLock;

use alloy::primitives::{Bytes, U256};

use degenbot_execution::{ComposeError, PathInfo};

use crate::composers::{
    config_for_options, encode_cmd_stream_detailed, encode_execute_call, resolve_axes,
    EncodeContext, EncodeOptions, EncodeRequest,
};
use crate::grammar_ledger::{Bribe, ProfitCapture};
use crate::grammar_shape::Derive;
use crate::receipt::ExecutionReceipt;
use crate::release::{
    capability, CapabilityId, ReleaseManifest, RELEASE_ID_TAG, RELEASE_MANIFEST_V2,
};
use crate::release_registry::{ReleaseCall, ReleaseEncoding};

#[expect(
    clippy::expect_used,
    reason = "the embedded production manifest is a build-time invariant validated by ReleaseRegistry::production"
)]
static CMD_STREAM_V1_MANIFEST: LazyLock<ReleaseManifest> = LazyLock::new(|| {
    ReleaseManifest::parse(RELEASE_MANIFEST_V2)
        .expect("embedded cmd_executor/2 manifest must validate")
});

pub(crate) fn enforce_stream_length(commands: &[u8], max: u32) -> Result<(), ComposeError> {
    let max = usize::try_from(max)
        .map_err(|error| ComposeError::encode(format!("invalid stream-length bound: {error}")))?;
    if commands.len() > max {
        return Err(ComposeError::unsupported(format!(
            "command stream length {} exceeds the release maximum of {max} bytes",
            commands.len()
        )));
    }
    Ok(())
}

/// Enforce the command-stream bound declared by the embedded cmd_executor/2
/// manifest for callers that compose directly rather than through a bound release.
///
/// # Errors
///
/// [`ComposeError::Unsupported`] when the complete stream exceeds the declared bound.
pub fn enforce_cmd_stream_v1_length(commands: &[u8]) -> Result<(), ComposeError> {
    enforce_stream_length(
        commands,
        CMD_STREAM_V1_MANIFEST.call_encoding.max_stream_length,
    )
}

/// The production encoding: the `cmd_executor` command stream.
///
/// The name keeps its v1 because the call encoding did not move in the ADR-099
/// cutover: `call_encoding_version()` is still 1 and `execute` produces the same
/// bytes for the same path.
#[derive(Debug, Clone, Copy, Default)]
pub struct CmdStreamV1Encoding;

impl ReleaseEncoding for CmdStreamV1Encoding {
    fn release_id(&self) -> &str {
        RELEASE_ID_TAG
    }

    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "ADR-030: a validator Reject is a fatal internal invariant, not a candidate decline"
    )]
    fn encode(
        &self,
        context: &EncodeContext,
        request: &EncodeRequest,
    ) -> Result<ReleaseCall, ComposeError> {
        let commands = match encode_cmd_stream_detailed(context, request) {
            Derive::Encoded(commands) => commands,
            Derive::Declined => {
                return Err(ComposeError::unsupported(
                    "the command grammar has no producer for this path",
                ));
            }
            Derive::EncodingFailed(error) => {
                return Err(ComposeError::encode(format!(
                    "command stream emission failed: {error}"
                )));
            }
            Derive::Rejected(error) => panic!(
                "executor derivation REJECT (a latent bug, ADR-030): the encoder wrote a Plan the ledger validator rejected: {error:?}"
            ),
        };
        enforce_cmd_stream_v1_length(&commands)?;
        let config = config_for_options(request.opts, U256::ZERO)
            .map_err(|error| ComposeError::encode(format!("config_for_options failed: {error}")))?;
        let call = encode_execute_call(context.executor, &commands, config);
        Ok(ReleaseCall {
            data: Bytes::from(call.data),
            value: call.value,
            requires: required_capabilities(&request.path, request.opts),
        })
    }

    fn decode_receipt(
        &self,
        return_data: Option<&Bytes>,
    ) -> Result<ExecutionReceipt, degenbot_execution::AssessError> {
        ExecutionReceipt::decode(return_data)
    }
}

/// The capability IDs a solved path's `cmd_executor` call needs.
///
/// The requirement set is derived from what the encoder will actually emit, so
/// a release that does not declare one of these declines the path instead of
/// producing bytes its contract would revert on:
///
/// - every stream opens with the address table and reports a receipt, so
///   [`capability::ADDRESS_TABLE`] and [`capability::EXECUTION_RECEIPT`] always
///   apply;
/// - one family capability per pool family present in the path;
/// - [`capability::V4_BATCH`] only for a pure-V4 path whose
///   [`EncodeOptions::use_v4_batch`] axis asks for the batched open;
/// - [`capability::ERC6909_CLAIMS`] for an ERC6909 profit capture;
/// - [`capability::CONFIG_BRIBE`] when the config carries a bribe.
///
/// The list is sorted and de-duplicated, so two requirement sources naming one
/// capability report it once.
#[must_use]
pub fn required_capabilities(path: &PathInfo, opts: EncodeOptions) -> Vec<CapabilityId> {
    let mut required = vec![capability::ADDRESS_TABLE, capability::EXECUTION_RECEIPT];

    let mut v2 = false;
    let mut v3 = false;
    let mut v4 = false;
    for hop in &path.hops {
        match hop {
            degenbot_execution::HopInfo::V2(_) => v2 = true,
            degenbot_execution::HopInfo::V3(_) => v3 = true,
            degenbot_execution::HopInfo::V4(_) => v4 = true,
        }
    }
    if v2 {
        required.push(capability::V2_SWAPS);
    }
    if v3 {
        required.push(capability::V3_SWAPS);
    }
    if v4 {
        required.push(capability::V4_SWAPS);
    }
    if opts.use_v4_batch && v4 && !v2 && !v3 {
        required.push(capability::V4_BATCH);
    }

    let (_, capture, bribe) = resolve_axes(opts);
    if capture == ProfitCapture::Erc6909 {
        required.push(capability::ERC6909_CLAIMS);
    }
    if bribe != Bribe::None {
        required.push(capability::CONFIG_BRIBE);
    }

    required.sort_unstable();
    required.dedup();
    required
}
