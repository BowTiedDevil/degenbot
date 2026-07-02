"""Generate the Rust byte-parity test for the 3-hop command-stream composers.

Calls :func:`examples.eth_backrun_helpers._encode_cmd_3_hop` over representative
inputs across every 3-hop path type (all 27 V2/V3/V4 combinations), plus the
``use_v4_batch`` / ``erc6909_profit`` variants for V4-V4-V4.

Emits ``tests/composers_3hop_parity.rs`` whose assertions pin byte-exact output.
Re-run with::

    uv run python rust/crates/degenbot-executor/tests/fixtures/generate_composers_3hop_parity.py

The generated file references ``degenbot_executor::composers::*`` via the
``encode_cmd_stream`` dispatcher (sets 3-hop arm into motion).
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO_ROOT / "examples"))
sys.path.insert(0, str(REPO_ROOT / "src"))

import eth_backrun_helpers as ebh  # noqa: E402
from degenbot.arbitrage.hop_info import (  # noqa: E402
    PathInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
)

# ── Addresses ──
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"
WBTC = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
EXECUTOR = "0xDeAd0000000000000000000000000000000000Be"
PM = "0x000000000004444c5dc75cB358380D2e3dE08A90"
ZERO = "0x0000000000000000000000000000000000000000"

# Amounts (all < 2^127 so the int128 guard passes).
ONE_E18 = 1 * 10**18
TWO_K_USDC = 2_000 * 10**6
TWO_OH_ONE_E18 = 2_001_000_000_000_000_000  # profit over 1e18
TWO_K_ONE_USDC = 2_001 * 10**6  # profit over 2k
WBTC_OUT = 35_000_000  # 0.35 WBTC (sats)
HOOKS = ZERO
V4PID = "0x" + "11" * 32  # opaque to the composer

CASES: list[tuple[str, str, bytes | None]] = []  # (label, rust_expr, expected)


def v2(pool, t0, t1, fee, zfo):
    return V2HopInfo(pool_address=pool, token0_address=t0, token1_address=t1, fee=fee, zfo=zfo)


def v3(pool, t0, t1, fee, zfo):
    return V3HopInfo(pool_address=pool, token0_address=t0, token1_address=t1, fee=fee, zfo=zfo)


def v4(pm, pid, c0, c1, fee, ts, zfo):
    return V4HopInfo(
        pool_manager_address=pm,
        pool_id_hex=pid,
        currency0_address=c0,
        currency1_address=c1,
        fee=fee,
        tick_spacing=ts,
        hook_address=HOOKS,
        zfo=zfo,
    )


def addr_rust(a: str) -> str:
    return f'address!("{a[2:]}")'


def v2_rust(h: V2HopInfo) -> str:
    return (
        "HopInfo::V2(V2HopInfo{"
        f"pool_address:{addr_rust(h.pool_address)},"
        f"token0_address:{addr_rust(h.token0_address)},"
        f"token1_address:{addr_rust(h.token1_address)},"
        f"fee:{h.fee}u16,zfo:{str(h.zfo).lower()}}})"
    )


def v3_rust(h: V3HopInfo) -> str:
    return (
        "HopInfo::V3(V3HopInfo{"
        f"pool_address:{addr_rust(h.pool_address)},"
        f"token0_address:{addr_rust(h.token0_address)},"
        f"token1_address:{addr_rust(h.token1_address)},"
        f"fee:{h.fee}u32,zfo:{str(h.zfo).lower()}}})"
    )


def v4_rust(h: V4HopInfo) -> str:
    return (
        "HopInfo::V4(V4HopInfo{"
        f"pool_manager_address:{addr_rust(h.pool_manager_address)},"
        f'pool_id_hex:"{h.pool_id_hex}".to_string(),'
        f"currency0_address:{addr_rust(h.currency0_address)},"
        f"currency1_address:{addr_rust(h.currency1_address)},"
        f"fee:{h.fee}u32,tick_spacing:{h.tick_spacing}i32,"
        f"hook_address:{addr_rust(h.hook_address)},zfo:{str(h.zfo).lower()}}})"
    )


def path_rust(hops: list) -> str:
    body = ", ".join(
        v2_rust(h) if isinstance(h, V2HopInfo)
        else v3_rust(h) if isinstance(h, V3HopInfo)
        else v4_rust(h)
        for h in hops
    )
    return f"PathInfo::new(vec![{body}])"


def rs_bytes(b: bytes) -> str:
    if not b:
        return 'b""'
    return 'b"' + "".join(f"\\x{x:02x}" for x in b) + '"'


def opts_rust(opts: dict | None) -> str:
    if opts:
        return (
            ",EncodeOptions{erc6909_profit:"
            + str(opts.get("erc6909_profit", False)).lower()
            + ",use_v4_batch:"
            + str(opts.get("use_v4_batch", False)).lower()
            + "}"
        )
    return ",EncodeOptions::default()"


def enc_3hop_case(
    label: str,
    hops: list,
    optimal_input: int,
    hop_outputs: tuple,
    opts: dict | None = None,
):
    """Call the Python _encode_cmd_3_hop oracle and record the expected bytes."""
    path_info = PathInfo(hops=list(hops))
    expected = ebh._encode_cmd_3_hop(
        path_info=path_info,
        optimal_input=optimal_input,
        hop_outputs=hop_outputs,
        executor_address=EXECUTOR,
        pool_manager_address=PM,
        weth_address=WETH,
        **(opts or {}),
    )
    rust_path = path_rust(hops)
    outs = ", ".join(f"{o}u128" for o in hop_outputs)
    rust_opts = opts_rust(opts)
    rust = (
        "encode_cmd_3_hop(&" + rust_path + "," + str(optimal_input) + "u128,&["
        + outs + "]," + addr_rust(EXECUTOR) + "," + addr_rust(PM) + ","
        + addr_rust(WETH) + rust_opts + ")"
    )
    CASES.append((label, rust, expected))


# ── Pool addresses for V2/V3 hops (unique per pattern for distinct table slots)
P2A = "0x1111111111111111111111111111111111111111"
P2B = "0x2222222222222222222222222222222222222222"
P2C = "0x3333333333333333333333333333333333333333"
P3A = "0x4444444444444444444444444444444444444444"
P3B = "0x5555555555555555555555555555555555555555"
P3C = "0x6666666666666666666666666666666666666666"

# Common hop configurations for each pattern:
# ha: WETH→USDC (zfo=True: output=token1=USDC)
# hb: USDC→?  (zfo=True: output=token1=?)
# hc: ?→WETH  (zfo=True: output=token1=WETH)


# V2-V2-V2: WETH→USDC, USDC→WETH, WETH→USDC (same currency in/out)
enc_3hop_case(
    "v2_v2_v2",
    [v2(P2A, WETH, USDC, 30, True), v2(P2B, USDC, WETH, 30, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V2-V3
enc_3hop_case(
    "v2_v2_v3",
    [v2(P2A, WETH, USDC, 30, True), v2(P2B, USDC, WETH, 30, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V2-V4
enc_3hop_case(
    "v2_v2_v4",
    [v2(P2A, WETH, USDC, 30, True), v2(P2B, USDC, WETH, 30, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V3-V2
enc_3hop_case(
    "v2_v3_v2",
    [v2(P2A, WETH, USDC, 30, True), v3(P3B, USDC, WETH, 500, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V3-V3
enc_3hop_case(
    "v2_v3_v3",
    [v2(P2A, WETH, USDC, 30, True), v3(P3B, USDC, WETH, 500, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V3-V4: out_c is V4 swap's WETH output — must exceed optimal_input for the
# `enc_v4_take_compact(weth, executor, out_c - optimal_input)` to be positive.
enc_3hop_case(
    "v2_v3_v4",
    [v2(P2A, WETH, USDC, 30, True), v3(P3B, USDC, WETH, 500, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_OH_ONE_E18),
)

# V2-V4-V2
enc_3hop_case(
    "v2_v4_v2",
    [v2(P2A, WETH, USDC, 30, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V4-V3
enc_3hop_case(
    "v2_v4_v3",
    [v2(P2A, WETH, USDC, 30, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V2-V4-V4
enc_3hop_case(
    "v2_v4_v4",
    [v2(P2A, WETH, USDC, 30, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V2-V2
enc_3hop_case(
    "v3_v2_v2",
    [v3(P3A, WETH, USDC, 3000, True), v2(P2B, USDC, WETH, 30, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V2-V3
enc_3hop_case(
    "v3_v2_v3",
    [v3(P3A, WETH, USDC, 3000, True), v2(P2B, USDC, WETH, 30, True), v3(P3C, WETH, USDC, 500, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V2-V4
enc_3hop_case(
    "v3_v2_v4",
    [v3(P3A, WETH, USDC, 3000, True), v2(P2B, USDC, WETH, 30, True), v4(PM, V4PID, WETH, USDC, 500, 10, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V3-V2
enc_3hop_case(
    "v3_v3_v2",
    [v3(P3A, WETH, USDC, 3000, True), v3(P3B, USDC, WETH, 500, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V3-V3
enc_3hop_case(
    "v3_v3_v3",
    [v3(P3A, WETH, USDC, 3000, True), v3(P3B, USDC, WETH, 500, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V3-V4
enc_3hop_case(
    "v3_v3_v4",
    [v3(P3A, WETH, USDC, 3000, True), v3(P3B, USDC, WETH, 500, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V4-V2
enc_3hop_case(
    "v3_v4_v2",
    [v3(P3A, WETH, USDC, 3000, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V4-V3
enc_3hop_case(
    "v3_v4_v3",
    [v3(P3A, WETH, USDC, 3000, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V3-V4-V4
enc_3hop_case(
    "v3_v4_v4",
    [v3(P3A, WETH, USDC, 3000, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V2-V2
enc_3hop_case(
    "v4_v2_v2",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v2(P2B, USDC, WETH, 30, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V2-V3
enc_3hop_case(
    "v4_v2_v3",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v2(P2B, USDC, WETH, 30, True), v3(P3C, WETH, USDC, 500, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V2-V4
enc_3hop_case(
    "v4_v2_v4",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v2(P2B, USDC, WETH, 30, True), v4(PM, V4PID, WETH, USDC, 500, 10, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V3-V2
enc_3hop_case(
    "v4_v3_v2",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v3(P3B, USDC, WETH, 500, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V3-V3
enc_3hop_case(
    "v4_v3_v3",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v3(P3B, USDC, WETH, 500, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V3-V4
enc_3hop_case(
    "v4_v3_v4",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v3(P3B, USDC, WETH, 500, True), v4(PM, V4PID, WETH, USDC, 3000, 10, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V4-V2
enc_3hop_case(
    "v4_v4_v2",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v2(P2C, WETH, USDC, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V4-V3
enc_3hop_case(
    "v4_v4_v3",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v3(P3C, WETH, USDC, 3000, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V4-V4 (standard: V4_TAKE_DELTA profit)
enc_3hop_case(
    "v4_v4_v4",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
)

# V4-V4-V4 with use_v4_batch
enc_3hop_case(
    "v4_v4_v4_batch",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_K_ONE_USDC),
    opts={"use_v4_batch": True},
)

# V4-V4-V4 with erc6909_profit — out_c must exceed optimal_input for profit.
enc_3hop_case(
    "v4_v4_v4_erc6909",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True), v4(PM, V4PID, USDC, WETH, 500, 10, True), v4(PM, V4PID, WETH, USDC, 3000, 60, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18, TWO_OH_ONE_E18),
    opts={"erc6909_profit": True},
)


# ── Emit the Rust test file ──
def emit() -> str:
    lines: list[str] = []
    lines.append("// AUTO-GENERATED by tests/fixtures/generate_composers_3hop_parity.py.")
    lines.append("// Byte-exact parity vs the Python oracle:")
    lines.append("//   examples/eth_backrun_helpers.py::_encode_cmd_3_hop (27 patterns +")
    lines.append("//   use_v4_batch / erc6909_profit variants for V4-V4-V4).")
    lines.append("// Do not edit by hand — re-run the generator to refresh (§4.2 gate).")
    lines.append("")
    lines.append("#![allow(")
    lines.append("    clippy::too_many_lines,")
    lines.append("    clippy::unreadable_literal,")
    lines.append("    clippy::needless_pass_by_value,")
    lines.append(")]")
    lines.append("")
    lines.append("use alloy::primitives::address;")
    lines.append("use degenbot_executor::composers::{")
    lines.append("    EncodeOptions, HopInfo, PathInfo,")
    lines.append("    V2HopInfo, V3HopInfo, V4HopInfo, encode_cmd_3_hop,")
    lines.append("};")
    lines.append("")
    lines.append("fn hx(s: &[u8]) -> Vec<u8> {")
    lines.append("    s.to_vec()")
    lines.append("}")
    lines.append("")

    for label, rust_expr, expected in CASES:
        lines.append("#[test]")
        lines.append(f"fn parity_{label}() {{")
        if expected is None:
            lines.append("    // Python oracle returned None — unsupported path.")
            lines.append(f"    let rust = {rust_expr};")
            lines.append("    assert!(rust.is_none());")
        else:
            lines.append(f"    let rust = {rust_expr};")
            lines.append(f"    assert_eq!(rust, Some(hx({rs_bytes(expected)})));")
        lines.append("}")
        lines.append("")

    return "\n".join(lines) + "\n"


OUT = REPO_ROOT / "rust/crates/degenbot-executor/tests/composers_3hop_parity.rs"
OUT.write_text(emit())
print(f"wrote {OUT.relative_to(REPO_ROOT)} ({len(CASES)} cases)")