//! ERC-20 metadata reads: `name()` / `symbol()` / `decimals()` over the one
//! [`ContractRead`] call step.
//!
//! Two consumers read token metadata with genuinely different tolerances, and
//! each used to carry its own selector/decode/fallback family. This module is
//! that family's one home: [`read`] answers one token, [`read_batch`] folds an
//! ordered token list into Multicall3 wire shapes, and the closed
//! [`MetadataShape`] names which fallback contract each caller's request set
//! and decode dialect must keep. The variants are ports of established
//! consumer behavior, not a knob matrix — a quirk documented on a variant is
//! the contract, not an accident:
//!
//! - [`MetadataShape::LowerOnly`] — the lower spelling only, decoded strictly
//!   as ABI values (no `bytes32` answers), and the sequential form
//!   SHORT-CIRCUITS across fields: the first field that cannot be resolved
//!   stops the read, so later fields are never requested. The batched form is
//!   eager — every sub-call rides the one `aggregate3`.
//! - [`MetadataShape::LowerUpperFallThrough`] — both spellings, falling
//!   through to the upper spelling on a call error AND on a decode failure:
//!   a `string` answer may come back as a NUL-padded `bytes32` (NULs trimmed
//!   from both ends — an all-NUL answer is an empty string, present not
//!   absent), and a `decimals()` answer is only accepted at `u8` width, so an
//!   out-of-width word falls through too.
//! - [`MetadataShape::LowerUpperStopOnCallError`] — both spellings, but a
//!   failed call ends the field without consulting the fallback spelling;
//!   only a succeeded-but-undecodable return falls through. The `string`
//!   decode is strict about the dynamic offset and strips only trailing NULs
//!   from a `bytes32` answer (an all-NUL answer is absent).
//!
//! Results carry per-field absence and nothing else: what an absent field
//! becomes is the consumer's policy (Pool construction's UNKNOWN sentinels
//! and all-or-nothing collapse, Aave's per-field nullable cells), as is
//! numeric width beyond the per-shape decode (`u8` sentinel fallback, `i64`
//! column casts). Block pinning is likewise the caller's: pass a block to pin
//! every call to it, `None` for the transport's own latest.
//!
//! [`ContractRead`] is the only transport seam — an
//! [`AlloyProvider`](crate::provider::AlloyProvider) and a construction-side
//! handle read identically; test doubles record the exact call sequences the
//! contracts above promise.

use alloy::dyn_abi::{DynSolType, DynSolValue};
use alloy::primitives::{Address, Bytes, U256};
use degenbot_core::errors::ProviderResult;

use crate::abi::{selector, ContractRead};
use crate::multicall3::{
    decode_aggregate3_results, encode_aggregate3, multicall3_batch, MulticallResult,
    MULTICALL3_ADDRESS,
};

/// Which selector-fallback (and matching decode) contract one metadata read
/// follows. See the module docs for each variant's exact mechanics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataShape {
    /// The lower spelling only, ABI-value decode, sequential short-circuit.
    LowerOnly,
    /// Both spellings; any failure (call or decode) falls to the upper one.
    LowerUpperFallThrough,
    /// Both spellings; a call failure ends the field, a decode failure falls
    /// to the upper one.
    LowerUpperStopOnCallError,
}

/// The fields one [`read`] issues calls for. A masked-out field is never
/// requested — this is how Pool construction's partial fallback reads ask
/// for just the fields its persisted row was missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldMask {
    /// Read `name()`.
    pub name: bool,
    /// Read `symbol()`.
    pub symbol: bool,
    /// Read `decimals()`.
    pub decimals: bool,
}

impl FieldMask {
    /// Every metadata field.
    pub const ALL: Self = Self {
        name: true,
        symbol: true,
        decimals: true,
    };
}

/// One token's metadata read result: per-field absence, no policy. A field
/// the mask excluded or the shape's contract left unresolved is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenMetadata {
    /// The `name()` answer, if one resolved.
    pub name: Option<String>,
    /// The `symbol()` answer, if one resolved.
    pub symbol: Option<String>,
    /// The `decimals()` answer, if one resolved. Width is the shape's decode
    /// contract (see [`MetadataShape`]); consumers own any further narrowing.
    pub decimals: Option<u64>,
}

/// Read one token's metadata fields over sequential `eth_call`s, honoring
/// `shape`'s spelling fallback and `mask`'s field selection.
///
/// No failure escapes: a transport or decode failure is per-field absence
/// (the shape decides whether it ends the field, falls to the fallback
/// spelling, or stops the read).
pub async fn read(
    transport: &impl ContractRead,
    token: Address,
    shape: MetadataShape,
    mask: FieldMask,
    block: Option<u64>,
) -> TokenMetadata {
    let mut meta = TokenMetadata::default();
    if mask.name {
        meta.name = read_text_field(transport, token, shape, b"name()", b"NAME()", block).await;
        if shape == MetadataShape::LowerOnly && meta.name.is_none() {
            return meta;
        }
    }
    if mask.symbol {
        meta.symbol =
            read_text_field(transport, token, shape, b"symbol()", b"SYMBOL()", block).await;
        if shape == MetadataShape::LowerOnly && meta.symbol.is_none() {
            return meta;
        }
    }
    if mask.decimals {
        meta.decimals = read_uint_field(transport, token, shape, block).await;
    }
    meta
}

