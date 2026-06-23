//! Executor calldata construction helpers.
//!
//! This module provides a pure-Rust core for building calldata for the locked
//! `Executor.sol` entry points described in the mev-arbitrum architecture docs.
//! It does not broadcast transactions or own policy. It only turns typed
//! execution plans into ABI-encoded calldata.

use crate::errors::{ExecutionError, ExecutionResult};
use alloy::dyn_abi::DynSolValue;
use alloy::primitives::{keccak256, Address, Bytes, U256};

/// `Executor.executeNativeArb((...))` canonical signature.
pub const EXECUTE_NATIVE_ARB_SIGNATURE: &str =
    "executeNativeArb((address,uint8,address,uint256,(uint8,address,bytes,address,address,uint256,uint256)[],uint256,uint256))";

/// `Executor.matchInternal((...))` canonical signature (12-field MatchParams,
/// incl. ADR-029 settlement-funding) → selector 0x210ea473.
pub const MATCH_INTERNAL_SIGNATURE: &str =
    "matchInternal((bytes,bytes,address[],uint256[],address,uint8,address,uint256,uint256,uint256,address,uint256))";

/// `Executor.composeFourLeg((...))` canonical signature (12-field ComposeParams,
/// incl. ADR-029 settlement-funding) → selector 0x2e1977d2.
pub const COMPOSE_FOUR_LEG_SIGNATURE: &str =
    "composeFourLeg((bytes,(uint8,address,bytes,address,address,uint256,uint256)[],bytes,bytes,address,uint8,address,uint256,uint256,uint256,address,uint256))";

use std::str::FromStr;

/// Flash-loan protocol selector used by `Executor.sol`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashProtocol {
    Aave = 0,
    Morpho = 1,
    ERC3156 = 2,
    UniV3 = 3,
}

impl FromStr for FlashProtocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "aave" | "aavev3" => Ok(FlashProtocol::Aave),
            "morpho" | "morphoblue" => Ok(FlashProtocol::Morpho),
            "erc3156" => Ok(FlashProtocol::ERC3156),
            "univ3" | "uniswapv3" => Ok(FlashProtocol::UniV3),
            _ => Err(format!("unknown flash protocol: {}", s)),
        }
    }
}

/// DEX category selector used by `Executor.sol`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DexKind {
    UniV2 = 0,
    UniV3 = 1,
    UniV4 = 2,
    Curve = 3,
    Reserved = 4,
    AggregatorV6 = 5,
    MorphoBlue = 6,
    Algebra = 7,
    Solidly = 8,
    CurveNG = 9,
    BalancerV2 = 10,
    MaverickV2 = 11,
    DodoPmm = 12,
    FluidDex = 13,
    BalancerV3 = 14,
    KyberElastic = 15,
    LFJLiquidityBook = 16,
    GMXV2 = 17,
    Wombat = 18,
    Bebop = 19,
    Hashflow = 20,
    WooFi = 21,
    OKXDex = 22,
    Enso = 23,
    Squid = 24,
    LIFI = 25,
    Rango = 26,
    Rubic = 27,
    Native = 28,
}

impl FromStr for DexKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "univ2" | "uniswapv2" => Ok(DexKind::UniV2),
            "univ3" | "uniswapv3" => Ok(DexKind::UniV3),
            "univ4" | "uniswapv4" => Ok(DexKind::UniV4),
            "curve" => Ok(DexKind::Curve),
            "curveng" => Ok(DexKind::CurveNG),
            "solidly" | "aerodrome" => Ok(DexKind::Solidly),
            "algebra" => Ok(DexKind::Algebra),
            "maverick" | "maverickv2" => Ok(DexKind::MaverickV2),
            "dodo" | "dodopmm" => Ok(DexKind::DodoPmm),
            "fluid" | "fluiddex" => Ok(DexKind::FluidDex),
            "kyber" | "kyberelastic" => Ok(DexKind::KyberElastic),
            "lfj" | "lfjliquiditybook" => Ok(DexKind::LFJLiquidityBook),
            "gmx" | "gmxv2" => Ok(DexKind::GMXV2),
            "native" => Ok(DexKind::Native),
            "reserved" => Ok(DexKind::Reserved),
            "aggregatorv6" => Ok(DexKind::AggregatorV6),
            "morpho" | "morphoblue" => Ok(DexKind::MorphoBlue),
            "balancerv2" => Ok(DexKind::BalancerV2),
            "balancerv3" => Ok(DexKind::BalancerV3),
            "wombat" => Ok(DexKind::Wombat),
            "bebop" => Ok(DexKind::Bebop),
            "hashflow" => Ok(DexKind::Hashflow),
            "woofi" => Ok(DexKind::WooFi),
            "okxdex" => Ok(DexKind::OKXDex),
            "enso" => Ok(DexKind::Enso),
            "squid" => Ok(DexKind::Squid),
            "lifi" => Ok(DexKind::LIFI),
            "rango" => Ok(DexKind::Rango),
            "rubic" => Ok(DexKind::Rubic),
            _ => Err(format!("unknown dex kind: {}", s)),
        }
    }
}

