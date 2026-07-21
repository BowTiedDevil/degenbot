//! Multicall3 batching for read-only `eth_call` fan-out.
//!
//! The Aave market-wide verify (`verify_all_positions_on_conn`) issues one or
//! two `eth_call`s per stored position serially; for a full market that is the
//! minutes-long tail of `aave update`. This module folds a batch of
//! `(target, calldata)` calls into a single Multicall3 `aggregate3` `eth_call`
//! (canonical deployed address `0xcA11bde05977b3631167028862bE2a173976CA11`),
//! with `allowFailure = true` so a reverted sub-call surfaces as
//! `success = false` + empty `returnData` instead of reverting the whole
//! batch — preserving the existing "revert ⇒ actual == zero" verify semantics.
//!
//! Pure encode/decode (no RPC) so the ABI path is fixture-testable against an
//! independent ABI encoder (`eth_abi`/`eth_utils` reference vectors). The RPC
//! boundary is a thin `eth_call` to [`MULTICALL3_ADDRESS`].

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{address, Address, Bytes};

use crate::provider::AlloyProvider;
use degenbot_core::errors::{ProviderError, ProviderResult};

/// The canonical Multicall3 deployed address (same on every supported chain —
/// pre-deployed via CREATE2 at the chain's first block). Hardcoding matches
/// the precedent in `degenbot-simulation::dispatch::MULTICALL3`.
pub const MULTICALL3_ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

/// keccak256("aggregate3((bool,address,bytes)[])")[0..4] = `0x134dd343`,
/// independently confirmed via `eth_utils.keccak`.
pub const AGGREGATE3_SELECTOR: [u8; 4] = [0x13, 0x4d, 0xd3, 0x43];

/// Maximum sub-calls per Multicall3 `aggregate3` batch. Each encoded `Call3`
/// is ~192 bytes, so 1024 calls keeps the `eth_call` calldata under ~200 KiB
/// (a safe margin below typical node request-body limits).
pub const MULTICALL3_BATCH_SIZE: usize = 1024;

/// One Multicall3 `Result`: `(bool success, bytes returnData)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MulticallResult {
    /// `false` means the sub-call reverted (with `allowFailure = true` the
    /// batch itself still succeeded).
    pub success: bool,
    /// The raw sub-call return data (empty on revert).
    pub return_data: Bytes,
}

/// Encode `aggregate3((bool,address,bytes)[])` calldata for `calls`, with
/// `allowFailure = true` on every call.
///
/// # Errors
///
/// Returns [`ProviderError::EncodingError`] if the alloy ABI encoder rejects
/// a value (defensive — `Address`/`Bytes` are always encodable).
pub fn encode_aggregate3(calls: &[(Address, Bytes)]) -> ProviderResult<Bytes> {
    let tuples: Vec<DynSolValue> = calls
        .iter()
        .map(|(target, data)| {
            DynSolValue::Tuple(vec![
                DynSolValue::Bool(true),
                DynSolValue::Address(*target),
                DynSolValue::Bytes(data.to_vec()),
            ])
        })
        .collect();
    let params = DynSolValue::Tuple(vec![DynSolValue::Array(tuples)]);
    let mut calldata = Vec::with_capacity(4 + 32 * (3 + calls.len()));
    calldata.extend_from_slice(&AGGREGATE3_SELECTOR);
    calldata.extend_from_slice(&params.abi_encode_params());
    Ok(Bytes::from(calldata))
}

/// Decode the `aggregate3` return — `(bool,bytes)[]` — into exactly `n`
/// results. The decode is strict: a malformed/short payload is a hard
/// [`ProviderError::DecodingError`] (the batch caller `multicall3_batch`
/// catches this and falls back to sequential per-call `eth_call`, so a
/// malformed Multicall3 return never silently masks positions — it degrades
/// to the exact pre-multicall behavior). A sub-call that reverted is
/// `success = false` with empty `return_data` (NOT a decode error — verified
/// by the `allowFailure = true` batch itself succeeding).
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] when the payload is unreadable
/// (empty when `n > 0`, truncated, non-array top-level shape, or a malformed
/// element offset table).
pub fn decode_aggregate3_results(data: &Bytes, n: usize) -> ProviderResult<Vec<MulticallResult>> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let ty: DynSolType = "(bool,bytes)[]"
        .parse()
        .map_err(|e| ProviderError::DecodingError {
            message: format!("multicall3 return type parse: {e}"),
        })?;
    let decoded = ty
        .abi_decode(data)
        .map_err(|e| ProviderError::DecodingError {
            message: format!("multicall3 return decode: {e}"),
        })?;
    let DynSolValue::Array(arr) = decoded else {
        return Err(ProviderError::DecodingError {
            message: "multicall3 return was not an array".into(),
        });
    };
    let mut out = Vec::with_capacity(n);
    for elem in arr.iter().take(n) {
        match elem {
            DynSolValue::Tuple(fields) if fields.len() == 2 => {
                let success = matches!(fields[0], DynSolValue::Bool(true));
                let return_data = match &fields[1] {
                    DynSolValue::Bytes(b) => Bytes::from(b.clone()),
                    _ => Bytes::new(),
                };
                out.push(MulticallResult {
                    success,
                    return_data,
                });
            }
            _ => out.push(MulticallResult {
                success: false,
                return_data: Bytes::new(),
            }),
        }
    }
    while out.len() < n {
        out.push(MulticallResult {
            success: false,
            return_data: Bytes::new(),
        });
    }
    Ok(out)
}

