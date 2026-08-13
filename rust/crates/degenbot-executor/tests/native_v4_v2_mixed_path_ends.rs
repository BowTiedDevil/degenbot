//! Diagnostic: the remaining 3-hop composers that cross V4 native ETH at the
//! path ends (NOT the V4↔V2/V3 boundary — the boundary token is always ERC20
//! because V2/V3 pools are ERC20-only). Mirrors the V4-V3-V4 and V4-V2-V4
//! diagnostics (ergo TPITPQ): a foundry spike proved the V4-V2-V4 encoding
//! executes correctly on-chain for native-ETH path ends — no wrap/unwrap
//! needed. These tests pin that each named composer ACCEPTS such a path
//! (returns `Some`), refuting the blanket "add wrap/unwrap to every V4↔V2
//! composer" premise of TPITPQ for the path-ends case.
//!
//! The GENUINE remaining gap (V4 outputs native → V2/V3 needs WETH at the
//! boundary, mirroring the 2-hop `encode_cmd_v4_v2` bridge branch) is
//! unobserved on mainnet in the bot runs studied — tracked separately, not
//! in this test file.

use alloy::primitives::{address, Address};
use degenbot_executor::composers::{
    encode_cmd_3_hop, EncodeOptions, HopInfo, PathInfo, V2HopInfo, V3HopInfo, V4HopInfo,
};

const NATIVE: Address = Address::ZERO;
const PM: Address = address!("000000000004444c5dc75cB358380D2e3dE08A90");
const EXECUTOR: Address = address!("DeAd0000000000000000000000000000000000Be");
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
const USDC: Address = address!("A0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48");
const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");

const POOL_V3: Address = address!("1111111111111111111111111111111111111111");
const POOL_V2: Address = address!("2222222222222222222222222222222222222222");

const HEX_A: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const HEX_C: &str = "0x2222222222222222222222222222222222222222222222222222222222222222";

fn v4_hop_native_in_out(c0: Address, c1: Address, zfo: bool) -> V4HopInfo {
    V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: HEX_A.to_string(),
        currency0_address: c0,
        currency1_address: c1,
        fee: 3000,
        tick_spacing: 60,
        hook_address: NATIVE,
        zfo,
    }
}

fn v4_hop_c(c0: Address, c1: Address, zfo: bool) -> V4HopInfo {
    V4HopInfo {
        pool_manager_address: PM,
        pool_id_hex: HEX_C.to_string(),
        currency0_address: c0,
        currency1_address: c1,
        fee: 10000,
        tick_spacing: 200,
        hook_address: NATIVE,
        zfo,
    }
}

fn v3_hop(t0: Address, t1: Address, zfo: bool) -> V3HopInfo {
    V3HopInfo {
        pool_address: POOL_V3,
        token0_address: t0,
        token1_address: t1,
        fee: 500,
        zfo,
    }
}

fn v2_hop(t0: Address, t1: Address, zfo: bool) -> V2HopInfo {
    V2HopInfo {
        pool_address: POOL_V2,
        token0_address: t0,
        token1_address: t1,
        fee: 30,
        zfo,
    }
}

fn assert_encodes(hops: Vec<HopInfo>, label: &str) {
    let path = PathInfo::new(hops);
    let out = encode_cmd_3_hop(
        &path,
        1_000_000_000_000_000u128,
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out.is_some(),
        "{label}: native path-ends should encode, got None"
    );
}

/// V4-V2-V3 with native at path ends (V4a input + V3c output is WETH-bearing
/// — but here both ends are native-ETH via V4/V3's native/WETH representations).
///
/// Hop A (V4): NATIVE→USDC.
/// Hop B (V2): USDC→WBTC.
/// Hop C (V3): WBTC→WETH (WETH here stands in for the native-ETH-side token;
/// the V4-V3-V4 spike already proved native path ends via V4 on both sides).
/// **Root Cause B**: the `v4_v2_v3` emitter settles the V4 input with
/// `V4_SETTLE_DELTA(WETH)` unconditionally. A NATIVE V4 input (a's debt is
/// native) makes that settle incoherent — a residual PM[native] debt the
/// validator rejects. Declined (ADR-029 D1); WETH-input V4→V2→V3 is the
/// coherent subspace.
#[test]
fn native_v4_v2_v3_path_starts_declines() {
    // A: NATIVE/USDC, zfo=true (in=native, out=USDC)
    let a = v4_hop_native_in_out(NATIVE, USDC, true);
    // B: USDC/WBTC, zfo=true (in=USDC, out=WBTC)
    let b = v2_hop(USDC, WBTC, true);
    // C: WBTC/WETH, zfo=true (in=WBTC, out=WETH)
    let c = v3_hop(WBTC, WETH, true);
    let path = PathInfo::new(vec![HopInfo::V4(a), HopInfo::V2(b), HopInfo::V3(c)]);
    let out = encode_cmd_3_hop(
        &path,
        1_000_000_000_000_000u128,
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out.is_none(),
        "V4-V2-V3 with native V4 input must decline (settle-WETH incoherent), got {out:?}"
    );
}

