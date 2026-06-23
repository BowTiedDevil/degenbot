//! Cross-language wire types — serde mirrors of the Executor strategy
//! entry-point structs.
//!
//! These types deserialize the JSON snapshot in
//! `coordinator/src/types/fixtures.json` (the cross-language ABI lock) and
//! convert into the `sol!`-generated structs in [`super::executor`] for
//! ABI encoding. They are also the canonical inbound shape for any
//! coordinator-emitted strategy params over the Unix-domain-socket NDJSON
//! IPC channel — the strategist composes a `WireXxxParams` and the engine
//! runs `WireXxxParams::into() -> XxxParams::abi_encode()`.
//!
//! ## Wire invariants (locked against the TS generator + fixtures snapshot)
//!
//! - keys use **camelCase** (`flashLender`, `dexKind`, …);
//! - `U256` is a **plain decimal string** (`"1000000000"`), NOT hex and NOT
//!   the TS-internal `{__bi: "<dec>"}` envelope used in `coordinator/.../wire.ts`;
//! - `Address` is `"0x..."` checksummed on emission via the
//!   [`checksum_address`] / [`checksum_address_vec`] serde adapters
//!   (EIP-55, no chain-id prefix per EIP-1191). alloy's default `Address`
//!   serde emits lowercase, which would break the Phase G envelope byte
//!   lock against the viem-generated fixture; the adapters bring Rust's
//!   emission in line with the TS generator. Decode side accepts any case;
//! - `Bytes` is even-length lowercase hex with `0x` prefix; `"0x"` is a
//!   valid empty-bytes value.
//!
//! ## sol! bridge
//!
//! `From<WireXxxParams> for super::executor::XxxParams` performs the
//! conversion. alloy's `sol!` macro lowers `type FlashProtocol is uint8;`
//! and `type DexKind is uint8;` to plain `u8` fields on the surrounding
//! structs (with the user-defined wrapper types existing separately for
//! external typed use), so the bridge is a straight-through copy.

use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};

use super::executor::{ComposeParams, MatchParams, NativeArbParams, SwapStep};
use super::settlement::SettlementResultKind;

// ---------------------------------------------------------------------------
// Decimal-string serde adapters for `U256` and `Vec<U256>`.
// ---------------------------------------------------------------------------

/// `#[serde(with = "decimal_u256")]` — round-trip `U256` as a base-10 string.
///
/// alloy's default `U256` serde uses hex; the cross-language fixture format
/// is decimal (`"1000000000"`), matching how Python emits `str(int)` and
/// how viem's `bigint.toString()` renders. Diverging here breaks the
/// fixture lock.
pub mod decimal_u256 {
    use alloy::primitives::U256;
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serializer};

    /// Serialize `U256` as its decimal-string representation.
    pub fn serialize<S>(value: &U256, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    /// Deserialize `U256` from a base-10 decimal string. Hex is rejected —
    /// strict format keeps the wire layer self-describing.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<U256, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        U256::from_str_radix(&s, 10)
            .map_err(|e| D::Error::custom(format!("decimal_u256: failed to parse {s:?}: {e}")))
    }
}

/// `#[serde(with = "decimal_u256_vec")]` — round-trip `Vec<U256>` as a JSON
/// array of decimal strings. Serde's `with` attribute does NOT compose over
/// `Vec`, so the per-vec adapter is a separate module.
pub mod decimal_u256_vec {
    use alloy::primitives::U256;
    use serde::{de::Error as DeError, ser::SerializeSeq, Deserialize, Deserializer, Serializer};

    /// Serialize each element as a decimal string.
    pub fn serialize<S>(values: &[U256], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for v in values {
            seq.serialize_element(&v.to_string())?;
        }
        seq.end()
    }

    /// Deserialize a sequence of decimal strings into `Vec<U256>`.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<U256>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let strings: Vec<String> = Vec::deserialize(deserializer)?;
        strings
            .into_iter()
            .map(|s| {
                U256::from_str_radix(&s, 10).map_err(|e| {
                    D::Error::custom(format!("decimal_u256_vec: failed to parse {s:?}: {e}"))
                })
            })
            .collect()
    }
}