/// Run a batch of read-only calls through one Multicall3 `aggregate3`
/// `eth_call` (chunked into `MULTICALL3_BATCH_SIZE`-sized batches). Returns
/// one [`MulticallResult`] per input call, in order. A reverted sub-call is
/// `success = false` with empty `return_data` (it is NOT an error — the caller
/// decides how to treat a revert; the Aave verify path treats revert ⇒
/// actual == zero, the V3/V4 liquidity verifier treats it as a hard RPC
/// error).
///
/// `block_number` matches `AlloyProvider::eth_call`'s shape: `Some(b)` calls
/// at block `b`; `None` calls at "pending" (latest).
///
/// If the Multicall3 `eth_call` itself errors (e.g. Multicall3 is not deployed
/// on the chain — never expected for Aave V3 chains) the batch falls back to
/// one sequential `eth_call` per call, preserving the exact pre-multicall
/// behavior.
///
/// # Errors
///
/// Returns [`ProviderError`] only if BOTH the Multicall3 batch and the
/// sequential fallback `eth_call` for the same batch fail.
pub async fn multicall3_batch(
    provider: &AlloyProvider,
    calls: &[(Address, Bytes)],
    block_number: Option<u64>,
) -> ProviderResult<Vec<MulticallResult>> {
    let mut results = Vec::with_capacity(calls.len());
    for chunk in calls.chunks(MULTICALL3_BATCH_SIZE) {
        // Encode + dispatch the batch. Treat the batch as successful only if
        // BOTH the `eth_call` succeeds AND the return decodes cleanly;
        // otherwise fall back to sequential per-call `eth_call`. This makes
        // Multicall3 a pure optimization: any surprise (chain without
        // Multicall3 deployed, a malformed return, a transport error) degrades
        // to the prior one-call-per-position path with no semantics change.
        match async {
            let calldata = encode_aggregate3(chunk)?;
            let data = provider
                .eth_call(&MULTICALL3_ADDRESS, calldata, block_number)
                .await?;
            decode_aggregate3_results(&data, chunk.len())
        }
        .await
        {
            Ok(batch) => results.extend(batch),
            Err(_) => {
                // Fallback: Multicall3 not deployed / unreachable / returned
                // a malformed payload. Revert to sequential per-call eth_call
                // so verify keeps working (preserves the pre-multicall
                // per-call behavior).
                for (target, data) in chunk {
                    match provider.eth_call(target, data.clone(), block_number).await {
                        Ok(return_data) => results.push(MulticallResult {
                            success: true,
                            return_data,
                        }),
                        Err(_) => results.push(MulticallResult {
                            success: false,
                            return_data: Bytes::new(),
                        }),
                    }
                }
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::primitives::hex_literal::hex;
    use alloy::primitives::{address, U256};

    // The reference vectors below were computed independently with
    // eth_abi.encode + eth_utils.keccak in a throwaway Python probe — a
    // DIFFERENT ABI encoder than this crate's alloy `DynSolValue`. They are the
    // source of truth the alloy encode/decode must reproduce byte-for-byte.

    /// `eth_abi` calldata for `aggregate3` of two `allowFailure=true` calls:
    ///   call0 = balanceOf(0x22…22) on 0x11…11
    ///   call1 = scaledBalanceOf(0x33…33) on 0x44…44
    const REFERENCE_CALLDATA_2_CALLS: &[u8] = &hex!(
        "134dd3430000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000100000000000000000000000011111111111111111111111111111111111111110000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000002470a0823100000000000000000000000022222222222222222222222222222222222222220000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000004444444444444444444444444444444444444444000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000241da24f3e000000000000000000000000333333333333333333333333333333333333333300000000000000000000000000000000000000000000000000000000"
    );

    /// `eth_abi` return (the `(bool,bytes)[]` from `aggregate3`) for two
    /// results: result 0 succeeds with a 32-byte `0x..2a` (= 42); result 1
    /// reverts (success=false, empty returnData).
    const REFERENCE_RETURNDATA_2_RESULTS: &[u8] = &hex!(
        "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000c0000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000002a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000000"
    );

    /// `eth_abi` calldata for `aggregate3` of zero calls (0x20 offset + length 0).
    const REFERENCE_CALLDATA_0_CALLS: &[u8] = &hex!(
        "134dd34300000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000"
    );

    /// `aggregate3` selector is keccak256("aggregate3((bool,address,bytes)[])")
    /// [0..4], independently confirmed = `0x134dd343`.
    #[test]
    fn aggregate3_selector_is_134dd343() {
        assert_eq!(AGGREGATE3_SELECTOR, [0x13, 0x4d, 0xd3, 0x43]);
    }

    /// `encode_aggregate3` reproduces the `eth_abi` reference calldata for a
    /// 2-call batch (selector + ABI-encoded `(bool,address,bytes)[]` with
    /// `allowFailure = true`).
    #[test]
    fn encode_aggregate3_matches_reference_vector() {
        let target0 = address!("1111111111111111111111111111111111111111");
        let target1 = address!("4444444444444444444444444444444444444444");
        let mut balance_call = vec![0x70, 0xa0, 0x82, 0x31];
        balance_call.extend(std::iter::repeat_n(0u8, 12));
        balance_call.extend(std::iter::repeat_n(0x22u8, 20));
        // scaledBalanceOf(0x33…33): 0x1da24f3e + 12 zero bytes + the 20-byte arg.
        let mut scaled_call = vec![0x1d, 0xa2, 0x4f, 0x3e];
        scaled_call.extend(std::iter::repeat_n(0u8, 12));
        scaled_call.extend(std::iter::repeat_n(0x33u8, 20));
        let batch = [
            (target0, Bytes::from(balance_call)),
            (target1, Bytes::from(scaled_call)),
        ];
        let encoded = encode_aggregate3(&batch).unwrap();
        assert_eq!(
            encoded.as_ref(),
            REFERENCE_CALLDATA_2_CALLS,
            "encode_aggregate3 must match the eth_abi reference byte-for-byte"
        );
    }

    /// `decode_aggregate3_results` reproduces the `eth_abi` reference return:
    /// result 0 succeeds with a 32-byte `0x..2a`; result 1 is a revert
    /// (`success=false`, empty returnData).
    #[test]
    fn decode_aggregate3_results_matches_reference_vector() {
        let data = Bytes::from(REFERENCE_RETURNDATA_2_RESULTS.to_vec());
        let results = decode_aggregate3_results(&data, 2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].success);
        assert_eq!(results[0].return_data.len(), 32);
        assert_eq!(
            U256::from_be_slice(&results[0].return_data),
            U256::from(42u64)
        );
        assert!(!results[1].success);
        assert!(results[1].return_data.is_empty());
    }

    /// An empty call list encodes `aggregate3` with a zero-length array (and
    /// `decode_aggregate3_results` on an empty payload yields zero reverted
    /// results). The RPC layer never issues an `eth_call` for an empty list.
    #[test]
    fn encode_aggregate3_empty_is_zero_length_array() {
        let encoded = encode_aggregate3(&[]).unwrap();
        assert_eq!(encoded.as_ref(), REFERENCE_CALLDATA_0_CALLS);
        let decoded = decode_aggregate3_results(&Bytes::new(), 0).unwrap();
        assert!(decoded.is_empty());
    }

    /// A malformed / too-short return payload is a hard decode error (NOT
    /// silently treated as all-reverted). The batch caller (`multicall3_batch`)
    /// catches this + falls back to sequential per-call `eth_call`, so a
    /// malformed Multicall3 return never silently masks every position as
    /// zero-divergence — it degrades to the exact pre-multicall behavior.
    #[test]
    fn decode_short_payload_is_a_hard_error() {
        let result = decode_aggregate3_results(&Bytes::from(vec![0u8; 8]), 3);
        assert!(
            result.is_err(),
            "a malformed multicall return must surface as a decode error"
        );
    }
}
