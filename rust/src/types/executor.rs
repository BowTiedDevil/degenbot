//! Rust canonical mirror of `contracts/src/interfaces/IExecutor.sol`.
//!
//! Phase F (Rust half). This module is the Rust source of truth for the
//! strategy-entry-point structs the engine ABI-encodes when the coordinator
//! routes a `Plan` whose calldata it intends to compose itself (rather than
//! forward verbatim from the TS coordinator). It mirrors the TS module
//! `coordinator/src/types/executor.ts` and the Python module
//! `solver/driver/types/executor.py` — every field name, ordering, and wire
//! type is identical to the on-chain contract.
//!
//! ## Locked invariants
//!
//! - Function selectors:
//!     - `executeNativeArb`         = `0xf6f6add1`
//!     - `executeOwnedSwaps`        = `0xba44420d` (disabled owned-inventory ABI)
//!     - `matchInternal`            = `0x210ea473` (12-field MatchParams, incl. ADR-029 settlement-funding)
//!     - `composeFourLeg`           = `0x2e1977d2` (12-field ComposeParams, incl. ADR-029 settlement-funding)
//!     - `triggerCoWFlashLoanRouter`= `0x900866ce` (Pick C — user-side entry)
//!     - `transferToSettlement`     = `0x4515dd0f` (ADR-029 — `interactions[1]` callback)
//! - `FlashProtocol` ordinals — only 0..3 are currently implemented on-chain
//!   (`Executor._triggerFlashLoan` at `contracts/src/executors/Executor.sol:1132+`):
//!     - 0 = Aave V3       — `IAavePool(lender).flashLoan(...)` with `M-A1` lender pin
//!     - 1 = Morpho Blue   — `IMorphoBlue(lender).flashLoan(...)`
//!     - 2 = ERC-3156      — `IERC3156FlashLender(lender).flashLoan(...)`
//!     - 3 = Uniswap V3    — REVERT STUB per ADR-007 / L-3 hardening
//!     - 4 = reserved      — phased in for Balancer V2 (CLAUDE.md ADR-027); currently reverts
//!     - 5 = reserved      — phased in for Balancer V3 transient-unlock (ADR-027); currently reverts
//! - `DexKind` ordinals: `UniV2Style=0, UniV3Pool=1, UniV4PoolManager=2,
//!   Curve=3, Reserved=4 (Aerodrome — Base-only), AggregatorV6=5,
//!   MorphoBlueAction=6, Algebra=7, Solidly=8, CurveNG=9, BalancerV2=10`,
//!   Phase F.3: `MaverickV2=11, DodoPmm=12, FluidDex=13`,
//!   registry POC slots: `BalancerV3=14` through `Native=28`
//!   (from `IExecutor.sol`).
//!
//! Any deviation is a wire-protocol break — locked by `tests/fixtures_lock.rs`
//! against the cross-language fixture file at
//! `coordinator/src/types/fixtures.json`.
//!
//! ## sol! form
//!
//! We use free-standing `struct` + `function` declarations rather than
//! wrapping them in an `interface { ... }` block. This produces the named
//! call structs `executeNativeArbCall`, `matchInternalCall`,
//! `composeFourLegCall` with their `SELECTOR` constants and `abi_encode`
//! methods — exactly what the engine needs to compose calldata against the
//! Executor.

use alloy::sol;