/// `#[serde(with = "decimal_u64")]` — round-trip `u64` as a base-10 string.
///
/// The Phase I settlement wire format encodes `gasUsed` as a **decimal
/// string** (e.g., `"350000"`), NOT a JSON number. The TS encoder
/// (`coordinator/src/types/settlement-wire.ts`) writes
/// `bigint.toString()` and `coordinator/src/types/fixtures.json`
/// records that shape. Diverging here breaks the cross-language byte
/// lock for the Settlement payload.
///
/// `block` stays a JSON number on both sides (Arbitrum block heights
/// fit comfortably below `Number.MAX_SAFE_INTEGER`); only `gasUsed`
/// uses this adapter.
pub mod decimal_u64 {
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serializer};

    /// Serialize `u64` as its decimal-string representation.
    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    /// Deserialize `u64` from a base-10 decimal string. Hex / signed /
    /// scientific are rejected for symmetry with `decimal_u256` — the
    /// wire layer is strict by design.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<u64>()
            .map_err(|e| D::Error::custom(format!("decimal_u64: failed to parse {s:?}: {e}")))
    }
}

// ---------------------------------------------------------------------------
// EIP-55 checksum-address serde adapters.
// ---------------------------------------------------------------------------

/// `#[serde(with = "checksum_address")]` — round-trip `Address` as an
/// EIP-55 mixed-case checksum string.
///
/// alloy's default `Address` `Serialize` emits lowercase
/// (`0x794a61358d6845594f94dc1db02a252b5b4814ad`); viem (the TS fixture
/// generator) and `Address::to_checksum(None)` emit the EIP-55 form
/// (`0x794a61358D6845594F94dc1DB02A252b5b4814aD`). The Phase G envelope
/// byte lock asserts on the rendered JSON, so Rust must match TS or the
/// fixture lock breaks.
///
/// `Address::to_checksum(None)` produces the **chain-id-agnostic** form
/// (no EIP-1191 prefix), matching viem's default and the locked
/// `coordinator/src/types/fixtures.json` snapshot.
///
/// Decode side accepts any case — alloy's `FromStr` for `Address` is
/// case-insensitive — so a payload coming back from a peer that doesn't
/// re-checksum still parses cleanly.
pub mod checksum_address {
    use alloy::primitives::Address;
    use serde::{de::Error as DeError, Deserialize, Deserializer, Serializer};

    /// Serialize `Address` in EIP-55 checksum form.
    pub fn serialize<S>(value: &Address, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_checksum(None))
    }

    /// Deserialize `Address` from any-case `0x` hex.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Address, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<Address>()
            .map_err(|e| D::Error::custom(format!("checksum_address: failed to parse {s:?}: {e}")))
    }
}

/// `#[serde(with = "checksum_address_vec")]` — round-trip `Vec<Address>`
/// as a JSON array of EIP-55 checksum strings. Mirror of [`checksum_address`]
/// for the `expected_token_inflows` field on [`WireMatchParams`]. Serde's
/// `with` attribute does NOT compose over `Vec`, so the per-vec adapter is
/// a separate module.
pub mod checksum_address_vec {
    use alloy::primitives::Address;
    use serde::{de::Error as DeError, ser::SerializeSeq, Deserialize, Deserializer, Serializer};

    /// Serialize each element via `Address::to_checksum(None)`.
    pub fn serialize<S>(values: &[Address], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for v in values {
            seq.serialize_element(&v.to_checksum(None))?;
        }
        seq.end()
    }