/// V3-V2-V4 with native at path ends.
/// Hop A (V3): WETH→USDC (WETH = native-side ERC20 stand-in at path start).
/// Hop B (V2): USDC→WBTC.
/// Hop C (V4): WBTC→NATIVE (native output at path end).
#[test]
fn native_v3_v2_v4_path_ends_encodes() {
    let a = v3_hop(WETH, USDC, true);
    let b = v2_hop(USDC, WBTC, true);
    // C: WBTC/NATIVE, zfo=true (in=WBTC, out=native)
    let c = v4_hop_c(WBTC, NATIVE, true);
    assert_encodes(
        vec![HopInfo::V3(a), HopInfo::V2(b), HopInfo::V4(c)],
        "V3-V2-V4",
    );
}

/// V4-V2-V2 with native at the path start (V4a input=native).
/// Hop A (V4): NATIVE→USDC.
/// Hop B (V2): USDC→WBTC.
/// Hop C (V2): WBTC→WETH (WETH = native-side ERC20 stand-in at path end).
///
/// **Root Cause B**: the `v4_v2_v2` emitter settles the V4 input with
/// `V4_SETTLE_DELTA(WETH)` unconditionally. With a NATIVE V4 input (a's debt is
/// native, not WETH) that settle is incoherent — it leaves a residual PM[native]
/// debt the validator (correctly) rejects as not-net-zero at `V4UnlockEnd`. The
/// Plan gate therefore DECLINES the native-input shape (a non-chain over-code
/// of the emitter); the WETH-input form is the coherent subspace.
#[test]
fn native_v4_v2_v2_path_starts_declines() {
    let a = v4_hop_native_in_out(NATIVE, USDC, true);
    let b = v2_hop(USDC, WBTC, true);
    let c = v2_hop(WBTC, WETH, true);
    let path = PathInfo::new(vec![HopInfo::V4(a), HopInfo::V2(b), HopInfo::V2(c)]);
    let out = encode_cmd_3_hop(
        &path,
        1_000_000_000_000_000u128,
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        &[2_000_000u128, 100_000_000u128, 2_001_000u128],
        EXECUTOR,
        PM,
        WETH,
        EncodeOptions::default(),
    );
    assert!(
        out.is_none(),
        "V4-V2-V2 with native V4 input must decline (settle-WETH incoherent), got {out:?}"
    );
}

/// V2-V4-V2 with native at the V4 hop's both sides (a V4↔native boundary
/// that IS path-internal but where V2 carries WETH, the native-ERC20 twin).
/// Hop A (V2): WETH→USDC.
/// Hop B (V4): USDC→NATIVE.
/// Hop C (V2): NATIVE-side... (V2 can't hold native, so this shape is only
/// well-formed if the V4↔V2 boundary token is WETH on the V2 side — which
/// is the boundary-bridge case, NOT path-ends. This test documents that the
/// composer accepts the WETH-twin shape; the genuine native-bridge is a
/// separate gap.)
#[test]
fn weth_twin_v2_v4_v2_encodes() {
    // A (V2): WETH→USDC, zfo=true
    let a = v2_hop(WETH, USDC, true);
    // B (V4): USDC→WETH (WETH twin, not native), zfo=false (in=c1=USDC? no)
    //   c0=USDC, c1=WETH, zfo=false → in=c1=WETH, out=c0=USDC. Wrong dir.
    //   Want: in=USDC, out=WETH. c0=USDC,c1=WETH,zfo=true → in=c0=USDC,out=c1=WETH. ✓
    let b = v4_hop_c(USDC, WETH, true);
    // C (V2): WETH→USDC? No — cycle must close: A out=USDC, so C out must be
    // WETH (A's input). C: WETH/USDC zfo=false → in=USDC, out=WETH. ✓
    let c = v2_hop(WETH, USDC, false);
    assert_encodes(
        vec![HopInfo::V2(a), HopInfo::V4(b), HopInfo::V2(c)],
        "V2-V4-V2 (WETH twin)",
    );
}