sol! {
    /// Flash-route selector mirrored from `IFlashLoanInterfaces.sol` and
    /// `Executor._triggerFlashLoan`.
    ///
    /// Implemented on-chain (0..2):
    ///   0 = Aave V3
    ///   1 = Morpho Blue
    ///   2 = ERC-3156
    ///
    /// Reserved (revert stubs on-chain; reachable by ABI but not by
    /// execution path):
    ///   3 = Uniswap V3        (L-3 hardening — `revert Executor__InvalidActionData`)
    ///   4 = Balancer V2       (ADR-027 phase-in — not yet wired)
    ///   5 = Balancer V3       (ADR-027 phase-in — transient unlock; not yet wired)
    type FlashProtocol is uint8;

    /// DEX-implementation hint mirrored from `IExecutor.sol`. Ordinals are
    /// append-only; reordering breaks ABI parity with the contract and the
    /// TS / Python mirrors.
    ///
    /// Existing variants (Phases A–G):
    ///   0 = Uniswap V2-style pair direct (incl. Sushi V2, Camelot V2, Pancake V2)
    ///   1 = Uniswap V3-style router call (incl. Sushi V3, PancakeSwap V3)
    ///   2 = Uniswap V4 PoolManager `unlock`
    ///   3 = Curve (legacy stable, int128 indices)
    ///   4 = reserved (formerly Aerodrome — Base-only; not used on Arbitrum)
    ///   5 = Aggregator V6 (1inch / 0x / Paraswap / Odos / Kyber / OpenOcean /
    ///                       Universal Router V4 — must be in router whitelist)
    ///   6 = Morpho Blue action (singleton-pinned standard liquidation)
    ///
    /// Phase E additions (2026-05):
    ///   7  = Algebra            (Camelot V3 dynamic-fee pools)
    ///   8  = Solidly            (Ramses / Chronos / Solidlizard — stable/volatile)
    ///   9  = CurveNG            (Curve new-gen pools using uint256 indices)
    ///   10 = BalancerV2         (Vault.swap byPoolId)
    ///
    /// Phase F.3 promotions (2026-05-12) — pool-direct standalone kinds
    /// formerly routed via AggregatorV6:
    ///   11 = MaverickV2         (Boosted Position binmap; pool-direct call)
    ///   12 = DodoPmm            (PMM v2 proactive market maker)
    ///   13 = FluidDex           (Smart Debt / Smart Collateral pools)
    ///
    /// Phase registry POC slots (2026-05-13) — protocol-specific router calls
    /// represented in off-chain SwapStep wiring; all execute via generic approve+call:
    ///   14 = BalancerV3
    ///   15 = KyberElastic
    ///   16 = LFJLiquidityBook
    ///   17 = GMXV2
    ///   18 = Wombat
    ///   19 = Bebop
    ///   20 = Hashflow
    ///   21 = WooFi
    ///   22 = OKXDex
    ///   23 = Enso
    ///   24 = Squid
    ///   25 = LiFi
    ///   26 = Rango
    ///   27 = Rubic
    ///   28 = Native
    type DexKind is uint8;

    /// One leg of a swap chain. See `Executor._runSwapChain` for execution
    /// semantics. Field order MUST match `IExecutor.sol`.
    ///
    /// `Debug` and `Clone` are auto-derived by the `sol!` macro on every
    /// generated struct, so we don't add them explicitly (would conflict).
    struct SwapStep {
        DexKind dexKind;
        address router;
        bytes callData;
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint256 amountOutMin;
    }

    /// Params for `executeNativeArb` (Pick D3 / native arbitrage).
    struct NativeArbParams {
        address flashLender;
        FlashProtocol flashProtocol;
        address flashToken;
        uint256 flashAmount;
        SwapStep[] swaps;
        uint256 minProfit;
        uint256 deadline;
    }

    /// Params for the disabled `executeOwnedSwaps` ABI surface. v1
    /// production execution is strict zero-working-capital; the contract
    /// keeps this struct only for ABI continuity and always reverts the entry
    /// after auth / pause / reentrancy guards. Field order MUST match
    /// `IExecutor.sol`.
    struct OwnedSwapsParams {
        SwapStep[] swaps;
        address profitToken;
        uint256 minProfit;
        uint256 deadline;
    }

    /// Params for `matchInternal` (Pick A — internal CoW + UniswapX matching).
    /// `settlementFundingToken` / `settlementFundingMax` are the ADR-029
    /// settlement-funding commitment (audit 2026-06-09 hardening) — `(0x0, 0)`
    /// when there is no CoW leg.
    struct MatchParams {
        bytes cowSettlementCalldata;
        bytes uniswapxBatchCalldata;
        address[] expectedTokenInflows;
        uint256[] expectedTokenInflowMin;
        address flashLender;
        FlashProtocol flashProtocol;
        address flashToken;
        uint256 flashAmount;
        uint256 minProfit;
        uint256 deadline;
        address settlementFundingToken;
        uint256 settlementFundingMax;
    }

    /// Params for `composeFourLeg` (Pick B — 4-leg cross-protocol composition).
    /// `settlementFundingToken` / `settlementFundingMax` are the ADR-029
    /// settlement-funding commitment (audit 2026-06-09 hardening) — `(0x0, 0)`
    /// when Leg 3 is empty.
    struct ComposeParams {
        bytes acrossFillCalldata;
        SwapStep[] arbSwaps;
        bytes cowFillCalldata;
        bytes uniswapxRebalanceCalldata;
        address flashLender;
        FlashProtocol flashProtocol;
        address flashToken;
        uint256 flashAmount;
        uint256 minProfit;
        uint256 deadline;
        address settlementFundingToken;
        uint256 settlementFundingMax;
    }

    /// Params for `triggerCoWFlashLoanRouter` (Pick C — solver-side user entry
    /// into the CoW Router walk). The strategist computes `expectedRoot` off-chain
    /// via the same `_chainCowHash` subset shape as the on-chain
    /// `_continueCoWFlashLoanRouter`. `initialLoanCalldata` is opaque to this
    /// contract; it's forwarded verbatim to `COW_FLASH_LOAN_ROUTER`. Field
    /// order MUST match `IExecutor.sol:159`.
    struct CoWFlashLoanRouterStartParams {
        bytes32 expectedRoot;
        uint256 totalRounds;
        bytes initialLoanCalldata;
        uint256 deadline;
    }

    /// Strategy entry points exposed by `Executor.sol`.
    ///
    /// Declared as free-standing functions so the macro emits separately
    /// named `<name>Call` types — each with `SELECTOR: [u8; 4]` and
    /// `abi_encode() -> Vec<u8>`.
    function executeNativeArb(NativeArbParams calldata p) external;
    function executeOwnedSwaps(OwnedSwapsParams calldata p) external;
    function matchInternal(MatchParams calldata p) external;
    function composeFourLeg(ComposeParams calldata p) external;
    function triggerCoWFlashLoanRouter(CoWFlashLoanRouterStartParams calldata p) external;

    /// ADR-029 Pick A settlement-funding helper. Callable only from inside an
    /// active matchInternal / composeFourLeg flow by `COW_SETTLEMENT` (issued
    /// via `interactions[1]` during the CoW `settle()` loop). The strategist
    /// encodes a call to this selector into `interactions[1]`'s callData so the
    /// Executor forwards `amount` of `token` to the Settlement contract during
    /// the same atomic flow. Reverts `Executor__NotSettlement` for any other
    /// caller, `Executor__NoActiveFlow` outside an active strategy entry.
    function transferToSettlement(address token, uint256 amount) external;
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::sol_types::SolCall;

    /// Locked function selector — the contract's external interface must
    /// produce this 4-byte prefix when `keccak256` is taken over the
    /// canonical Solidity signature. Drift here = ABI break.
    #[test]
    fn execute_native_arb_selector_locked() {
        assert_eq!(executeNativeArbCall::SELECTOR, [0xf6, 0xf6, 0xad, 0xd1]);
    }

    #[test]
    fn match_internal_selector_locked() {
        // 12-field MatchParams (incl. ADR-029 settlement-funding). `cast keccak
        // 'matchInternal((bytes,bytes,address[],uint256[],address,uint8,address,uint256,uint256,uint256,address,uint256))'`
        // → 0x210ea473
        assert_eq!(matchInternalCall::SELECTOR, [0x21, 0x0e, 0xa4, 0x73]);
    }

    #[test]
    fn compose_four_leg_selector_locked() {
        // 12-field ComposeParams (incl. ADR-029 settlement-funding) → 0x2e1977d2
        assert_eq!(composeFourLegCall::SELECTOR, [0x2e, 0x19, 0x77, 0xd2]);
    }

    #[test]
    fn execute_owned_swaps_selector_locked() {
        // Disabled owned-inventory ABI surface retained only for compatibility.
        // Drift here still matters because historical callers must hit the
        // contract's explicit `Executor__OwnedInventoryDisabled` path.
        assert_eq!(executeOwnedSwapsCall::SELECTOR, [0xba, 0x44, 0x42, 0x0d]);
    }

    #[test]
    fn trigger_cow_flash_loan_router_selector_locked() {
        // Pick C user-side entry. `cast keccak
        // 'triggerCoWFlashLoanRouter((bytes32,uint256,bytes,uint256))'`
        // → 0x900866ce
        assert_eq!(
            triggerCoWFlashLoanRouterCall::SELECTOR,
            [0x90, 0x08, 0x66, 0xce],
        );
    }

    #[test]
    fn transfer_to_settlement_selector_locked() {
        // ADR-029 settlement-funding callback. `cast keccak
        // 'transferToSettlement(address,uint256)'` → 0x4515dd0f
        assert_eq!(transferToSettlementCall::SELECTOR, [0x45, 0x15, 0xdd, 0x0f]);
    }

    /// User-defined value types over `uint8` must wrap and unwrap a `u8`
    /// through `From<u8>` and `Into<u8>` (alloy's newtype convention). This
    /// guards the `wire::WireXxx -> sol::Xxx` bridge in `wire.rs`.
    #[test]
    fn flash_protocol_u8_round_trip() {
        for raw in 0u8..=5 {
            let fp: FlashProtocol = FlashProtocol::from(raw);
            let back: u8 = fp.into();
            assert_eq!(raw, back);
        }
    }

    #[test]
    fn dex_kind_u8_round_trip() {
        // Walk the full registry POC ordinal range (0..=28). Append-only ordinals
        // mean this test guards the Solidity comment block + TS / Python
        // mirrors against a silent renumber.
        for raw in 0u8..=28 {
            let dk: DexKind = DexKind::from(raw);
            let back: u8 = dk.into();
            assert_eq!(raw, back);
        }
    }

    /// Phase F.3 (2026-05-12): the three new ordinals must lift through the
    /// `sol!`-generated newtype with the locked numeric values. Same shape
    /// as the Phase E lock — pin each one explicitly to catch a refactor
    /// that silently shifts an ordinal by editing only the comment block.
    #[test]
    fn dex_kind_phase_f3_ordinals_locked() {
        let maverick_v2: u8 = DexKind::from(11u8).into();
        let dodo_pmm: u8 = DexKind::from(12u8).into();
        let fluid_dex: u8 = DexKind::from(13u8).into();
        assert_eq!(maverick_v2, 11, "DexKind::MaverickV2 ordinal");
        assert_eq!(dodo_pmm, 12, "DexKind::DodoPmm ordinal");
        assert_eq!(fluid_dex, 13, "DexKind::FluidDex ordinal");
    }

    /// Registry POC (2026-05-13): protocol-specific router-call lanes remain
    /// raw `u8` newtype ordinals, so pin every append-only value explicitly.
    #[test]
    fn dex_kind_registry_poc_ordinals_locked() {
        let balancer_v3: u8 = DexKind::from(14u8).into();
        let kyber_elastic: u8 = DexKind::from(15u8).into();
        let lfj_liquidity_book: u8 = DexKind::from(16u8).into();
        let gmx_v2: u8 = DexKind::from(17u8).into();
        let wombat: u8 = DexKind::from(18u8).into();
        let bebop: u8 = DexKind::from(19u8).into();
        let hashflow: u8 = DexKind::from(20u8).into();
        let woofi: u8 = DexKind::from(21u8).into();
        let okx_dex: u8 = DexKind::from(22u8).into();
        let enso: u8 = DexKind::from(23u8).into();
        let squid: u8 = DexKind::from(24u8).into();
        let lifi: u8 = DexKind::from(25u8).into();
        let rango: u8 = DexKind::from(26u8).into();
        let rubic: u8 = DexKind::from(27u8).into();
        let native: u8 = DexKind::from(28u8).into();
        assert_eq!(balancer_v3, 14, "DexKind::BalancerV3 ordinal");
        assert_eq!(kyber_elastic, 15, "DexKind::KyberElastic ordinal");
        assert_eq!(lfj_liquidity_book, 16, "DexKind::LFJLiquidityBook ordinal");
        assert_eq!(gmx_v2, 17, "DexKind::GMXV2 ordinal");
        assert_eq!(wombat, 18, "DexKind::Wombat ordinal");
        assert_eq!(bebop, 19, "DexKind::Bebop ordinal");
        assert_eq!(hashflow, 20, "DexKind::Hashflow ordinal");
        assert_eq!(woofi, 21, "DexKind::WooFi ordinal");
        assert_eq!(okx_dex, 22, "DexKind::OKXDex ordinal");
        assert_eq!(enso, 23, "DexKind::Enso ordinal");
        assert_eq!(squid, 24, "DexKind::Squid ordinal");
        assert_eq!(lifi, 25, "DexKind::LiFi ordinal");
        assert_eq!(rango, 26, "DexKind::Rango ordinal");
        assert_eq!(rubic, 27, "DexKind::Rubic ordinal");
        assert_eq!(native, 28, "DexKind::Native ordinal");
    }

    /// Phase E (2026-05): the four new ordinals must lift through the
    /// `sol!`-generated newtype with the locked numeric values. Pinning
    /// each one explicitly catches a refactor that silently shifts an
    /// ordinal by editing only the comment block.
    #[test]
    fn dex_kind_phase_e_ordinals_locked() {
        let algebra: u8 = DexKind::from(7u8).into();
        let solidly: u8 = DexKind::from(8u8).into();
        let curve_ng: u8 = DexKind::from(9u8).into();
        let balancer_v2: u8 = DexKind::from(10u8).into();
        assert_eq!(algebra, 7, "DexKind::Algebra ordinal");
        assert_eq!(solidly, 8, "DexKind::Solidly ordinal");
        assert_eq!(curve_ng, 9, "DexKind::CurveNG ordinal");
        assert_eq!(balancer_v2, 10, "DexKind::BalancerV2 ordinal");
    }
}