    /// Deserialize a sequence of any-case `0x` hex strings into
    /// `Vec<Address>`.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<Address>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let strings: Vec<String> = Vec::deserialize(deserializer)?;
        strings
            .into_iter()
            .map(|s| {
                s.parse::<Address>().map_err(|e| {
                    D::Error::custom(format!("checksum_address_vec: failed to parse {s:?}: {e}"))
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Wire structs.
// ---------------------------------------------------------------------------

/// Wire-format mirror of [`SwapStep`]. `dex_kind` is a raw `u8` — matches
/// the underlying primitive `sol!` lowers `type DexKind is uint8;` to at
/// the struct-field level (see the macro-shape note in `executor.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSwapStep {
    /// `DexKind` ordinal — see `super::executor::DexKind` doc.
    pub dex_kind: u8,
    #[serde(with = "checksum_address")]
    pub router: Address,
    pub call_data: Bytes,
    #[serde(with = "checksum_address")]
    pub token_in: Address,
    #[serde(with = "checksum_address")]
    pub token_out: Address,
    #[serde(with = "decimal_u256")]
    pub amount_in: U256,
    #[serde(with = "decimal_u256")]
    pub amount_out_min: U256,
}

/// Wire-format mirror of [`NativeArbParams`]. `flash_protocol` is a raw `u8`
/// for the same reason as `dex_kind` above.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireNativeArbParams {
    #[serde(with = "checksum_address")]
    pub flash_lender: Address,
    pub flash_protocol: u8,
    #[serde(with = "checksum_address")]
    pub flash_token: Address,
    #[serde(with = "decimal_u256")]
    pub flash_amount: U256,
    pub swaps: Vec<WireSwapStep>,
    #[serde(with = "decimal_u256")]
    pub min_profit: U256,
    #[serde(with = "decimal_u256")]
    pub deadline: U256,
}

/// Wire-format mirror of [`MatchParams`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMatchParams {
    pub cow_settlement_calldata: Bytes,
    pub uniswapx_batch_calldata: Bytes,
    #[serde(with = "checksum_address_vec")]
    pub expected_token_inflows: Vec<Address>,
    #[serde(with = "decimal_u256_vec")]
    pub expected_token_inflow_min: Vec<U256>,
    #[serde(with = "checksum_address")]
    pub flash_lender: Address,
    pub flash_protocol: u8,
    #[serde(with = "checksum_address")]
    pub flash_token: Address,
    #[serde(with = "decimal_u256")]
    pub flash_amount: U256,
    #[serde(with = "decimal_u256")]
    pub min_profit: U256,
    #[serde(with = "decimal_u256")]
    pub deadline: U256,
    /// ADR-029 settlement-funding commitment (audit 2026-06-09). `(0x0, 0)`
    /// when there is no CoW leg. Present in the canonical wire fixture
    /// (`coordinator/src/types/fixtures.json`) and required to round-trip the
    /// 12-field `MatchParams`.
    #[serde(with = "checksum_address")]
    pub settlement_funding_token: Address,
    #[serde(with = "decimal_u256")]
    pub settlement_funding_max: U256,
}

/// Wire-format mirror of [`ComposeParams`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireComposeParams {
    pub across_fill_calldata: Bytes,
    pub arb_swaps: Vec<WireSwapStep>,
    pub cow_fill_calldata: Bytes,
    pub uniswapx_rebalance_calldata: Bytes,
    #[serde(with = "checksum_address")]
    pub flash_lender: Address,
    pub flash_protocol: u8,
    #[serde(with = "checksum_address")]
    pub flash_token: Address,
    #[serde(with = "decimal_u256")]
    pub flash_amount: U256,
    #[serde(with = "decimal_u256")]
    pub min_profit: U256,
    #[serde(with = "decimal_u256")]
    pub deadline: U256,
    /// ADR-029 settlement-funding commitment (audit 2026-06-09). `(0x0, 0)`
    /// when Leg 3 is empty. Present in the canonical wire fixture
    /// (`coordinator/src/types/fixtures.json`) and required to round-trip the
    /// 12-field `ComposeParams`.
    #[serde(with = "checksum_address")]
    pub settlement_funding_token: Address,
    #[serde(with = "decimal_u256")]
    pub settlement_funding_max: U256,
}

// ---------------------------------------------------------------------------
// Phase I — Settlement wire form.
// ---------------------------------------------------------------------------

/// JSON wire mirror of the Phase I `Settlement` payload.
///
/// Locked byte-for-byte against the `settlements` array in
/// `coordinator/src/types/fixtures.json`. The TS source of truth is
/// `coordinator/src/types/settlement-wire.ts`. Field-level wire shape
/// (also documented in `coordinator/src/types/settlement-README.md`):
///
/// | Field            | Wire shape                            |
/// |------------------|---------------------------------------|
/// | `planId`         | 0x-prefixed 32-byte hex string        |
/// | `version`        | bare JSON number (uint8)              |
/// | `result`         | **variant NAME string** (not ordinal) |
/// | `txHash`         | 0x-prefixed 32-byte hex string        |
/// | `block`          | bare JSON number (uint64)             |
/// | `gasUsed`        | **decimal string** (uint64)           |
/// | `preflightDelta` | **decimal string** (uint256)          |
/// | `gasEstimate`    | **decimal string** (uint256)          |
/// | `error`          | bare string (empty on `Included`)     |
///
/// Why `result` is a string, not the ordinal: the TS canonical struct
/// (`coordinator/src/types/settlement.ts`) keeps a numeric ordinal in
/// memory but the wire codec emits the variant name. This Rust mirror
/// follows the same shape so a serde-deserialized Rust Settlement and a
/// `JSON.parse → settlementFromWire` TS Settlement carry byte-identical
/// content. Use [`SettlementResultKind::from_name`] to lift to the typed
/// Rust enum.
///
/// Why `gasUsed` is a decimal string while `block` is a number: TS
/// emits `gasUsed` via `bigint.toString()` for forward-compat with a
/// future widening of the Solidity field to `uint256`, while `block`
/// is `number` because Arbitrum block heights stay well below
/// `Number.MAX_SAFE_INTEGER`. The fixture JSON freezes both shapes;
/// see [`super::decimal_u64`] for the adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSettlement {
    /// `bytes32` — Plan id (Executor's `flowId`).
    pub plan_id: B256,
    /// Wire-protocol version. Currently
    /// [`super::settlement::SETTLEMENT_VERSION`] (1).
    pub version: u8,
    /// `SettlementResult` variant name. One of
    /// `"Included" | "Reverted" | "Dropped" | "PreflightFailed" | "Error"`.
    pub result: String,
    /// Broadcast tx hash, or 32 zero bytes when the Plan never reached the
    /// wire (`Dropped` pre-broadcast / `PreflightFailed` / `Error`).
    pub tx_hash: B256,
    /// Block number where the tx was included; 0 when not on chain.
    pub block: u64,
    /// Gas units consumed by the on-chain execution; 0 when not broadcast.
    /// Encoded as a **decimal string** on the wire (see module docs).
    #[serde(with = "decimal_u64")]
    pub gas_used: u64,
    /// REVM-simulated `profit_token` balance delta (wei). Populated for
    /// every variant that reached preflight, including `PreflightFailed`.
    #[serde(with = "decimal_u256")]
    pub preflight_delta: U256,
    /// Estimated total cost in wei (L1 + L2). 0 when the engine never
    /// reached the gas-estimation step (some `Error` variants).
    #[serde(with = "decimal_u256")]
    pub gas_estimate: U256,
    /// Free-text reason. Empty on `Included`; non-empty on every other
    /// variant.
    pub error: String,
}