/// Read many tokens' metadata in ordered Multicall3 wire shapes — one
/// `aggregate3` call carrying every spelling of every field, eager — with one
/// [`TokenMetadata`] per input address, in input order.
///
/// Because the request set always carries the fallback spellings, the
/// per-field decisions (identical to [`read`]'s) may never consult some of
/// them; a recorder sees the batched shape whole. When the batch itself
/// cannot answer, the shape's own degradation applies:
/// [`MetadataShape::LowerOnly`] / [`MetadataShape::LowerUpperFallThrough`]
/// degrade to per-token [`read`] calls (the exact pre-batch request shape);
/// [`MetadataShape::LowerUpperStopOnCallError`] degrades to sequential
/// per-sub-call `eth_call`s through [`multicall3_batch`], keeping the whole
/// eager request set.
///
/// No failure escapes: a failed batch or sub-call is per-field absence under
/// the same per-field rules as [`read`].
pub async fn read_batch(
    transport: &impl ContractRead,
    tokens: &[Address],
    shape: MetadataShape,
    block: Option<u64>,
) -> Vec<TokenMetadata> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let calls: Vec<(Address, Bytes)> = tokens
        .iter()
        .flat_map(|token| sub_calls(*token, shape))
        .collect();
    match shape {
        MetadataShape::LowerOnly | MetadataShape::LowerUpperFallThrough => {
            // ONE `aggregate3` for the whole list (the construction batch's
            // wire shape — no chunking), consumed only if BOTH the call and
            // the return decode cleanly; otherwise the pre-batch per-token
            // reads keep the exact sequential behavior.
            let batch = async {
                let calldata = encode_aggregate3(&calls)?;
                let data = transport.call(MULTICALL3_ADDRESS, calldata, block).await?;
                decode_aggregate3_results(&data, calls.len())
            }
            .await;
            let Ok(results) = batch else {
                let mut out = Vec::with_capacity(tokens.len());
                for token in tokens {
                    out.push(read(transport, *token, shape, FieldMask::ALL, block).await);
                }
                return out;
            };
            decode_batch_results(shape, tokens.len(), &results)
        }
        MetadataShape::LowerUpperStopOnCallError => {
            // `multicall3_batch` degrades a failed/malformed batch to
            // sequential per-call `eth_call`s mapped to per-item `success`
            // flags; an `Err` can only mean its own encode step failed
            // (defensively unreachable) — mirror it as all-failed results,
            // which the per-field decisions turn into absent fields.
            let results = match multicall3_batch(transport, &calls, block).await {
                Ok(results) => results,
                Err(_) => calls
                    .iter()
                    .map(|_| MulticallResult {
                        success: false,
                        return_data: Bytes::new(),
                    })
                    .collect(),
            };
            decode_batch_results(shape, tokens.len(), &results)
        }
    }
}

