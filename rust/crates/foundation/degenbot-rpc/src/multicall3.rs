//! Multicall3 batching for read-only `eth_call` fan-out.
//!
//! The Aave verification gates issue one or two `eth_call`s per stored
//! position; for a full market that is the minutes-long tail of `aave update`.
//! This module folds a batch of `(target, calldata)` calls into a single
//! Multicall3 `aggregate3` `eth_call` (canonical deployed address
//! `0xcA11bde05977b3631167028862bE2a173976CA11`), with `allowFailure = true` so
//! a reverted sub-call is a per-call [`MulticallOutcome::Reverted`] outcome
//! instead of a batch-wide revert: the caller classifies it as read evidence,
//! never as a zero balance (ADR-105 D3). A transport fault is not an outcome: it
//! propagates as a [`ProviderError`], so a sustained outage can never be read as
//! a fan of sub-call reverts.
//!
//! Pure encode/decode (no RPC) so the ABI path is fixture-testable against an
//! independent ABI encoder (`eth_abi`/`eth_utils` reference vectors). The RPC
//! boundary is a thin `eth_call` to [`MULTICALL3_ADDRESS`].

use std::future::Future;

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{address, Address, Bytes};

use crate::abi::ContractRead;
use crate::chain_evidence::ExactReadError;
use degenbot_core::canonical_head::ChainAnchor;
use degenbot_core::errors::{ProviderError, ProviderResult};

/// The canonical Multicall3 deployed address (same on every supported chain —
/// pre-deployed via CREATE2 at the chain's first block). Hardcoding matches the
/// convention used in the settlement-arbitrage strategy crate's dispatch tests.
pub const MULTICALL3_ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

/// keccak256("aggregate3((bool,address,bytes)[])")[0..4] = `0x134dd343`,
/// independently confirmed via `eth_utils.keccak`.
pub const AGGREGATE3_SELECTOR: [u8; 4] = [0x13, 0x4d, 0xd3, 0x43];

/// Maximum sub-calls per Multicall3 `aggregate3` batch. Each encoded `Call3`
/// is ~192 bytes, so 1024 calls keeps the `eth_call` calldata under ~200 KiB
/// (a safe margin below typical node request-body limits).
pub const MULTICALL3_BATCH_SIZE: usize = 1024;

/// keccak256("tryAggregate(bool,(address,bytes)[])")[0..4] = `0xbce38bd7`.
///
/// The Multicall2-patented batch primitive: same sub-call fan-out and the SAME
/// `(bool,bytes)[]` return shape as `aggregate3`, so [`decode_aggregate3_results`]
/// decodes it unchanged. Some nodes carry a Multicall2 deployment at
/// [`MULTICALL3_ADDRESS`] (no `aggregate3` there) — callers degrade to this
/// selector before giving up on batching.
pub const TRY_AGGREGATE_SELECTOR: [u8; 4] = [0xbc, 0xe3, 0x8b, 0xd7];

/// One Multicall3 item as the ABI reports it: `(bool success, bytes returnData)`.
///
/// The wire row of a decoded batch return — a decode artifact, not a semantic
/// class. [`MulticallOutcome`] is the class a read consumer names, and
/// [`multicall3_batch`] produces it; this row survives only for callers that
/// re-serve or inspect a batch's own ABI payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MulticallResult {
    /// The ABI tuple's success flag.
    pub success: bool,
    /// The ABI tuple's return data (a revert's payload when `success` is
    /// `false`).
    pub return_data: Bytes,
}

/// One read's outcome at the Multicall3 boundary.
///
/// No variant can hold a transport fault: [`multicall3_batch`] propagates those
/// as [`ProviderError`], so a reader matching on this type names both states and
/// can never mistake an outage for a sub-call revert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MulticallOutcome {
    /// The sub-call returned this payload.
    Returned(Bytes),
    /// The sub-call reverted, carrying the payload the wire exposed. Empty when
    /// the revert arrived as a provider error rather than as an `allowFailure`
    /// item: a JSON-RPC revert carries its reason in the error message, not as
    /// return data.
    Reverted(Bytes),
}