impl WireSettlement {
    /// Lift the wire `result: String` to the typed
    /// [`SettlementResultKind`] enum. Returns `None` if the wire form
    /// carries an unknown name — strict on purpose, callers must
    /// surface an error rather than silently coerce.
    #[must_use]
    pub fn result_kind(&self) -> Option<SettlementResultKind> {
        SettlementResultKind::from_name(&self.result)
    }
}

// ---------------------------------------------------------------------------
// Wire -> sol! bridges. These are infallible — `u8` always lifts into the
// alloy newtypes, and `Address` / `Bytes` / `U256` are reused verbatim.
// ---------------------------------------------------------------------------

impl From<WireSwapStep> for SwapStep {
    fn from(w: WireSwapStep) -> Self {
        SwapStep {
            dexKind: w.dex_kind,
            router: w.router,
            callData: w.call_data,
            tokenIn: w.token_in,
            tokenOut: w.token_out,
            amountIn: w.amount_in,
            amountOutMin: w.amount_out_min,
        }
    }
}

impl From<WireNativeArbParams> for NativeArbParams {
    fn from(w: WireNativeArbParams) -> Self {
        NativeArbParams {
            flashLender: w.flash_lender,
            flashProtocol: w.flash_protocol,
            flashToken: w.flash_token,
            flashAmount: w.flash_amount,
            swaps: w.swaps.into_iter().map(SwapStep::from).collect(),
            minProfit: w.min_profit,
            deadline: w.deadline,
        }
    }
}

impl From<WireMatchParams> for MatchParams {
    fn from(w: WireMatchParams) -> Self {
        MatchParams {
            cowSettlementCalldata: w.cow_settlement_calldata,
            uniswapxBatchCalldata: w.uniswapx_batch_calldata,
            expectedTokenInflows: w.expected_token_inflows,
            expectedTokenInflowMin: w.expected_token_inflow_min,
            flashLender: w.flash_lender,
            flashProtocol: w.flash_protocol,
            flashToken: w.flash_token,
            flashAmount: w.flash_amount,
            minProfit: w.min_profit,
            deadline: w.deadline,
            settlementFundingToken: w.settlement_funding_token,
            settlementFundingMax: w.settlement_funding_max,
        }
    }
}