/// Decode one name/symbol-shaped `eth_call` return: the strict ABI dynamic
/// `string` first (offset exactly `0x20`, exact length — trailing NULs are
/// data, never stripped), then the `bytes32` fallback older tokens answer
/// with (first 32 bytes, trailing NULs stripped, non-empty).
///
/// This is [`MetadataShape::LowerUpperStopOnCallError`]'s per-spelling string
/// decode; non-ERC-20 reads that answer the same shapes (the market-id read)
/// decode through the same contract.
#[must_use]
pub fn decode_string_return(ret: &[u8]) -> Option<String> {
    if let Some(s) = decode_dynamic_string(ret) {
        return Some(s);
    }
    if ret.len() >= 32 {
        let s = String::from_utf8_lossy(&ret[..32])
            .trim_end_matches('\0')
            .to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

/// The strict dynamic-`string` decode: a 32-byte offset of exactly `0x20`,
/// the length word, then the data. The length is exact, so trailing NULs
/// inside it are preserved; a malformed shape is `None`. The length word is
/// untrusted: one that overflows the `usize` conversion or the `64 + length`
/// bounds sum is `None`, never a truncated or wrapped bound.
fn decode_dynamic_string(ret: &[u8]) -> Option<String> {
    if ret.len() < 64 {
        return None;
    }
    let offset = U256::from_be_slice(&ret[..32]);
    if offset != U256::from(0x20u32) {
        return None;
    }
    let length = usize::try_from(U256::from_be_slice(&ret[32..64])).ok()?;
    let end = 64usize.checked_add(length)?;
    if end > ret.len() {
        return None;
    }
    let s = String::from_utf8_lossy(&ret[64..end]).into_owned();
    Some(s)
}

/// [`MetadataShape::LowerOnly`]'s string decode: a Solidity `string` and
/// nothing else (no `bytes32` answers — an unshaped return is absent).
fn decode_text_declared(ret: &[u8]) -> Option<String> {
    match DynSolType::String.abi_decode(ret) {
        Ok(DynSolValue::String(value)) => Some(value),
        _ => None,
    }
}

/// [`MetadataShape::LowerUpperFallThrough`]'s string decode: a Solidity
/// `string`, else a UTF-8 `bytes32` with NULs trimmed from both ends (an
/// all-NUL answer decodes as an empty string — present, not absent).
fn decode_text_or_bytes32(ret: &[u8]) -> Option<String> {
    if let Ok(DynSolValue::String(s)) = DynSolType::String.abi_decode(ret) {
        return Some(s);
    }
    if let Ok(DynSolValue::FixedBytes(fb, _)) = DynSolType::FixedBytes(32).abi_decode(ret) {
        return Some(
            String::from_utf8_lossy(fb.as_slice())
                .trim_matches('\0')
                .to_string(),
        );
    }
    None
}

/// [`MetadataShape::LowerOnly`]'s decimals decode: the return must ABI-decode
/// as a `uint256` that fits `u64` — an out-of-width answer is this shape's
/// decode absence, never a truncated low word.
fn decode_uint_declared(ret: &[u8]) -> Option<u64> {
    match DynSolType::Uint(256).abi_decode(ret) {
        Ok(DynSolValue::Uint(n, _)) => u64::try_from(n).ok(),
        _ => None,
    }
}

/// The blind first-word read the fallback shapes decode `decimals()` with:
/// any 32-byte word that fits `u64` — an out-of-width word is this decode's
/// absence (falling through exactly like an unshaped return), never a
/// truncated low word.
fn decode_uint_word(ret: &[u8]) -> Option<u64> {
    if ret.len() < 32 {
        return None;
    }
    u64::try_from(U256::from_be_slice(&ret[..32])).ok()
}

/// [`MetadataShape::LowerUpperFallThrough`]'s decimals decode: the first
/// word, accepted only at `u8` width — an out-of-width word is this
/// spelling's decode failure and falls through like any other.
fn decode_uint_word_u8(ret: &[u8]) -> Option<u64> {
    let value = decode_uint_word(ret)?;
    u8::try_from(value).ok().map(u64::from)
}

/// One field's sequential spelling walk (`lower_sig`, then `upper_sig`),
/// under `shape`'s failure contract. The decode dialect rides the shape (see
/// the module docs).
async fn read_text_field(
    transport: &impl ContractRead,
    token: Address,
    shape: MetadataShape,
    lower_sig: &'static [u8],
    upper_sig: &'static [u8],
    block: Option<u64>,
) -> Option<String> {
    match shape {
        MetadataShape::LowerOnly => {
            let bytes = call_getter(transport, token, lower_sig, block).await.ok()?;
            decode_text_declared(&bytes)
        }
        MetadataShape::LowerUpperFallThrough => {
            for sig in [lower_sig, upper_sig] {
                if let Ok(bytes) = call_getter(transport, token, sig, block).await {
                    if let Some(value) = decode_text_or_bytes32(&bytes) {
                        return Some(value);
                    }
                }
            }
            None
        }
        MetadataShape::LowerUpperStopOnCallError => {
            for sig in [lower_sig, upper_sig] {
                let bytes = call_getter(transport, token, sig, block).await.ok()?;
                if let Some(value) = decode_string_return(&bytes) {
                    return Some(value);
                }
            }
            None
        }
    }
}

/// The `decimals()` field's sequential spelling walk (`decimals()` /
/// `DECIMALS()`), under `shape`'s failure contract.
async fn read_uint_field(
    transport: &impl ContractRead,
    token: Address,
    shape: MetadataShape,
    block: Option<u64>,
) -> Option<u64> {
    match shape {
        MetadataShape::LowerOnly => {
            let bytes = call_getter(transport, token, b"decimals()", block)
                .await
                .ok()?;
            decode_uint_declared(&bytes)
        }
        MetadataShape::LowerUpperFallThrough => {
            for sig in [b"decimals()" as &[u8], b"DECIMALS()"] {
                if let Ok(bytes) = call_getter(transport, token, sig, block).await {
                    if let Some(value) = decode_uint_word_u8(&bytes) {
                        return Some(value);
                    }
                }
            }
            None
        }
        MetadataShape::LowerUpperStopOnCallError => {
            for sig in [b"decimals()" as &[u8], b"DECIMALS()"] {
                let bytes = call_getter(transport, token, sig, block).await.ok()?;
                if let Some(value) = decode_uint_word(&bytes) {
                    return Some(value);
                }
            }
            None
        }
    }
}

/// One no-argument getter call through the one call step.
async fn call_getter(
    transport: &impl ContractRead,
    token: Address,
    signature: &[u8],
    block: Option<u64>,
) -> ProviderResult<Bytes> {
    transport
        .call(token, Bytes::copy_from_slice(&selector(signature)), block)
        .await
}

/// The sub-call sequence one token contributes to a batch: every field's
/// spellings in field order — the lower-only shape issues just the lower
/// spellings, the fallback shapes carry both (eager).
fn sub_calls(token: Address, shape: MetadataShape) -> Vec<(Address, Bytes)> {
    let sigs: &[&[u8]] = match shape {
        MetadataShape::LowerOnly => &[b"name()", b"symbol()", b"decimals()"],
        MetadataShape::LowerUpperFallThrough | MetadataShape::LowerUpperStopOnCallError => &[
            b"name()",
            b"NAME()",
            b"symbol()",
            b"SYMBOL()",
            b"decimals()",
            b"DECIMALS()",
        ],
    };
    sigs.iter()
        .map(|sig| (token, Bytes::copy_from_slice(&selector(sig))))
        .collect()
}

/// Fold one batch's sub-call results into per-token [`TokenMetadata`]s under
/// `shape`'s per-field decision — the exact sequential order (which failure
/// ends the field, and when the fallback spelling is consulted, is the
/// shape's contract). The callers carry exactly one whole token's eager
/// request set per chunk (the strict batch decode refuses any other result
/// count), so every chunk folds to exactly one [`TokenMetadata`].
fn decode_batch_results(
    shape: MetadataShape,
    token_count: usize,
    results: &[MulticallResult],
) -> Vec<TokenMetadata> {
    let per_token = match shape {
        MetadataShape::LowerOnly => 3,
        MetadataShape::LowerUpperFallThrough | MetadataShape::LowerUpperStopOnCallError => 6,
    };
    let mut out = Vec::with_capacity(token_count);
    for chunk in results.chunks_exact(per_token) {
        let (name, symbol, decimals) = match shape {
            MetadataShape::LowerOnly => (
                single_text(&chunk[0], decode_text_declared),
                single_text(&chunk[1], decode_text_declared),
                single_uint(&chunk[2], decode_uint_declared),
            ),
            MetadataShape::LowerUpperFallThrough => (
                fall_through_text(&chunk[0], &chunk[1]),
                fall_through_text(&chunk[2], &chunk[3]),
                fall_through_uint(&chunk[4], &chunk[5]),
            ),
            MetadataShape::LowerUpperStopOnCallError => (
                stop_on_call_error_text(&chunk[0], &chunk[1]),
                stop_on_call_error_text(&chunk[2], &chunk[3]),
                stop_on_call_error_uint(&chunk[4], &chunk[5]),
            ),
        };
        out.push(TokenMetadata {
            name,
            symbol,
            decimals,
        });
    }
    out
}

/// One eager sub-call's answer under the lower-only rule: a failed sub-call
/// is absence, a decoded answer is the value.
fn single_text(res: &MulticallResult, decode: fn(&[u8]) -> Option<String>) -> Option<String> {
    if res.success {
        decode(&res.return_data)
    } else {
        None
    }
}

/// [`single_text`]'s decimals twin.
fn single_uint(res: &MulticallResult, decode: fn(&[u8]) -> Option<u64>) -> Option<u64> {
    if res.success {
        decode(&res.return_data)
    } else {
        None
    }
}

/// The fall-through pair decision: the first spelling whose sub-call
/// succeeded AND decodes wins — a failed sub-call falls through exactly like
/// the sequential shape's call error.
fn fall_through_text(lower: &MulticallResult, upper: &MulticallResult) -> Option<String> {
    for res in [lower, upper] {
        if let Some(value) = single_text(res, decode_text_or_bytes32) {
            return Some(value);
        }
    }
    None
}

/// [`fall_through_text`]'s decimals twin (the `u8`-width decode included).
fn fall_through_uint(lower: &MulticallResult, upper: &MulticallResult) -> Option<u64> {
    for res in [lower, upper] {
        if let Some(value) = single_uint(res, decode_uint_word_u8) {
            return Some(value);
        }
    }
    None
}

/// The stop-on-call-error pair decision — the exact sequential order: a
/// failed lower result returns absence WITHOUT consulting the upper spelling
/// (the sequential call-error early return); a succeeded-but-undecodable
/// lower return falls through to the upper spelling.
fn stop_on_call_error_text(lower: &MulticallResult, upper: &MulticallResult) -> Option<String> {
    if !lower.success {
        return None;
    }
    decode_string_return(&lower.return_data).or_else(|| {
        if !upper.success {
            return None;
        }
        decode_string_return(&upper.return_data)
    })
}

/// [`stop_on_call_error_text`]'s decimals twin.
fn stop_on_call_error_uint(lower: &MulticallResult, upper: &MulticallResult) -> Option<u64> {
    if !lower.success {
        return None;
    }
    decode_uint_word(&lower.return_data).or_else(|| {
        if !upper.success {
            return None;
        }
        decode_uint_word(&upper.return_data)
    })
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;
    use std::future::Future;
    use std::sync::Mutex;

    use alloy::primitives::address;
    use degenbot_core::errors::ProviderError;

    use super::*;
    use crate::multicall3::AGGREGATE3_SELECTOR;

    /// The canned-answer table: one result (or failure) per `(target,
    /// selector)`.
    type Answers = HashMap<(Address, [u8; 4]), Result<Vec<u8>, ProviderError>>;

    const TOKEN_A: Address = address!("11111111111111111111111111111111111111a1");
    const TOKEN_B: Address = address!("22222222222222222222222222222222222222b2");
    const PIN: Option<u64> = Some(21_000_000);

    /// A [`ContractRead`] double keyed on `(target, selector)`: every wire
    /// call and every decoded Multicall3 sub-call is recorded, and one
    /// `aggregate3` batch is answered from the same per-sub-call table (a
    /// missing or failing sub-call is a `success = false` item). Flipping
    /// `fail_batch` makes the batch call itself fail, which is where each
    /// shape's degradation contract becomes observable; `short_batch` serves
    /// an `aggregate3` return missing its last result, which is where the
    /// cardinality decode's degradation becomes observable.
    #[derive(Default)]
    struct Chain {
        answers: Mutex<Answers>,
        calls: Mutex<Vec<(Address, Bytes, Option<u64>)>>,
        sub_calls: Mutex<Vec<(Address, Bytes)>>,
        fail_batch: bool,
        short_batch: bool,
    }

    impl Chain {
        fn new() -> Self {
            Self::default()
        }

        fn answer(self, token: Address, signature: &[u8], bytes: Vec<u8>) -> Self {
            self.answers
                .lock()
                .unwrap()
                .insert((token, selector(signature)), Ok(bytes));
            self
        }

        fn fail(self, token: Address, signature: &[u8], error: ProviderError) -> Self {
            self.answers
                .lock()
                .unwrap()
                .insert((token, selector(signature)), Err(error));
            self
        }

        fn failing_batch(self) -> Self {
            Self {
                fail_batch: true,
                ..self
            }
        }

        fn short_batch(self) -> Self {
            Self {
                short_batch: true,
                ..self
            }
        }

        fn lookup(&self, token: Address, data: &[u8]) -> Result<Vec<u8>, ProviderError> {
            let sel = first_four(data);
            self.answers
                .lock()
                .unwrap()
                .get(&(token, sel))
                .cloned()
                .unwrap_or_else(|| {
                    Err(ProviderError::ExecutionReverted {
                        code: -32000,
                        message: "no canned answer".into(),
                    })
                })
        }
    }

    impl Chain {
        fn dispatch(&self, to: Address, data: &Bytes, block: Option<u64>) -> ProviderResult<Bytes> {
            self.calls.lock().unwrap().push((to, data.clone(), block));
            if to == MULTICALL3_ADDRESS && first_four(data) == AGGREGATE3_SELECTOR {
                if self.fail_batch {
                    return Err(ProviderError::ExecutionReverted {
                        code: -32000,
                        message: "no aggregate3 here".into(),
                    });
                }
                let mut items = Vec::new();
                for (target, sub) in decode_sub_calls(data) {
                    self.sub_calls.lock().unwrap().push((target, sub.clone()));
                    match self.lookup(target, &sub) {
                        Ok(bytes) => items.push((true, bytes)),
                        Err(_) => items.push((false, Vec::new())),
                    }
                }
                if self.short_batch {
                    items.pop();
                }
                return Ok(Bytes::from(encode_batch_results(&items)));
            }
            self.lookup(to, data).map(Bytes::from)
        }
    }

    impl ContractRead for Chain {
        fn call(
            &self,
            to: Address,
            data: Bytes,
            block: Option<u64>,
        ) -> impl Future<Output = ProviderResult<Bytes>> + Send {
            std::future::ready(self.dispatch(to, &data, block))
        }
    }

    fn first_four(data: &[u8]) -> [u8; 4] {
        let mut sel = [0u8; 4];
        sel.copy_from_slice(&data[..4]);
        sel
    }

    /// The wire calls recorded so far: `(target, selector, block)`.
    fn wire(chain: &Chain) -> Vec<(Address, [u8; 4], Option<u64>)> {
        chain
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(to, data, block)| (*to, first_four(data), *block))
            .collect()
    }

    /// The Multicall3 sub-calls recorded so far, in wire order.
    fn subs(chain: &Chain) -> Vec<(Address, [u8; 4])> {
        chain
            .sub_calls
            .lock()
            .unwrap()
            .iter()
            .map(|(to, data)| (*to, first_four(data)))
            .collect()
    }

    fn revert() -> ProviderError {
        ProviderError::ExecutionReverted {
            code: -32000,
            message: "revert".into(),
        }
    }

    /// A canonical ABI dynamic-`string` return.
    fn str_ret(s: &str) -> Vec<u8> {
        let n = s.len();
        let mut v = U256::from(32u64).to_be_bytes::<32>().to_vec();
        v.extend_from_slice(&U256::from(n).to_be_bytes::<32>());
        v.extend_from_slice(s.as_bytes());
        v.resize(v.len() + ((32 - n % 32) % 32), 0u8);
        v
    }

    /// A single-word `uint` return.
    fn word_ret(v: u64) -> Vec<u8> {
        U256::from(v).to_be_bytes::<32>().to_vec()
    }

    /// A single-word `uint` return from a full-width `U256` value.
    fn big_word_ret(v: U256) -> Vec<u8> {
        v.to_be_bytes::<32>().to_vec()
    }

    /// A left-aligned `bytes32` answer, NUL-padded.
    fn bytes32_ret(s: &str) -> Vec<u8> {
        let mut word = vec![0u8; 32];
        word[..s.len()].copy_from_slice(s.as_bytes());
        word
    }

    /// Decode one recorded `aggregate3` calldata into its `(target,
    /// sub-calldata)` pairs.
    fn decode_sub_calls(data: &[u8]) -> Vec<(Address, Bytes)> {
        let ty: DynSolType = "(bool,address,bytes)[]".parse().unwrap();
        let decoded = ty.abi_decode(&data[4..]).unwrap();
        let DynSolValue::Array(items) = decoded else {
            panic!("aggregate3 params decode to an array");
        };
        items
            .into_iter()
            .map(|item| {
                let DynSolValue::Tuple(fields) = item else {
                    panic!("each aggregate3 item is a tuple");
                };
                let (DynSolValue::Address(target), DynSolValue::Bytes(sub)) =
                    (&fields[1], &fields[2])
                else {
                    panic!("each aggregate3 item is (bool,address,bytes)");
                };
                (*target, Bytes::from(sub.clone()))
            })
            .collect()
    }

    /// The `(bool,bytes)[]` return an `aggregate3` batch answers with.
    fn encode_batch_results(items: &[(bool, Vec<u8>)]) -> Vec<u8> {
        let tuples: Vec<DynSolValue> = items
            .iter()
            .map(|(success, data)| {
                DynSolValue::Tuple(vec![
                    DynSolValue::Bool(*success),
                    DynSolValue::Bytes(data.clone()),
                ])
            })
            .collect();
        DynSolValue::Array(tuples).abi_encode()
    }

    // ── sequential read: exact call sequences per shape ───────────────────

    #[tokio::test]
    async fn read_lower_only_orders_name_symbol_decimals_at_the_pinned_block() {
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Dai Stablecoin"))
            .answer(TOKEN_A, b"symbol()", str_ret("DAI"))
            .answer(TOKEN_A, b"decimals()", word_ret(18));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerOnly,
            FieldMask::ALL,
            PIN,
        )
        .await;
        assert_eq!(
            meta,
            TokenMetadata {
                name: Some("Dai Stablecoin".into()),
                symbol: Some("DAI".into()),
                decimals: Some(18),
            }
        );
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"name()"), PIN),
                (TOKEN_A, selector(b"symbol()"), PIN),
                (TOKEN_A, selector(b"decimals()"), PIN),
            ]
        );
    }

    #[tokio::test]
    async fn read_lower_only_short_circuits_at_the_first_absent_field() {
        // A reverting `name()` stops the read: symbol()/decimals() are never
        // requested.
        let chain = Chain::new();
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerOnly,
            FieldMask::ALL,
            None,
        )
        .await;
        assert_eq!(meta, TokenMetadata::default());
        assert_eq!(wire(&chain), vec![(TOKEN_A, selector(b"name()"), None)]);

        // An undecodable `symbol()` stops the read before decimals().
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Dai"))
            .answer(TOKEN_A, b"symbol()", vec![0u8; 16]);
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerOnly,
            FieldMask::ALL,
            None,
        )
        .await;
        assert_eq!(meta.name.as_deref(), Some("Dai"));
        assert_eq!(meta.symbol, None);
        assert_eq!(meta.decimals, None);
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"name()"), None),
                (TOKEN_A, selector(b"symbol()"), None),
            ]
        );
    }

    #[tokio::test]
    async fn read_lower_only_has_no_bytes32_answer_fallback() {
        // A `bytes32`-shaped name() answer is absence for this shape (the
        // strict ABI-value decode), and absence short-circuits the read.
        let chain = Chain::new().answer(TOKEN_A, b"name()", bytes32_ret("WETH"));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerOnly,
            FieldMask::ALL,
            None,
        )
        .await;
        assert_eq!(meta, TokenMetadata::default());
        assert_eq!(wire(&chain), vec![(TOKEN_A, selector(b"name()"), None)]);
    }

    #[tokio::test]
    async fn read_fall_through_falls_on_both_call_and_decode_errors() {
        // name() reverts -> NAME(); symbol() answers unshaped -> SYMBOL()
        // answers bytes32; decimals() answers out of u8 width -> DECIMALS().
        let chain = Chain::new()
            .fail(TOKEN_A, b"name()", revert())
            .answer(TOKEN_A, b"NAME()", str_ret("Maker"))
            .answer(TOKEN_A, b"symbol()", vec![0u8; 16])
            .answer(TOKEN_A, b"SYMBOL()", bytes32_ret("MKR"))
            .answer(TOKEN_A, b"decimals()", word_ret(300))
            .answer(TOKEN_A, b"DECIMALS()", word_ret(6));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperFallThrough,
            FieldMask::ALL,
            None,
        )
        .await;
        assert_eq!(
            meta,
            TokenMetadata {
                name: Some("Maker".into()),
                symbol: Some("MKR".into()),
                decimals: Some(6),
            }
        );
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"name()"), None),
                (TOKEN_A, selector(b"NAME()"), None),
                (TOKEN_A, selector(b"symbol()"), None),
                (TOKEN_A, selector(b"SYMBOL()"), None),
                (TOKEN_A, selector(b"decimals()"), None),
                (TOKEN_A, selector(b"DECIMALS()"), None),
            ]
        );
    }

    #[tokio::test]
    async fn read_fall_through_keeps_the_all_nul_bytes32_as_an_empty_string() {
        // The bytes32 arm trims NULs from both ends and accepts the empty
        // result — an all-NUL answer is a PRESENT empty name, so NAME() is
        // never consulted.
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", vec![0u8; 32])
            .answer(TOKEN_A, b"NAME()", str_ret("Maker"));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperFallThrough,
            FieldMask {
                name: true,
                symbol: false,
                decimals: false,
            },
            None,
        )
        .await;
        assert_eq!(meta.name.as_deref(), Some(""));
        assert_eq!(wire(&chain), vec![(TOKEN_A, selector(b"name()"), None)]);
    }

    #[tokio::test]
    async fn read_fall_through_decimals_width_falls_to_the_upper_spelling() {
        // Both spellings answer out of u8 width -> the field is absent (the
        // consumer's sentinel policy is not this read's).
        let chain = Chain::new()
            .answer(TOKEN_A, b"decimals()", word_ret(300))
            .answer(TOKEN_A, b"DECIMALS()", word_ret(70_000));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperFallThrough,
            FieldMask {
                name: false,
                symbol: false,
                decimals: true,
            },
            None,
        )
        .await;
        assert_eq!(meta.decimals, None);
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"decimals()"), None),
                (TOKEN_A, selector(b"DECIMALS()"), None),
            ]
        );
    }

    #[tokio::test]
    async fn read_stop_on_call_error_ends_a_field_on_a_call_error() {
        // name() reverts -> the field is absent WITHOUT a NAME() call; the
        // fields stay independent, so symbol()/decimals() still read.
        let chain = Chain::new()
            .fail(TOKEN_A, b"name()", revert())
            .answer(TOKEN_A, b"NAME()", str_ret("Maker"))
            .answer(TOKEN_A, b"symbol()", str_ret("MKR"))
            .answer(TOKEN_A, b"decimals()", word_ret(18));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperStopOnCallError,
            FieldMask::ALL,
            PIN,
        )
        .await;
        assert_eq!(meta.name, None);
        assert_eq!(meta.symbol.as_deref(), Some("MKR"));
        assert_eq!(meta.decimals, Some(18));
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"name()"), PIN),
                (TOKEN_A, selector(b"symbol()"), PIN),
                (TOKEN_A, selector(b"decimals()"), PIN),
            ]
        );
    }

    #[tokio::test]
    async fn read_stop_on_call_error_falls_only_on_a_decode_failure() {
        // An unshaped lower answer falls to the upper spelling; an all-NUL
        // bytes32 is absent for this shape (trailing-NULs-only strip).
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", vec![0u8; 16])
            .answer(TOKEN_A, b"NAME()", str_ret("Maker"))
            .answer(TOKEN_A, b"symbol()", vec![0u8; 32])
            .answer(TOKEN_A, b"SYMBOL()", str_ret("MKR"))
            .fail(TOKEN_A, b"decimals()", revert())
            .answer(TOKEN_A, b"DECIMALS()", word_ret(6));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperStopOnCallError,
            FieldMask::ALL,
            None,
        )
        .await;
        assert_eq!(meta.name.as_deref(), Some("Maker"));
        assert_eq!(meta.symbol.as_deref(), Some("MKR"));
        assert_eq!(meta.decimals, None);
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"name()"), None),
                (TOKEN_A, selector(b"NAME()"), None),
                (TOKEN_A, selector(b"symbol()"), None),
                (TOKEN_A, selector(b"SYMBOL()"), None),
                (TOKEN_A, selector(b"decimals()"), None),
            ]
        );
    }

    #[tokio::test]
    async fn read_mask_selects_the_requested_fields_in_canonical_order() {
        // A partial mask (Pool construction's fallback reads) issues only the
        // missing fields' spellings, in field order.
        let chain = Chain::new()
            .answer(TOKEN_A, b"symbol()", str_ret("MKR"))
            .answer(TOKEN_A, b"decimals()", word_ret(18))
            .answer(TOKEN_A, b"DECIMALS()", word_ret(18));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperFallThrough,
            FieldMask {
                name: false,
                symbol: true,
                decimals: true,
            },
            None,
        )
        .await;
        assert_eq!(meta.name, None);
        assert_eq!(meta.symbol.as_deref(), Some("MKR"));
        assert_eq!(meta.decimals, Some(18));
        assert_eq!(
            wire(&chain),
            vec![
                (TOKEN_A, selector(b"symbol()"), None),
                (TOKEN_A, selector(b"decimals()"), None),
            ]
        );
    }

    // ── batched read: eager request sets and both degradation shapes ─────

    #[tokio::test]
    async fn read_batch_lower_only_is_eager_three_sub_calls_per_token() {
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .answer(TOKEN_A, b"symbol()", str_ret("ALPHA"))
            .answer(TOKEN_A, b"decimals()", word_ret(6))
            .answer(TOKEN_B, b"name()", str_ret("Beta"))
            .answer(TOKEN_B, b"symbol()", str_ret("BETA"))
            .answer(TOKEN_B, b"decimals()", word_ret(18));
        let metas = read_batch(&chain, &[TOKEN_A, TOKEN_B], MetadataShape::LowerOnly, None).await;
        assert_eq!(
            metas,
            vec![
                TokenMetadata {
                    name: Some("Alpha".into()),
                    symbol: Some("ALPHA".into()),
                    decimals: Some(6),
                },
                TokenMetadata {
                    name: Some("Beta".into()),
                    symbol: Some("BETA".into()),
                    decimals: Some(18),
                },
            ]
        );
        // ONE aggregate3 call for the whole ordered list, unpinned.
        assert_eq!(
            wire(&chain),
            vec![(MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, None)]
        );
        assert_eq!(
            subs(&chain),
            vec![
                (TOKEN_A, selector(b"name()")),
                (TOKEN_A, selector(b"symbol()")),
                (TOKEN_A, selector(b"decimals()")),
                (TOKEN_B, selector(b"name()")),
                (TOKEN_B, selector(b"symbol()")),
                (TOKEN_B, selector(b"decimals()")),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_lower_only_reports_per_field_absence() {
        // A token whose symbol() sub-call reverts keeps its other fields —
        // the all-or-nothing collapse is the consumer's policy, not this
        // read's.
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .answer(TOKEN_A, b"decimals()", word_ret(6));
        let metas = read_batch(&chain, &[TOKEN_A], MetadataShape::LowerOnly, None).await;
        assert_eq!(
            metas[0],
            TokenMetadata {
                name: Some("Alpha".into()),
                symbol: None,
                decimals: Some(6),
            }
        );
    }

    #[tokio::test]
    async fn read_batch_lower_only_degrades_to_per_token_sequential_reads() {
        // The batch call fails: each token falls back to its exact pre-batch
        // sequential shape — including the short-circuit (TOKEN_B's
        // decimals() is never requested once symbol() is absent).
        let chain = Chain::new()
            .failing_batch()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .answer(TOKEN_A, b"symbol()", str_ret("ALPHA"))
            .answer(TOKEN_A, b"decimals()", word_ret(6))
            .answer(TOKEN_B, b"name()", str_ret("Beta"));
        let metas = read_batch(&chain, &[TOKEN_A, TOKEN_B], MetadataShape::LowerOnly, PIN).await;
        assert_eq!(metas[0].symbol.as_deref(), Some("ALPHA"));
        assert_eq!(metas[1].name.as_deref(), Some("Beta"));
        assert_eq!(metas[1].symbol, None);
        assert_eq!(metas[1].decimals, None);
        assert_eq!(
            wire(&chain),
            vec![
                (MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, PIN),
                (TOKEN_A, selector(b"name()"), PIN),
                (TOKEN_A, selector(b"symbol()"), PIN),
                (TOKEN_A, selector(b"decimals()"), PIN),
                (TOKEN_B, selector(b"name()"), PIN),
                (TOKEN_B, selector(b"symbol()"), PIN),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_short_aggregate_return_degrades_to_per_token_sequential_reads() {
        // The batched `aggregate3` return carries one result fewer than the
        // request: the strict cardinality decode refuses it, and each token
        // falls back to its exact pre-batch sequential shape — never padded
        // default metadata.
        let chain = Chain::new()
            .short_batch()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .answer(TOKEN_A, b"symbol()", str_ret("ALPHA"))
            .answer(TOKEN_A, b"decimals()", word_ret(6))
            .answer(TOKEN_B, b"name()", str_ret("Beta"))
            .answer(TOKEN_B, b"symbol()", str_ret("BETA"))
            .answer(TOKEN_B, b"decimals()", word_ret(18));

        let metas = read_batch(&chain, &[TOKEN_A, TOKEN_B], MetadataShape::LowerOnly, None).await;

        assert_eq!(
            metas,
            vec![
                TokenMetadata {
                    name: Some("Alpha".into()),
                    symbol: Some("ALPHA".into()),
                    decimals: Some(6),
                },
                TokenMetadata {
                    name: Some("Beta".into()),
                    symbol: Some("BETA".into()),
                    decimals: Some(18),
                },
            ]
        );
        assert_eq!(
            wire(&chain),
            vec![
                (MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, None),
                (TOKEN_A, selector(b"name()"), None),
                (TOKEN_A, selector(b"symbol()"), None),
                (TOKEN_A, selector(b"decimals()"), None),
                (TOKEN_B, selector(b"name()"), None),
                (TOKEN_B, selector(b"symbol()"), None),
                (TOKEN_B, selector(b"decimals()"), None),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_fall_through_folds_six_sub_calls_and_falls_per_spelling() {
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .fail(TOKEN_A, b"symbol()", revert())
            .answer(TOKEN_A, b"SYMBOL()", str_ret("ALPHA"))
            .answer(TOKEN_A, b"decimals()", word_ret(300))
            .answer(TOKEN_A, b"DECIMALS()", word_ret(6));
        let metas = read_batch(
            &chain,
            &[TOKEN_A],
            MetadataShape::LowerUpperFallThrough,
            PIN,
        )
        .await;
        assert_eq!(
            metas[0],
            TokenMetadata {
                name: Some("Alpha".into()),
                symbol: Some("ALPHA".into()),
                decimals: Some(6),
            }
        );
        assert_eq!(
            wire(&chain),
            vec![(MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, PIN)]
        );
        assert_eq!(
            subs(&chain),
            vec![
                (TOKEN_A, selector(b"name()")),
                (TOKEN_A, selector(b"NAME()")),
                (TOKEN_A, selector(b"symbol()")),
                (TOKEN_A, selector(b"SYMBOL()")),
                (TOKEN_A, selector(b"decimals()")),
                (TOKEN_A, selector(b"DECIMALS()")),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_fall_through_degrades_to_per_token_sequential_reads() {
        let chain = Chain::new()
            .failing_batch()
            .fail(TOKEN_A, b"name()", revert())
            .answer(TOKEN_A, b"NAME()", str_ret("Alpha"));
        let metas = read_batch(
            &chain,
            &[TOKEN_A],
            MetadataShape::LowerUpperFallThrough,
            None,
        )
        .await;
        assert_eq!(metas[0].name.as_deref(), Some("Alpha"));
        // The degraded per-token read is the exact sequential fall-through
        // shape: name() -> NAME(), then the absent fields' own walks.
        assert_eq!(
            wire(&chain),
            vec![
                (MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, None),
                (TOKEN_A, selector(b"name()"), None),
                (TOKEN_A, selector(b"NAME()"), None),
                (TOKEN_A, selector(b"symbol()"), None),
                (TOKEN_A, selector(b"SYMBOL()"), None),
                (TOKEN_A, selector(b"decimals()"), None),
                (TOKEN_A, selector(b"DECIMALS()"), None),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_stop_on_call_error_is_eager_six_sub_calls_per_token() {
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .answer(TOKEN_A, b"symbol()", str_ret("ALPHA"))
            .answer(TOKEN_A, b"decimals()", word_ret(6))
            .answer(TOKEN_B, b"name()", str_ret("Beta"))
            .answer(TOKEN_B, b"symbol()", str_ret("BETA"))
            .answer(TOKEN_B, b"decimals()", word_ret(18));
        let metas = read_batch(
            &chain,
            &[TOKEN_A, TOKEN_B],
            MetadataShape::LowerUpperStopOnCallError,
            PIN,
        )
        .await;
        assert_eq!(metas[0].name.as_deref(), Some("Alpha"));
        assert_eq!(metas[1].decimals, Some(18));
        // ONE pinned aggregate3 call carrying BOTH spellings of every field
        // per token — the fallback spellings ride along even though the
        // lower answers decoded.
        assert_eq!(
            wire(&chain),
            vec![(MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, PIN)]
        );
        assert_eq!(
            subs(&chain),
            vec![
                (TOKEN_A, selector(b"name()")),
                (TOKEN_A, selector(b"NAME()")),
                (TOKEN_A, selector(b"symbol()")),
                (TOKEN_A, selector(b"SYMBOL()")),
                (TOKEN_A, selector(b"decimals()")),
                (TOKEN_A, selector(b"DECIMALS()")),
                (TOKEN_B, selector(b"name()")),
                (TOKEN_B, selector(b"NAME()")),
                (TOKEN_B, selector(b"symbol()")),
                (TOKEN_B, selector(b"SYMBOL()")),
                (TOKEN_B, selector(b"decimals()")),
                (TOKEN_B, selector(b"DECIMALS()")),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_stop_on_call_error_decisions_mirror_the_sequential_matrix() {
        // Every spelling rides the batch; the decisions consume the results
        // in the sequential order's exact shape:
        //   name:     failed lower -> absent WITHOUT the upper answer;
        //   symbol:   undecodable lower -> the upper spelling answers;
        //   decimals: the lower word answers directly (the upper rides
        //             along, unconsumed).
        let chain = Chain::new()
            .fail(TOKEN_A, b"name()", revert())
            .answer(TOKEN_A, b"NAME()", str_ret("Maker"))
            .answer(TOKEN_A, b"symbol()", vec![0u8; 32])
            .answer(TOKEN_A, b"SYMBOL()", str_ret("MKR"))
            .answer(TOKEN_A, b"decimals()", word_ret(18))
            .answer(TOKEN_A, b"DECIMALS()", word_ret(6));
        let metas = read_batch(
            &chain,
            &[TOKEN_A],
            MetadataShape::LowerUpperStopOnCallError,
            None,
        )
        .await;
        assert_eq!(metas[0].name, None);
        assert_eq!(metas[0].symbol.as_deref(), Some("MKR"));
        assert_eq!(metas[0].decimals, Some(18));

        // A succeeded-but-undecodable lower + a failed upper is absent too.
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", vec![0u8; 32])
            .fail(TOKEN_A, b"NAME()", revert())
            .answer(TOKEN_A, b"symbol()", str_ret("MKR"))
            .answer(TOKEN_A, b"decimals()", vec![0u8; 16])
            .answer(TOKEN_A, b"DECIMALS()", word_ret(6));
        let metas = read_batch(
            &chain,
            &[TOKEN_A],
            MetadataShape::LowerUpperStopOnCallError,
            None,
        )
        .await;
        assert_eq!(metas[0].name, None);
        assert_eq!(metas[0].symbol.as_deref(), Some("MKR"));
        assert_eq!(metas[0].decimals, Some(6));
    }

    #[tokio::test]
    async fn read_batch_stop_on_call_error_degrades_to_per_call_sequential_reads() {
        // The batch call fails: every sub-call becomes its own `eth_call`
        // (the whole eager request set survives degradation — NAME() is
        // issued even though name() answered), and the failed ones map to
        // absent fields.
        let chain = Chain::new()
            .failing_batch()
            .answer(TOKEN_A, b"name()", str_ret("Alpha"))
            .answer(TOKEN_A, b"symbol()", str_ret("ALPHA"))
            .answer(TOKEN_A, b"decimals()", word_ret(6));
        let metas = read_batch(
            &chain,
            &[TOKEN_A],
            MetadataShape::LowerUpperStopOnCallError,
            PIN,
        )
        .await;
        assert_eq!(
            metas[0],
            TokenMetadata {
                name: Some("Alpha".into()),
                symbol: Some("ALPHA".into()),
                decimals: Some(6),
            }
        );
        assert_eq!(
            wire(&chain),
            vec![
                (MULTICALL3_ADDRESS, AGGREGATE3_SELECTOR, PIN),
                (TOKEN_A, selector(b"name()"), PIN),
                (TOKEN_A, selector(b"NAME()"), PIN),
                (TOKEN_A, selector(b"symbol()"), PIN),
                (TOKEN_A, selector(b"SYMBOL()"), PIN),
                (TOKEN_A, selector(b"decimals()"), PIN),
                (TOKEN_A, selector(b"DECIMALS()"), PIN),
            ]
        );
    }

    #[tokio::test]
    async fn read_batch_of_no_tokens_issues_no_calls() {
        let chain = Chain::new();
        assert_eq!(
            read_batch(&chain, &[], MetadataShape::LowerOnly, None).await,
            Vec::new()
        );
        assert!(wire(&chain).is_empty());
    }

    // ── decode edge cases ─────────────────────────────────────────────────

    #[test]
    fn decode_string_return_carries_the_strict_dynamic_and_bytes32_contract() {
        // The dynamic answer decodes at its exact length.
        assert_eq!(decode_string_return(&str_ret("WETH")), Some("WETH".into()));
        // A bytes32 answer: trailing NULs stripped, leading kept.
        assert_eq!(
            decode_string_return(&bytes32_ret("DAI")),
            Some("DAI".into())
        );
        // An all-NUL bytes32 is absent — never an empty string.
        assert_eq!(decode_string_return(&[0u8; 32]), None);
        // A dynamic shape with a non-`0x20` offset is not the strict dynamic
        // answer; the bytes32 arm reads the first word verbatim, so the
        // offset byte itself survives as data.
        let mut odd = str_ret("WETH");
        odd[31] = 0x40;
        assert_eq!(
            decode_string_return(&odd),
            Some(
                String::from_utf8_lossy(&odd[..32])
                    .trim_end_matches('\0')
                    .to_string()
            )
        );
    }

    #[test]
    fn decode_dynamic_string_decodes_name() {
        let mut ret = vec![0u8; 96];
        ret[31] = 0x20;
        ret[63] = 4;
        ret[64..68].copy_from_slice(b"WETH");
        assert_eq!(decode_dynamic_string(&ret), Some("WETH".to_string()));
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_short_return() {
        assert_eq!(decode_dynamic_string(&[0u8; 32]), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_non_0x20_offset() {
        let mut ret = vec![0u8; 96];
        ret[31] = 0x40;
        ret[63] = 4;
        ret[64..68].copy_from_slice(b"WETH");
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_length_overrun() {
        let mut ret = vec![0u8; 96];
        ret[31] = 0x20;
        ret[63] = 0xff;
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_a_max_u256_length() {
        // `0xff..ff` must not truncate to `usize::MAX` and overflow the
        // bounds addition — it is absence.
        let mut ret = vec![0u8; 96];
        ret[31] = 0x20;
        ret[32..64].fill(0xff);
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_a_length_past_usize_width() {
        // 2^64 must not truncate to zero and decode as an empty string.
        let mut ret = vec![0u8; 96];
        ret[31] = 0x20;
        ret[55] = 1;
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    #[test]
    fn decode_dynamic_string_returns_none_for_a_length_overflowing_the_bounds_sum() {
        // `usize::MAX` fits the length conversion but not `64 + length`:
        // checked addition, not a wrapped bound.
        let mut ret = vec![0u8; 96];
        ret[31] = 0x20;
        ret[56..64].fill(0xff);
        assert_eq!(decode_dynamic_string(&ret), None);
    }

    #[tokio::test]
    async fn read_lower_only_decimals_past_u64_width_is_absence_not_truncation() {
        // A decimals word past `u64` width is this shape's decode absence —
        // never a truncated low word.
        let chain = Chain::new()
            .answer(TOKEN_A, b"name()", str_ret("Dai"))
            .answer(TOKEN_A, b"symbol()", str_ret("DAI"))
            .answer(
                TOKEN_A,
                b"decimals()",
                big_word_ret((U256::from(1u64) << 64) + U256::from(5u64)),
            );
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerOnly,
            FieldMask::ALL,
            None,
        )
        .await;
        assert_eq!(meta.decimals, None);
    }

    #[tokio::test]
    async fn read_fall_through_decimals_past_u64_width_falls_to_the_upper_spelling() {
        // The lower word is past `u64` width (truncation would read it as 5):
        // a decode failure, falling through like the `u8` width rule.
        let chain = Chain::new()
            .answer(
                TOKEN_A,
                b"decimals()",
                big_word_ret((U256::from(1u64) << 64) + U256::from(5u64)),
            )
            .answer(TOKEN_A, b"DECIMALS()", word_ret(6));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperFallThrough,
            FieldMask {
                name: false,
                symbol: false,
                decimals: true,
            },
            None,
        )
        .await;
        assert_eq!(meta.decimals, Some(6));
    }

    #[tokio::test]
    async fn read_stop_on_call_error_decimals_past_u64_width_is_absence() {
        // Both spellings answer past `u64` width: absence, never a truncated
        // low word.
        let chain = Chain::new()
            .answer(
                TOKEN_A,
                b"decimals()",
                big_word_ret((U256::from(1u64) << 64) + U256::from(5u64)),
            )
            .answer(TOKEN_A, b"DECIMALS()", big_word_ret(U256::MAX));
        let meta = read(
            &chain,
            TOKEN_A,
            MetadataShape::LowerUpperStopOnCallError,
            FieldMask {
                name: false,
                symbol: false,
                decimals: true,
            },
            None,
        )
        .await;
        assert_eq!(meta.decimals, None);
    }
}