impl From<MulticallResult> for MulticallOutcome {
    /// Classify one decoded wire row. Only ever called on a batch return that
    /// is itself a success, so `success = false` is a sub-call revert.
    fn from(row: MulticallResult) -> Self {
        if row.success {
            Self::Returned(row.return_data)
        } else {
            Self::Reverted(row.return_data)
        }
    }
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

/// Encode `tryAggregate(bool,(address,bytes)[])` calldata for `calls`, with
/// `requireSuccess = false` (a reverted sub-call is an item result, never a
/// batch fault) — the Multicall2 twin of [`encode_aggregate3`].
///
/// # Errors
///
/// Returns [`ProviderError::EncodingError`] if the alloy ABI encoder rejects a
/// value (defensive — the inputs are always encodable).
pub fn encode_try_aggregate(calls: &[(Address, Bytes)]) -> ProviderResult<Bytes> {
    let tuples: Vec<DynSolValue> = calls
        .iter()
        .map(|(target, data)| {
            DynSolValue::Tuple(vec![
                DynSolValue::Address(*target),
                DynSolValue::Bytes(data.to_vec()),
            ])
        })
        .collect();
    let params = DynSolValue::Tuple(vec![DynSolValue::Bool(false), DynSolValue::Array(tuples)]);
    let mut calldata = Vec::with_capacity(4 + 32 * (3 + 2 * calls.len()));
    calldata.extend_from_slice(&TRY_AGGREGATE_SELECTOR);
    calldata.extend_from_slice(&params.abi_encode_params());
    Ok(Bytes::from(calldata))
}

/// Decode the `aggregate3` return — `(bool,bytes)[]` — into exactly `n`
/// results. The decode is strict: a malformed payload or a result array
/// whose length is not exactly `n` is a hard [`ProviderError::DecodingError`]
/// (the batch caller `multicall3_batch` catches this and falls back to
/// sequential per-call `eth_call`, so a malformed or miscounting Multicall3
/// return never silently masks positions — it degrades to the exact
/// pre-multicall behavior). A short array is never padded with all-reverted
/// rows, and a long one is never truncated to the first `n`. A reverted
/// sub-call is `success = false` (NOT a decode error — the `allowFailure =
/// true` batch itself succeeded) and keeps whatever payload the batch put in
/// `returnData`.
/// For `n == 0` the payload is not consulted and the result is empty.
///
/// # Errors
///
/// Returns [`ProviderError::DecodingError`] when the payload is unreadable
/// (empty when `n > 0`, truncated, non-array top-level shape, or a malformed
/// element offset table) or carries a result count other than exactly `n`.
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
    if arr.len() != n {
        return Err(ProviderError::DecodingError {
            message: format!(
                "multicall3 return carried {} results, expected exactly {n}",
                arr.len()
            ),
        });
    }
    let mut out = Vec::with_capacity(n);
    for elem in &arr {
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
    Ok(out)
}

/// Run a batch of read-only calls through one Multicall3 `aggregate3`
/// `eth_call` (chunked into [`MULTICALL3_BATCH_SIZE`]-sized batches). Returns
/// one [`MulticallOutcome`] per input call, in order. A reverted sub-call is
/// [`MulticallOutcome::Reverted`] — an outcome, not an error: the caller
/// decides what a revert means (the Aave verify path reads revert ⇒ actual ==
/// zero, the V3/V4 liquidity verifier treats it as a hard read error). A
/// transport fault is never an outcome: it propagates, so "the node did not
/// answer" and "the contract reverted" stay distinguishable to every caller.
///
/// `block_number` matches [`ContractRead::call`]'s shape: `Some(b)` calls at
/// block `b`; `None` calls at the transport's own latest.
///
/// If the Multicall3 `eth_call` itself errors (Multicall3 is not deployed on
/// the chain, a transport fault, a malformed return) the batch falls back to
/// one sequential `eth_call` per call — the exact pre-multicall request shape —
/// and those sub-calls classify the same way.
///
/// # Errors
///
/// Returns [`ProviderError`] when the batch leg could not answer AND a fallback
/// sub-call failed for any reason other than an EVM revert (a timeout, a
/// dropped connection, an exhausted retry budget). A revert never surfaces
/// here.
pub async fn multicall3_batch(
    transport: &impl ContractRead,
    calls: &[(Address, Bytes)],
    block_number: Option<u64>,
) -> ProviderResult<Vec<MulticallOutcome>> {
    let mut results = Vec::with_capacity(calls.len());
    for chunk in calls.chunks(MULTICALL3_BATCH_SIZE) {
        // Encode + dispatch the batch. The batch is a pure optimization: a
        // chain without Multicall3 deployed, a malformed return, or a failing
        // batch call degrades to the pre-multicall one-call-per-sub-call path,
        // which classifies its own results.
        match async {
            let calldata = encode_aggregate3(chunk)?;
            let data = transport
                .call(MULTICALL3_ADDRESS, calldata, block_number)
                .await?;
            decode_aggregate3_results(&data, chunk.len())
        }
        .await
        {
            Ok(rows) => results.extend(rows.into_iter().map(MulticallOutcome::from)),
            Err(_) => {
                for (target, calldata) in chunk {
                    results.push(classify_sub_call(
                        transport
                            .call(*target, calldata.clone(), block_number)
                            .await,
                    )?);
                }
            }
        }
    }
    Ok(results)
}

/// A read transport BOUND to one exact block: every call names the snapshot's
/// own hash (EIP-1898), so no call can be served by another block.
///
/// The capability belongs to the transport: one that cannot pin by hash reports
/// [`ExactReadError::Unsupported`] and never degrades to a number-addressed
/// call. [`crate::chain_evidence::BlockReads`] is the production
/// implementation; a recorded replay transport and the test doubles implement
/// the same contract, so a batch runs over ONE interface live and offline.
pub trait ExactContractRead: Send + Sync {
    /// The exact block this transport reads at.
    fn anchor(&self) -> ChainAnchor;