/// One swap step in an execution plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapStep {
    pub dex_kind: DexKind,
    pub router: Address,
    pub call_data: Bytes,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub amount_out_min: U256,
}

impl SwapStep {
    #[must_use]
    pub fn to_alloy(&self) -> DynSolValue {
        DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(self.dex_kind as u8), 8),
            DynSolValue::Address(self.router),
            DynSolValue::Bytes(self.call_data.to_vec()),
            DynSolValue::Address(self.token_in),
            DynSolValue::Address(self.token_out),
            DynSolValue::Uint(self.amount_in, 256),
            DynSolValue::Uint(self.amount_out_min, 256),
        ])
    }
}

/// Native arbitrage calldata parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeArbParams {
    pub flash_lender: Address,
    pub flash_protocol: FlashProtocol,
    pub flash_token: Address,
    pub flash_amount: U256,
    pub swaps: Vec<SwapStep>,
    pub min_profit: U256,
    pub deadline: U256,
}

impl NativeArbParams {
    pub const fn validate(&self) -> ExecutionResult<()> {
        if self.swaps.is_empty() {
            return Err(ExecutionError::EmptySwapChain {
                strategy: "native arb",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn to_alloy(&self) -> DynSolValue {
        DynSolValue::Tuple(vec![
            DynSolValue::Address(self.flash_lender),
            DynSolValue::Uint(U256::from(self.flash_protocol as u8), 8),
            DynSolValue::Address(self.flash_token),
            DynSolValue::Uint(self.flash_amount, 256),
            DynSolValue::Array(self.swaps.iter().map(SwapStep::to_alloy).collect()),
            DynSolValue::Uint(self.min_profit, 256),
            DynSolValue::Uint(self.deadline, 256),
        ])
    }
}

/// Internal match calldata parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchParams {
    pub cow_settlement_calldata: Bytes,
    pub uniswapx_batch_calldata: Bytes,
    pub expected_token_inflows: Vec<Address>,
    pub expected_token_inflow_min: Vec<U256>,
    pub flash_lender: Address,
    pub flash_protocol: FlashProtocol,
    pub flash_token: Address,
    pub flash_amount: U256,
    pub min_profit: U256,
    pub deadline: U256,
    /// ADR-029 settlement-funding commitment (audit 2026-06-09). `(0x0, 0)` when no CoW leg.
    pub settlement_funding_token: Address,
    pub settlement_funding_max: U256,
}

impl MatchParams {
    pub const fn validate(&self) -> ExecutionResult<()> {
        if self.expected_token_inflows.len() != self.expected_token_inflow_min.len() {
            return Err(ExecutionError::LengthMismatch {
                left: "expected_token_inflows",
                left_len: self.expected_token_inflows.len(),
                right: "expected_token_inflow_min",
                right_len: self.expected_token_inflow_min.len(),
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn to_alloy(&self) -> DynSolValue {
        DynSolValue::Tuple(vec![
            DynSolValue::Bytes(self.cow_settlement_calldata.to_vec()),
            DynSolValue::Bytes(self.uniswapx_batch_calldata.to_vec()),
            DynSolValue::Array(
                self.expected_token_inflows
                    .iter()
                    .copied()
                    .map(DynSolValue::Address)
                    .collect(),
            ),
            DynSolValue::Array(
                self.expected_token_inflow_min
                    .iter()
                    .copied()
                    .map(|amount| DynSolValue::Uint(amount, 256))
                    .collect(),
            ),
            DynSolValue::Address(self.flash_lender),
            DynSolValue::Uint(U256::from(self.flash_protocol as u8), 8),
            DynSolValue::Address(self.flash_token),
            DynSolValue::Uint(self.flash_amount, 256),
            DynSolValue::Uint(self.min_profit, 256),
            DynSolValue::Uint(self.deadline, 256),
            DynSolValue::Address(self.settlement_funding_token),
            DynSolValue::Uint(self.settlement_funding_max, 256),
        ])
    }
}

/// Four-leg composition calldata parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeParams {
    pub across_fill_calldata: Bytes,
    pub arb_swaps: Vec<SwapStep>,
    pub cow_fill_calldata: Bytes,
    pub uniswapx_rebalance_calldata: Bytes,
    pub flash_lender: Address,
    pub flash_protocol: FlashProtocol,
    pub flash_token: Address,
    pub flash_amount: U256,
    pub min_profit: U256,
    pub deadline: U256,
    /// ADR-029 settlement-funding commitment (audit 2026-06-09). `(0x0, 0)` when Leg 3 is empty.
    pub settlement_funding_token: Address,
    pub settlement_funding_max: U256,
}

impl ComposeParams {
    pub const fn validate(&self) -> ExecutionResult<()> {
        if self.arb_swaps.is_empty() {
            return Err(ExecutionError::EmptySwapChain {
                strategy: "four-leg composition",
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn to_alloy(&self) -> DynSolValue {
        DynSolValue::Tuple(vec![
            DynSolValue::Bytes(self.across_fill_calldata.to_vec()),
            DynSolValue::Array(self.arb_swaps.iter().map(SwapStep::to_alloy).collect()),
            DynSolValue::Bytes(self.cow_fill_calldata.to_vec()),
            DynSolValue::Bytes(self.uniswapx_rebalance_calldata.to_vec()),
            DynSolValue::Address(self.flash_lender),
            DynSolValue::Uint(U256::from(self.flash_protocol as u8), 8),
            DynSolValue::Address(self.flash_token),
            DynSolValue::Uint(self.flash_amount, 256),
            DynSolValue::Uint(self.min_profit, 256),
            DynSolValue::Uint(self.deadline, 256),
            DynSolValue::Address(self.settlement_funding_token),
            DynSolValue::Uint(self.settlement_funding_max, 256),
        ])
    }
}

/// Compute the 4-byte selector for a canonical Solidity signature.
#[must_use]
pub fn function_selector(signature: &str) -> [u8; 4] {
    let hash = keccak256(signature.as_bytes());
    let mut selector = [0u8; 4];
    selector.copy_from_slice(&hash[..4]);
    selector
}

fn encode_calldata(signature: &str, value: &DynSolValue) -> Bytes {
    let encoded = value.abi_encode_params();
    let mut calldata = Vec::with_capacity(4 + encoded.len());
    calldata.extend_from_slice(&function_selector(signature));
    calldata.extend_from_slice(&encoded);
    Bytes::from(calldata)
}

/// Encode `Executor.executeNativeArb`.
pub fn encode_native_arb_calldata(params: &NativeArbParams) -> ExecutionResult<Bytes> {
    params.validate()?;
    Ok(encode_calldata(
        EXECUTE_NATIVE_ARB_SIGNATURE,
        &params.to_alloy(),
    ))
}

/// Encode `Executor.matchInternal`.
pub fn encode_match_internal_calldata(params: &MatchParams) -> ExecutionResult<Bytes> {
    params.validate()?;
    Ok(encode_calldata(
        MATCH_INTERNAL_SIGNATURE,
        &params.to_alloy(),
    ))
}

/// Encode `Executor.composeFourLeg`.
pub fn encode_compose_four_leg_calldata(params: &ComposeParams) -> ExecutionResult<Bytes> {
    params.validate()?;
    Ok(encode_calldata(
        COMPOSE_FOUR_LEG_SIGNATURE,
        &params.to_alloy(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use alloy::dyn_abi::{DynSolType, DynSolValue};
    use alloy::primitives::{Address, Bytes, U256};

    fn sample_swap_step() -> SwapStep {
        SwapStep {
            dex_kind: DexKind::UniV3,
            router: Address::repeat_byte(0x11),
            call_data: Bytes::from(vec![0xaa, 0xbb, 0xcc]),
            token_in: Address::repeat_byte(0x22),
            token_out: Address::repeat_byte(0x33),
            amount_in: U256::from(456u64),
            amount_out_min: U256::from(789u64),
        }
    }

    fn sample_native_arb_params() -> NativeArbParams {
        NativeArbParams {
            flash_lender: Address::repeat_byte(0x44),
            flash_protocol: FlashProtocol::Aave,
            flash_token: Address::repeat_byte(0x55),
            flash_amount: U256::from(1234u64),
            swaps: vec![sample_swap_step()],
            min_profit: U256::from(42u64),
            deadline: U256::from(999_999u64),
        }
    }

    fn sample_match_params() -> MatchParams {
        MatchParams {
            cow_settlement_calldata: Bytes::from(vec![0xca, 0xfe]),
            uniswapx_batch_calldata: Bytes::from(vec![0xba, 0xbe]),
            expected_token_inflows: vec![Address::repeat_byte(0x66)],
            expected_token_inflow_min: vec![U256::from(11u64)],
            flash_lender: Address::repeat_byte(0x44),
            flash_protocol: FlashProtocol::Morpho,
            flash_token: Address::repeat_byte(0x55),
            flash_amount: U256::from(1234u64),
            min_profit: U256::from(42u64),
            deadline: U256::from(999_999u64),
            settlement_funding_token: Address::ZERO,
            settlement_funding_max: U256::ZERO,
        }
    }

    fn sample_compose_params() -> ComposeParams {
        ComposeParams {
            across_fill_calldata: Bytes::from(vec![0xac, 0x11]),
            arb_swaps: vec![sample_swap_step()],
            cow_fill_calldata: Bytes::from(vec![0xc0, 0x0f]),
            uniswapx_rebalance_calldata: Bytes::from(vec![0xfa, 0xce]),
            flash_lender: Address::repeat_byte(0x44),
            flash_protocol: FlashProtocol::ERC3156,
            flash_token: Address::repeat_byte(0x55),
            flash_amount: U256::from(1234u64),
            min_profit: U256::from(42u64),
            deadline: U256::from(999_999u64),
            settlement_funding_token: Address::ZERO,
            settlement_funding_max: U256::ZERO,
        }
    }

    /// The encode_* entry points must PREPEND the locked 4-byte selectors to
    /// their ABI body. Asserts against byte literals (not `function_selector`
    /// recomputed on the same string), so it actually guards the
    /// signature-const → `encode_calldata` wiring end-to-end. (The prior
    /// `test_selector_matches_keccak` compared `function_selector(SIG)` to
    /// `keccak256(SIG)` — the same computation on both sides, so it could
    /// never fail.)
    #[test]
    fn encoders_prepend_locked_selectors() {
        assert_eq!(
            &encode_native_arb_calldata(&sample_native_arb_params()).unwrap()[..4],
            &[0xf6, 0xf6, 0xad, 0xd1]
        );
        assert_eq!(
            &encode_match_internal_calldata(&sample_match_params()).unwrap()[..4],
            &[0x21, 0x0e, 0xa4, 0x73]
        );
        assert_eq!(
            &encode_compose_four_leg_calldata(&sample_compose_params()).unwrap()[..4],
            &[0x2e, 0x19, 0x77, 0xd2]
        );
    }

    #[test]
    fn test_strategy_selectors_match_locked_values() {
        assert_eq!(
            function_selector(EXECUTE_NATIVE_ARB_SIGNATURE),
            [0xf6, 0xf6, 0xad, 0xd1]
        );
        assert_eq!(
            function_selector(MATCH_INTERNAL_SIGNATURE),
            [0x21, 0x0e, 0xa4, 0x73]
        );
        assert_eq!(
            function_selector(COMPOSE_FOUR_LEG_SIGNATURE),
            [0x2e, 0x19, 0x77, 0xd2]
        );
    }

    /// Cross-check: this hand-rolled `DynSolValue` encoder and the alloy
    /// `sol!`-generated call structs in `crate::types::executor` (the
    /// `IExecutor.sol` mirror, fixtures-locked against
    /// `coordinator/src/types/fixtures.json`) MUST agree on every selector. A
    /// single edit to one signature string or one sol! struct field — e.g. the
    /// ADR-029 settlement-funding pair landing in only one of the two — fails
    /// here, rather than silently shipping calldata the diamond reverts on.
    #[test]
    fn selectors_match_sol_call_source_of_truth() {
        use crate::types::executor::{
            composeFourLegCall, executeNativeArbCall, matchInternalCall,
        };
        use alloy::sol_types::SolCall;

        assert_eq!(
            function_selector(EXECUTE_NATIVE_ARB_SIGNATURE),
            executeNativeArbCall::SELECTOR
        );
        assert_eq!(
            function_selector(MATCH_INTERNAL_SIGNATURE),
            matchInternalCall::SELECTOR
        );
        assert_eq!(
            function_selector(COMPOSE_FOUR_LEG_SIGNATURE),
            composeFourLegCall::SELECTOR
        );
    }

    #[test]
    fn test_native_arb_encoding_round_trip() {
        let params = sample_native_arb_params();

        let calldata = encode_native_arb_calldata(&params).unwrap();
        assert_eq!(
            &calldata[..4],
            &function_selector(EXECUTE_NATIVE_ARB_SIGNATURE)
        );

        let decoded = DynSolType::Tuple(vec![
            DynSolType::Address,
            DynSolType::Uint(8),
            DynSolType::Address,
            DynSolType::Uint(256),
            DynSolType::Array(Box::new(DynSolType::Tuple(vec![
                DynSolType::Uint(8),
                DynSolType::Address,
                DynSolType::Bytes,
                DynSolType::Address,
                DynSolType::Address,
                DynSolType::Uint(256),
                DynSolType::Uint(256),
            ]))),
            DynSolType::Uint(256),
            DynSolType::Uint(256),
        ])
        .abi_decode_params(&calldata[4..])
        .expect("calldata should decode");

        match decoded {
            DynSolValue::Tuple(fields) => {
                assert_eq!(fields.len(), 7);
                assert_eq!(fields[0], DynSolValue::Address(params.flash_lender));
                assert_eq!(
                    fields[1],
                    DynSolValue::Uint(U256::from(FlashProtocol::Aave as u8), 8)
                );
            }
            other => panic!("expected tuple, got {other:?}"),
        }
    }

    #[test]
    fn test_match_internal_encoding_round_trip() {
        let params = sample_match_params();

        let calldata = encode_match_internal_calldata(&params).unwrap();
        assert_eq!(&calldata[..4], &function_selector(MATCH_INTERNAL_SIGNATURE));

        let decoded = DynSolType::Tuple(vec![
            DynSolType::Bytes,
            DynSolType::Bytes,
            DynSolType::Array(Box::new(DynSolType::Address)),
            DynSolType::Array(Box::new(DynSolType::Uint(256))),
            DynSolType::Address,
            DynSolType::Uint(8),
            DynSolType::Address,
            DynSolType::Uint(256),
            DynSolType::Uint(256),
            DynSolType::Uint(256),
            DynSolType::Address,
            DynSolType::Uint(256),
        ])
        .abi_decode_params(&calldata[4..])
        .expect("calldata should decode");

        match decoded {
            DynSolValue::Tuple(fields) => {
                assert_eq!(fields.len(), 12);
                assert_eq!(
                    fields[0],
                    DynSolValue::Bytes(params.cow_settlement_calldata.to_vec())
                );
                assert_eq!(
                    fields[5],
                    DynSolValue::Uint(U256::from(FlashProtocol::Morpho as u8), 8)
                );
            }
            other => panic!("expected tuple, got {other:?}"),
        }
    }

    #[test]
    fn test_compose_four_leg_encoding_round_trip() {
        let params = sample_compose_params();

        let calldata = encode_compose_four_leg_calldata(&params).unwrap();
        assert_eq!(
            &calldata[..4],
            &function_selector(COMPOSE_FOUR_LEG_SIGNATURE)
        );

        let decoded = DynSolType::Tuple(vec![
            DynSolType::Bytes,
            DynSolType::Array(Box::new(DynSolType::Tuple(vec![
                DynSolType::Uint(8),
                DynSolType::Address,
                DynSolType::Bytes,
                DynSolType::Address,
                DynSolType::Address,
                DynSolType::Uint(256),
                DynSolType::Uint(256),
            ]))),
            DynSolType::Bytes,
            DynSolType::Bytes,
            DynSolType::Address,
            DynSolType::Uint(8),
            DynSolType::Address,
            DynSolType::Uint(256),
            DynSolType::Uint(256),
            DynSolType::Uint(256),
            DynSolType::Address,
            DynSolType::Uint(256),
        ])
        .abi_decode_params(&calldata[4..])
        .expect("calldata should decode");

        match decoded {
            DynSolValue::Tuple(fields) => {
                assert_eq!(fields.len(), 12);
                assert_eq!(
                    fields[0],
                    DynSolValue::Bytes(params.across_fill_calldata.to_vec())
                );
                assert_eq!(
                    fields[5],
                    DynSolValue::Uint(U256::from(FlashProtocol::ERC3156 as u8), 8)
                );
            }
            other => panic!("expected tuple, got {other:?}"),
        }
    }

    #[test]
    fn test_match_validation_rejects_mismatched_lengths() {
        let mut params = sample_match_params();
        params.expected_token_inflow_min.clear();

        let err = encode_match_internal_calldata(&params).unwrap_err();
        assert!(matches!(err, ExecutionError::LengthMismatch { .. }));
    }

    #[test]
    fn test_compose_validation_rejects_empty_swap_chain() {
        let mut params = sample_compose_params();
        params.arb_swaps.clear();

        let err = encode_compose_four_leg_calldata(&params).unwrap_err();
        assert!(matches!(err, ExecutionError::EmptySwapChain { .. }));
    }
}