impl From<WireComposeParams> for ComposeParams {
    fn from(w: WireComposeParams) -> Self {
        ComposeParams {
            acrossFillCalldata: w.across_fill_calldata,
            arbSwaps: w.arb_swaps.into_iter().map(SwapStep::from).collect(),
            cowFillCalldata: w.cow_fill_calldata,
            uniswapxRebalanceCalldata: w.uniswapx_rebalance_calldata,
            flashLender: w.flash_lender,
            flashProtocol: w.flash_protocol,
            flashToken: w.flash_token,
            flashAmount: w.flash_amount,
            minProfit: w.min_profit,
            deadline: w.deadline,
            settlementFundingToken: w.settlement_funding_token,
            settlementFundingMax: w.settlement_funding_max,
        }
    }
}

#[cfg(test)]
mod tests {
    // Test-only `Wrap` newtypes exercise the serde adapters; serde writes
    // their field on deserialize but the assertions only check `is_err`.
    #![allow(dead_code)]

    use super::*;
    use alloy::primitives::address;
    use serde_json::json;

    #[test]
    fn decimal_u256_round_trip_small() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
        struct Wrap(#[serde(with = "decimal_u256")] U256);
        let w = Wrap(U256::from(1_000_000_000_u64));
        let s = serde_json::to_string(&w).unwrap();
        assert_eq!(s, "\"1000000000\"");
        let back: Wrap = serde_json::from_str(&s).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn decimal_u256_round_trip_max() {
        #[derive(Serialize, Deserialize)]
        struct Wrap(#[serde(with = "decimal_u256")] U256);
        let w = Wrap(U256::MAX);
        let s = serde_json::to_string(&w).unwrap();
        let back: Wrap = serde_json::from_str(&s).unwrap();
        assert_eq!(w.0, back.0);
    }

    #[test]
    fn decimal_u256_rejects_hex() {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "decimal_u256")] U256);
        let r: Result<Wrap, _> = serde_json::from_str("\"0xff\"");
        assert!(r.is_err(), "decimal-only adapter must reject hex");
    }

    #[test]
    fn decimal_u256_vec_round_trip() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
        struct Wrap(#[serde(with = "decimal_u256_vec")] Vec<U256>);
        let w = Wrap(vec![
            U256::from(1u64),
            U256::from(1_000_000_000_u64),
            U256::MAX,
        ]);
        let s = serde_json::to_string(&w).unwrap();
        let back: Wrap = serde_json::from_str(&s).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn wire_swap_step_camel_case_round_trip() {
        // Smoke test — verify camelCase keys come out as expected and an
        // empty-bytes `0x` value round-trips. This catches the pair (a) bad
        // serde rename, (b) Bytes empty handling.
        let v = WireSwapStep {
            dex_kind: 1,
            router: address!("111111125421cA6dc452d289314280a0f8842A65"),
            call_data: Bytes::new(),
            token_in: address!("aF88d065e77c8cC2239327C5EDb3A432268e5831"),
            token_out: address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
            amount_in: U256::from(1u64),
            amount_out_min: U256::ZERO,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["dexKind"], 1);
        assert_eq!(json["callData"], "0x");
        assert_eq!(json["amountIn"], "1");
        assert_eq!(json["amountOutMin"], "0");
        let back: WireSwapStep = serde_json::from_value(json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn wire_to_sol_swap_step_bridge_preserves_fields() {
        let w = WireSwapStep {
            dex_kind: 5,
            router: address!("111111125421cA6dc452d289314280a0f8842A65"),
            call_data: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            token_in: address!("aF88d065e77c8cC2239327C5EDb3A432268e5831"),
            token_out: address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1"),
            amount_in: U256::from(1_000_000_000_u64),
            amount_out_min: U256::from(250_000_000_000_000_000_u128),
        };
        let s: SwapStep = w.clone().into();
        // `dexKind` is `u8` after the sol! lowering of `type DexKind is uint8;`.
        assert_eq!(s.dexKind, w.dex_kind);
        assert_eq!(s.router, w.router);
        assert_eq!(s.callData, w.call_data);
        assert_eq!(s.tokenIn, w.token_in);
        assert_eq!(s.tokenOut, w.token_out);
        assert_eq!(s.amountIn, w.amount_in);
        assert_eq!(s.amountOutMin, w.amount_out_min);
    }

    #[test]
    fn wire_native_arb_camel_case_keys_match_fixtures() {
        // Locked field names — must stay camelCase for fixture compatibility.
        let v = json!({
            "flashLender": "0x794a61358D6845594F94dc1DB02A252b5b4814aD",
            "flashProtocol": 0,
            "flashToken": "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            "flashAmount": "1000000000",
            "swaps": [],
            "minProfit": "1000000",
            "deadline": "1730000000"
        });
        let parsed: WireNativeArbParams = serde_json::from_value(v).expect("decodes");
        assert_eq!(parsed.flash_protocol, 0);
        assert_eq!(parsed.flash_amount, U256::from(1_000_000_000_u64));
        assert_eq!(parsed.swaps.len(), 0);
    }

    // -----------------------------------------------------------------------
    // Phase I — decimal_u64 + WireSettlement.
    // -----------------------------------------------------------------------

    #[test]
    fn decimal_u64_round_trip_small() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
        struct Wrap(#[serde(with = "decimal_u64")] u64);
        let w = Wrap(350_000);
        let s = serde_json::to_string(&w).unwrap();
        // Locked emit shape: a JSON STRING, not a number — matches
        // settlement-wire.ts and fixtures.json (`"gasUsed": "350000"`).
        assert_eq!(s, "\"350000\"");
        let back: Wrap = serde_json::from_str(&s).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn decimal_u64_round_trip_zero() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
        struct Wrap(#[serde(with = "decimal_u64")] u64);
        let w = Wrap(0);
        let s = serde_json::to_string(&w).unwrap();
        assert_eq!(s, "\"0\"");
        let back: Wrap = serde_json::from_str(&s).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn decimal_u64_round_trip_max() {
        #[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
        struct Wrap(#[serde(with = "decimal_u64")] u64);
        let w = Wrap(u64::MAX);
        let s = serde_json::to_string(&w).unwrap();
        let back: Wrap = serde_json::from_str(&s).unwrap();
        assert_eq!(w, back);
    }

    #[test]
    fn decimal_u64_rejects_number_form() {
        // The wire form is a STRING. A bare JSON number must NOT decode —
        // diverging here would silently accept TS-internal `wire.ts` output
        // that uses bigint-as-number; we want strict.
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "decimal_u64")] u64);
        let r: Result<Wrap, _> = serde_json::from_str("350000");
        assert!(
            r.is_err(),
            "decimal_u64 must reject bare numbers (wire form is a string)"
        );
    }

    #[test]
    fn decimal_u64_rejects_negative() {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "decimal_u64")] u64);
        let r: Result<Wrap, _> = serde_json::from_str("\"-1\"");
        assert!(r.is_err(), "decimal_u64 must reject negative values");
    }

    #[test]
    fn decimal_u64_rejects_hex() {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "decimal_u64")] u64);
        let r: Result<Wrap, _> = serde_json::from_str("\"0xff\"");
        assert!(r.is_err(), "decimal_u64 must reject hex");
    }

    #[test]
    fn wire_settlement_camel_case_round_trip() {
        use alloy::primitives::b256;
        // Smoke test — verify the camelCase keys + decimal-string `gasUsed`
        // come out as expected. Catches: (a) bad serde rename, (b) wrong
        // adapter on `gasUsed`, (c) `result` accidentally re-typed to a
        // numeric ordinal.
        let v = WireSettlement {
            plan_id: b256!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            version: 1,
            result: "Included".to_string(),
            tx_hash: b256!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            block: 18_000_000,
            gas_used: 350_000,
            preflight_delta: U256::from(5_000_000_000_000_000_000_u128),
            gas_estimate: U256::from(200_000_000_000_000_u64),
            error: String::new(),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json["planId"],
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(json["version"], 1);
        assert_eq!(json["result"], "Included");
        assert_eq!(json["block"], 18_000_000);
        assert_eq!(json["gasUsed"], "350000"); // STRING, not number
        assert_eq!(json["preflightDelta"], "5000000000000000000");
        assert_eq!(json["gasEstimate"], "200000000000000");
        assert_eq!(json["error"], "");
        let back: WireSettlement = serde_json::from_value(json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn wire_settlement_result_kind_lift() {
        // `result_kind()` lifts the wire string into the typed enum and
        // returns None for unknown. Smoke test that the bridge module is
        // wired up.
        let mut v = WireSettlement {
            plan_id: B256::ZERO,
            version: 1,
            result: "Included".to_string(),
            tx_hash: B256::ZERO,
            block: 0,
            gas_used: 0,
            preflight_delta: U256::ZERO,
            gas_estimate: U256::ZERO,
            error: String::new(),
        };
        assert_eq!(v.result_kind(), Some(SettlementResultKind::Included));
        v.result = "Reverted".to_string();
        assert_eq!(v.result_kind(), Some(SettlementResultKind::Reverted));
        v.result = "Unknown".to_string();
        assert!(v.result_kind().is_none());
    }
}