    /// Issue one hash-pinned `eth_call` to `to` and return the raw return
    /// bytes.
    fn call_exact(
        &self,
        to: Address,
        data: Bytes,
    ) -> impl Future<Output = Result<Bytes, ExactReadError>> + Send;
}

/// Run a batch of read-only calls through one hash-pinned Multicall3
/// `aggregate3` `eth_call`, chunked into [`MULTICALL3_BATCH_SIZE`]-sized
/// batches, and return one [`MulticallOutcome`] per input call in order.
///
/// The pin is the transport's own ([`ExactContractRead::anchor`]), so a batch is
/// ONE read position: a same-height substitution between the anchor's resolution
/// and this read is refused by the node, never served from another branch.
///
/// A batch leg that cannot answer (Multicall3 not deployed, a malformed return)
/// degrades to one PINNED call per sub-call — the pre-multicall classification,
/// still at the same anchor. A batch leg REFUSED for its request shape
/// (`ExactReadError::Unsupported`) does not degrade: the sequential leg would
/// repeat the same refused shape, so the refusal is the answer (ADR-105 D3, D11).
///
/// # Errors
///
/// [`ExactReadError`] when the node has no hash-pinned read capability, or when
/// a sequential sub-call failed for any reason other than an EVM revert. A
/// revert is a [`MulticallOutcome::Reverted`], never an error.
pub async fn multicall3_batch_pinned(
    transport: &impl ExactContractRead,
    calls: &[(Address, Bytes)],
) -> Result<Vec<MulticallOutcome>, ExactReadError> {
    let mut results = Vec::with_capacity(calls.len());
    for chunk in calls.chunks(MULTICALL3_BATCH_SIZE) {
        let block_hash = transport.anchor().hash();
        let batch = async {
            let calldata = encode_aggregate3(chunk).map_err(|cause| ExactReadError::Failed {
                method: "eth_call",
                block_hash,
                cause,
            })?;
            let data = transport.call_exact(MULTICALL3_ADDRESS, calldata).await?;
            decode_aggregate3_results(&data, chunk.len()).map_err(|cause| ExactReadError::Failed {
                method: "eth_call",
                block_hash,
                cause,
            })
        }
        .await;
        match batch {
            Ok(rows) => results.extend(rows.into_iter().map(MulticallOutcome::from)),
            Err(refusal @ ExactReadError::Unsupported { .. }) => return Err(refusal),
            Err(ExactReadError::Failed { .. }) => {
                for (target, calldata) in chunk {
                    results.push(classify_pinned_sub_call(
                        transport.call_exact(*target, calldata.clone()).await,
                    )?);
                }
            }
        }
    }
    Ok(results)
}

/// The pinned twin of [`classify_sub_call`]: a revert is an outcome, every other
/// failure (including a lost capability) is the transport fault it is.
fn classify_pinned_sub_call(
    result: Result<Bytes, ExactReadError>,
) -> Result<MulticallOutcome, ExactReadError> {
    match result {
        Ok(return_data) => Ok(MulticallOutcome::Returned(return_data)),
        Err(ExactReadError::Failed {
            cause: ProviderError::ExecutionReverted { .. },
            ..
        }) => Ok(MulticallOutcome::Reverted(Bytes::new())),
        Err(error) => Err(error),
    }
}

/// Name one sequential sub-call's result: a revert is: a revert is
/// [`MulticallOutcome::Reverted`] (payload empty — a provider revert carries its
/// reason in the error message, not as return data), and every other failure
/// propagates as the transport fault it is.
///
/// This classification is the module's whole point: with it deleted, an
/// exhausted retry budget or a dropped connection reads exactly like a revert,
/// and every consumer downstream reconciles an outage as chain state.
fn classify_sub_call(result: ProviderResult<Bytes>) -> ProviderResult<MulticallOutcome> {
    match result {
        Ok(return_data) => Ok(MulticallOutcome::Returned(return_data)),
        Err(ProviderError::ExecutionReverted { .. }) => {
            Ok(MulticallOutcome::Reverted(Bytes::new()))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
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

    /// A well-formed `(bool,bytes)[]` carrying fewer results than the batch
    /// requested is a hard decode error — never padded with all-reverted
    /// rows — so the batch caller degrades to its sequential fallback
    /// instead of masking a miscounting return.
    #[test]
    fn decode_short_result_array_is_a_hard_error() {
        let data = Bytes::from(REFERENCE_RETURNDATA_2_RESULTS.to_vec());
        let result = decode_aggregate3_results(&data, 3);
        assert!(
            result.is_err(),
            "a short result array must surface as a decode error"
        );
    }

    /// A well-formed `(bool,bytes)[]` carrying more results than the batch
    /// requested is a hard decode error — never truncated to the first `n` —
    /// so a misaligned return can never silently shift answers between calls.
    #[test]
    fn decode_long_result_array_is_a_hard_error() {
        let data = Bytes::from(REFERENCE_RETURNDATA_2_RESULTS.to_vec());
        let result = decode_aggregate3_results(&data, 1);
        assert!(
            result.is_err(),
            "a long result array must surface as a decode error"
        );
    }

    // ── the batch boundary's outcome classification ───────────────────────

    /// One scripted `eth_call` transport: the Multicall3 batch address answers
    /// `batch`, every other address answers from `answers`. Failing the batch
    /// leg alone is what makes the sequential fallback's classification
    /// observable.
    struct Scripted {
        batch: ProviderResult<Bytes>,
        answers: Vec<(Address, ProviderResult<Bytes>)>,
    }

    impl Scripted {
        fn answer(&self, to: Address) -> ProviderResult<Bytes> {
            if to == MULTICALL3_ADDRESS {
                return self.batch.clone();
            }
            match self.answers.iter().find(|(address, _)| *address == to) {
                Some((_, answer)) => answer.clone(),
                None => Err(ProviderError::ConnectionFailed {
                    message: format!("no scripted answer for {to}"),
                }),
            }
        }
    }

    impl ContractRead for Scripted {
        fn call(
            &self,
            to: Address,
            _data: Bytes,
            _block: Option<u64>,
        ) -> impl std::future::Future<Output = ProviderResult<Bytes>> + Send {
            std::future::ready(self.answer(to))
        }
    }

    fn outage() -> ProviderError {
        ProviderError::Timeout {
            message: "the node stopped answering".into(),
        }
    }

    fn sub_call(tag: u8) -> (Address, Bytes) {
        (
            Address::repeat_byte(tag),
            Bytes::from(vec![tag, tag, tag, tag]),
        )
    }

    fn aggregate3_return(items: &[(bool, Vec<u8>)]) -> Bytes {
        let values: Vec<DynSolValue> = items
            .iter()
            .map(|(success, data)| {
                DynSolValue::Tuple(vec![
                    DynSolValue::Bool(*success),
                    DynSolValue::Bytes(data.clone()),
                ])
            })
            .collect();
        Bytes::from(DynSolValue::Array(values).abi_encode())
    }

    /// A transport failing every call is an ERROR, not a fan of reverted rows:
    /// "the node is gone" must never read as "every sub-call reverted".
    #[tokio::test]
    async fn a_dead_transport_is_an_error_never_a_revert_row() {
        let calls = vec![sub_call(0x11), sub_call(0x22)];
        let transport = Scripted {
            batch: Err(outage()),
            answers: vec![(calls[0].0, Err(outage())), (calls[1].0, Err(outage()))],
        };

        let outcome = multicall3_batch(&transport, &calls, Some(7)).await;

        assert!(
            matches!(
                &outcome,
                Err(ProviderError::Timeout { message }) if message.contains("stopped answering")
            ),
            "an outage must propagate as the transport fault it is, got {outcome:?}"
        );
    }

    /// With only the batch leg failing, a reverting sub-call and a returning
    /// one are named separately — the fallback leg needs no `success` flag to
    /// tell them apart.
    #[tokio::test]
    async fn a_fallback_leg_names_a_revert_and_a_return_separately() {
        let (reverting, reverting_data) = sub_call(0x33);
        let (live, live_data) = sub_call(0x44);
        let calls = vec![(reverting, reverting_data), (live, live_data)];
        let transport = Scripted {
            // No `aggregate3` at the batch address: the fallback leg runs.
            batch: Err(ProviderError::ExecutionReverted {
                code: -32000,
                message: "no aggregate3 here".into(),
            }),
            answers: vec![
                (
                    reverting,
                    Err(ProviderError::ExecutionReverted {
                        code: -32000,
                        message: "execution reverted: pool dead".into(),
                    }),
                ),
                (live, Ok(Bytes::from(vec![0x2a]))),
            ],
        };

        let rows = multicall3_batch(&transport, &calls, None).await.unwrap();

        assert_eq!(
            rows,
            vec![
                MulticallOutcome::Reverted(Bytes::new()),
                MulticallOutcome::Returned(Bytes::from(vec![0x2a])),
            ]
        );
    }

    /// The same fallback leg with one unreachable sub-call propagates that
    /// fault: a reverting sibling never turns it into a revert row.
    #[tokio::test]
    async fn a_fallback_leg_transport_fault_is_never_a_revert_row() {
        let (reverting, reverting_data) = sub_call(0x55);
        let (dead, dead_data) = sub_call(0x66);
        let calls = vec![(reverting, reverting_data), (dead, dead_data)];
        let transport = Scripted {
            batch: Err(ProviderError::ConnectionFailed {
                message: "no route to the batch address".into(),
            }),
            answers: vec![
                (
                    reverting,
                    Err(ProviderError::ExecutionReverted {
                        code: -32000,
                        message: "execution reverted".into(),
                    }),
                ),
                (dead, Err(outage())),
            ],
        };

        let outcome = multicall3_batch(&transport, &calls, None).await;

        assert!(
            matches!(&outcome, Err(ProviderError::Timeout { .. })),
            "a fallback transport fault must stay a fault, got {outcome:?}"
        );
    }

    /// A batched revert keeps the payload the wire reported, so both legs name
    /// the same class and differ only in the payload the transport exposed.
    #[tokio::test]
    async fn a_batched_revert_carries_its_payload() {
        let calls = vec![sub_call(0x77), sub_call(0x88)];
        let revert_payload = vec![0x08, 0xc3, 0x79, 0xa0];
        let transport = Scripted {
            batch: Ok(aggregate3_return(&[
                (true, vec![0x2a]),
                (false, revert_payload.clone()),
            ])),
            answers: Vec::new(),
        };

        let rows = multicall3_batch(&transport, &calls, Some(9)).await.unwrap();

        assert_eq!(
            rows,
            vec![
                MulticallOutcome::Returned(Bytes::from(vec![0x2a])),
                MulticallOutcome::Reverted(Bytes::from(revert_payload)),
            ]
        );
    }
}
