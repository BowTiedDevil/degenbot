"""Generate the Rust byte-parity test for the command-stream encoders.

Calls every ``enc_*`` in ``examples/cmd_stream.py`` (the §4.2 parity oracle)
over representative inputs across every opcode family and emits a Rust test
file (``tests/encoders_parity.rs``) whose assertions pin the byte-exact
outputs. Re-run with:

    uv run python rust/crates/degenbot-executor/tests/fixtures/generate_encoders_parity.py

The generated file references ``degenbot_executor::encoders::*`` and asserts
``enc_*(...) == hex(...)`` for each case. Regenerate whenever the oracle or
the encoder semantics change; the diff is the parity evidence.
"""

from __future__ import annotations

import sys
from pathlib import Path

# Make examples/ importable so we can import the parity oracle cmd_stream.
REPO_ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO_ROOT / "examples"))

import cmd_stream as cs  # noqa: E402  (the Python parity oracle)

# ── Representative addresses ──
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"
WBTC = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
EXECUTOR = "0xDeAd0000000000000000000000000000000000Be"
PM = "0x000000000004444c5dc75cB358380D2e3dE08A90"
ZERO = cs.ZERO_ADDRESS

# A dense set of representative amounts: small, 1e18, a 6-decimal token amount,
# a large-but-in-range value, and zero. All < 2^96 (uint96 bound).
ONE_E18 = 1 * 10**18
TWO_K_USDC = 2_000 * 10**6
LARGE_U96 = 2**96 - 1  # max uint96
MID = 0x0102030405060708090A0B0C

# A uint96 amount that overflows the field (== 2^96) — used for the
# overflow error-path parity check (both sides reject).
OVERFLOW_U96 = 2**96


def hx(b: bytes) -> str:
    """Format bytes as a Rust hex string literal."""
    return "".join(f"\\x{x:02x}" for x in b)


def rs_u128(v: int) -> str:
    return f"{v}u128"


def rs_bytes(b: bytes) -> str:
    if not b:
        return "b\"\""
    inner = "".join(f"\\x{x:02x}" for x in b)
    return f'b"{inner}"'


def rs_u256(v: int) -> str:
    """Format a uint256 as an alloy U256 constructor call."""
    return f"U256::from({v}u128)"


def rs_addr(a: str) -> str:
    return f'address!("{a[2:]}")'


# Each case: (rust_expr_args, python_args, kind) where kind dispatches the
# Rust encoder. ``rust_args`` is a list of Rust literal fragments;
# ``py_args`` is the matching Python arg list/tuple passed to the oracle.
CASES: list[tuple[str, list, list, str]] = []


def add(kind: str, py_fn, py_args, rust_args: list[str]):
    """Record a parity case: run the Python oracle, store the expected bytes."""
    expected = py_fn(*py_args)
    if isinstance(expected, int):
        # pack_config / pack_expected_balance return a uint256; encode as 32 BE bytes.
        expected = expected.to_bytes(32, "big")
    CASES.append((kind, rust_args, py_args, expected))


# ── Preprocessing ──
add("enc_set_address", cs.enc_set_address, [WETH], [rs_addr(WETH)])
add("enc_set_address", cs.enc_set_address, [EXECUTOR], [rs_addr(EXECUTOR)])

# AddressTable + enc_set_addresses + enc_preamble over a realistic table.
_at = cs.AddressTable(
    weth_address=WETH, executor_address=EXECUTOR, pool_manager_address=PM
)
_at.add(USDC)
_at.add(WBTC)
_at.add(ZERO)  # resolves to SENTINEL_NATIVE — NOT added to the table
add("enc_set_addresses", cs.enc_set_addresses, [_at], ["&table"])
add("enc_preamble", cs.enc_preamble, [_at], ["&table"])

# ── ERC20 / ETH / Native (0x10–0x17) ──
add("enc_erc20_transfer", cs.enc_erc20_transfer, [1, 2, ONE_E18], ["1u8", "2u8", rs_u128(ONE_E18)])
add("enc_erc20_transfer", cs.enc_erc20_transfer, [3, 0, LARGE_U96], ["3u8", "0u8", rs_u128(LARGE_U96)])
add("enc_erc20_transfer", cs.enc_erc20_transfer, [0, 0, 0], ["0u8", "0u8", "0u128"])
add("enc_erc20_xfer_balance", cs.enc_erc20_xfer_balance, [1, 2], ["1u8", "2u8"])
add("enc_weth_deposit", cs.enc_weth_deposit, [ONE_E18], [rs_u256(ONE_E18)])
add("enc_weth_deposit", cs.enc_weth_deposit, [0], [rs_u256(0)])
add("enc_weth_withdraw", cs.enc_weth_withdraw, [TWO_K_USDC], [rs_u256(TWO_K_USDC)])
add("enc_weth_deposit_all", cs.enc_weth_deposit_all, [], [])
add("enc_weth_withdraw_all", cs.enc_weth_withdraw_all, [], [])
add("enc_send_eth", cs.enc_send_eth, [2, ONE_E18], ["2u8", rs_u128(ONE_E18)])
add("enc_send_eth", cs.enc_send_eth, [0, MID], ["0u8", rs_u128(MID)])
add("enc_send_eth_all", cs.enc_send_eth_all, [2], ["2u8"])

# ── V2 (0x20–0x22) ──
add(
    "enc_v2_swap_compact", cs.enc_v2_swap_compact,
    [1, True, ONE_E18, 2, 30, b""],
    ["1u8", "true", rs_u128(ONE_E18), "2u8", "30u16", rs_bytes(b"")],
)
fwd = b"\x11\x22\x33"
add(
    "enc_v2_swap_compact", cs.enc_v2_swap_compact,
    [2, False, TWO_K_USDC, 0, 25, fwd],
    ["2u8", "false", rs_u128(TWO_K_USDC), "0u8", "25u16", rs_bytes(fwd)],
)
add(
    "enc_v2_swap_compact", cs.enc_v2_swap_compact,
    [3, True, LARGE_U96, 1, 100, b""],
    ["3u8", "true", rs_u128(LARGE_U96), "1u8", "100u16", rs_bytes(b"")],
)
add(
    "enc_v2_swap_calc", cs.enc_v2_swap_calc,
    [1, True, 2, 30], ["1u8", "true", "2u8", "30u16"],
)
add(
    "enc_v2_swap_calc", cs.enc_v2_swap_calc,
    [2, False, 0, 25], ["2u8", "false", "0u8", "25u16"],
)
add(
    "enc_v2_swap_direct", cs.enc_v2_swap_direct,
    [1, True, ONE_E18, 2], ["1u8", "true", rs_u128(ONE_E18), "2u8"],
)
add(
    "enc_v2_swap_direct", cs.enc_v2_swap_direct,
    [2, False, MID, 0], ["2u8", "false", rs_u128(MID), "0u8"],
)

# ── V3 (0x30–0x31) ──
add(
    "enc_v3_swap_compact", cs.enc_v3_swap_compact,
    [1, True, ONE_E18, 2, b""],
    ["1u8", "true", rs_u128(ONE_E18), "2u8", rs_bytes(b"")],
)
fwd3 = b"\xaa\xbb\xcc\xdd"
add(
    "enc_v3_swap_compact", cs.enc_v3_swap_compact,
    [2, False, TWO_K_USDC, 0, fwd3],
    ["2u8", "false", rs_u128(TWO_K_USDC), "0u8", rs_bytes(fwd3)],
)
add(
    "enc_v3_swap_delta", cs.enc_v3_swap_delta,
    [1, True, 2], ["1u8", "true", "2u8"],
)
add(
    "enc_v3_swap_delta", cs.enc_v3_swap_delta,
    [2, False, 0], ["2u8", "false", "0u8"],
)

# ── V4 swaps (0x40–0x42) ──
add(
    "enc_v4_swap_compact", cs.enc_v4_swap_compact,
    [1, 2, 3000, 60, 0xFF, True, ONE_E18],
    ["1u8", "2u8", "3000u16", "60i16", "0xffu8", "true", rs_u128(ONE_E18)],
)
add(
    "enc_v4_swap_compact", cs.enc_v4_swap_compact,
    [3, 4, 500, 10, 0xFF, False, LARGE_U96],
    ["3u8", "4u8", "500u16", "10i16", "0xffu8", "false", rs_u128(LARGE_U96)],
)
# Negative tick-spacing parity: the signed int16 two's-complement encoding must
# match the Python oracle's `signed=True`. (V4 spacings are positive in
# practice; this exercises the signed path.)
add(
    "enc_v4_swap_compact", cs.enc_v4_swap_compact,
    [1, 2, 100, -1, 0xFF, True, 7],
    ["1u8", "2u8", "100u16", "-1i16", "0xffu8", "true", "7u128"],
)
add(
    "enc_v4_swap_dynamic", cs.enc_v4_swap_dynamic,
    [1, 2, 3000, 60, 0xFF, True],
    ["1u8", "2u8", "3000u16", "60i16", "0xffu8", "true"],
)
add(
    "enc_v4_swap_dynamic", cs.enc_v4_swap_dynamic,
    [3, 4, 500, 10, 0xFF, False],
    ["3u8", "4u8", "500u16", "10i16", "0xffu8", "false"],
)
# V4_BATCH: two swaps (first explicit, second dynamic amount=0).
batch2 = [
    (1, 2, 3000, 60, 0xFF, True, ONE_E18),
    (2, 1, 500, 10, 0xFF, False, 0),
]
rust_batch2 = (
    "&[V4BatchEntry{c0_idx:1u8,c1_idx:2u8,fee:3000u16,tick_spacing:60i16,"
    "hooks_idx:0xffu8,zfo:true,amount_u96:"
    + rs_u128(ONE_E18)
    + "},V4BatchEntry{c0_idx:2u8,c1_idx:1u8,fee:500u16,tick_spacing:10i16,"
    "hooks_idx:0xffu8,zfo:false,amount_u96:0u128}]"
)
add("enc_v4_batch", cs.enc_v4_batch, [batch2], [rust_batch2])
# V4_BATCH single swap, uint96 max amount.
batch1 = [(5, 6, 10000, 200, 0xFF, True, LARGE_U96)]
rust_batch1 = (
    "&[V4BatchEntry{c0_idx:5u8,c1_idx:6u8,fee:10000u16,tick_spacing:200i16,"
    "hooks_idx:0xffu8,zfo:true,amount_u96:"
    + rs_u128(LARGE_U96)
    + "}]"
)
add("enc_v4_batch", cs.enc_v4_batch, [batch1], [rust_batch1])

# ── V4 settlement / ERC6909 (0x50–0x59) ──
fwd_unlock = b"\x40\x01\x02\x10\x01\x02"
add("enc_v4_unlock", cs.enc_v4_unlock, [fwd_unlock], [rs_bytes(fwd_unlock)])
add("enc_v4_unlock", cs.enc_v4_unlock, [b""], [rs_bytes(b"")])
add("enc_v4_take", cs.enc_v4_take, [1, 2, ONE_E18], ["1u8", "2u8", rs_u256(ONE_E18)])
add("enc_v4_take", cs.enc_v4_take, [3, 0, 0], ["3u8", "0u8", rs_u256(0)])
add(
    "enc_v4_take_compact", cs.enc_v4_take_compact,
    [1, 2, ONE_E18], ["1u8", "2u8", rs_u128(ONE_E18)],
)
add(
    "enc_v4_take_compact", cs.enc_v4_take_compact,
    [3, 0, LARGE_U96], ["3u8", "0u8", rs_u128(LARGE_U96)],
)
add("enc_v4_take_delta", cs.enc_v4_take_delta, [1, 2], ["1u8", "2u8"])
add("enc_v4_sync", cs.enc_v4_sync, [1], ["1u8"])
add("enc_v4_settle", cs.enc_v4_settle, [], [])
add("enc_v4_settle_delta", cs.enc_v4_settle_delta, [1], ["1u8"])
add("enc_v4_settle_all", cs.enc_v4_settle_all, [], [])
add(
    "enc_v4_mint_compact", cs.enc_v4_mint_compact,
    [1, 2, ONE_E18], ["1u8", "2u8", rs_u128(ONE_E18)],
)
add(
    "enc_v4_burn_compact", cs.enc_v4_burn_compact,
    [1, LARGE_U96], ["1u8", rs_u128(LARGE_U96)],
)

# ── pack_config / pack_expected_balance ──
add("pack_config", cs.pack_config, [0, 0, 0, 0], ["0u8", rs_u256(0), "0u16", "0u8"])
add("pack_config", cs.pack_config, [1, ONE_E18, 500, 2], ["1u8", rs_u256(ONE_E18), "500u16", "2u8"])
add("pack_config", cs.pack_config, [2, LARGE_U96 * 10, 10000, 31], ["2u8", rs_u256(LARGE_U96 * 10), "10000u16", "31u8"])
add("pack_expected_balance", cs.pack_expected_balance, [1, ONE_E18], ["1u8", rs_u256(ONE_E18)])

# ── make_pool_key: assert the currency sort + the returned tuple ──
def _pool_key_hex(c0, c1, fee, ts, hooks):
    k = cs.make_pool_key(c0, c1, fee, ts, hooks)
    # The tuple has no byte encoding; pack a 20+20+4+4+20 = 68-byte canon form.
    return (
        bytes.fromhex(k[0][2:]) + bytes.fromhex(k[1][2:]) + fee.to_bytes(4, "big") +
        ts.to_bytes(4, "big", signed=True) + bytes.fromhex(k[4][2:])
    )

for args, rs in [
    ((WETH, USDC, 3000, 60, ZERO), [rs_addr(WETH), rs_addr(USDC), "3000u32", "60i16", rs_addr(ZERO)]),
    ((USDC, WETH, 500, 10, ZERO), [rs_addr(USDC), rs_addr(WETH), "500u32", "10i16", rs_addr(ZERO)]),
    ((WBTC, w := WETH, 100, -60, EXECUTOR), [rs_addr(WBTC), rs_addr(WETH), "100u32", "-60i16", rs_addr(EXECUTOR)]),
]:
    # ts must be i32 for make_pool_key; emit accordingly.
    py_ts = args[3]
    rust_ts = "60i32" if py_ts == 60 else ("10i32" if py_ts == 10 else ("-60i32" if py_ts == -60 else f"{py_ts}i32"))
    rust_args = [rs_addr(args[0]), rs_addr(args[1]), f"{args[2]}u32", rust_ts, rs_addr(args[4])]
    add("make_pool_key", _pool_key_hex, list(args), rust_args)


# ── Emit the Rust test file ──
def emit() -> str:
    lines: list[str] = []
    lines.append("// AUTO-GENERATED by tests/fixtures/generate_encoders_parity.py.")
    lines.append("// Byte-exact parity vs the Python oracle examples/cmd_stream.py (§4.2 gate).")
    lines.append("// Do not edit by hand — re-run the generator to refresh.")
    lines.append("")
    lines.append("#![allow(clippy::too_many_lines, clippy::expect_used, clippy::unreadable_literal)]")
    lines.append("")
    lines.append("use alloy::primitives::{address, U256};")
    lines.append("use degenbot_executor::encoders::{self, V4BatchEntry};")
    lines.append("")
    lines.append("fn hx(s: &[u8]) -> Vec<u8> {")
    lines.append("    s.to_vec()  // already raw bytes in the literal")
    lines.append("}")
    lines.append("")
    lines.append("#[test]")
    lines.append("fn parity_vs_python_oracle() {")
    lines.append("    // The shared address table for enc_set_addresses / enc_preamble cases.")
    lines.append("    let table = encoders::AddressTable::with_sentinels(")
    lines.append(f"        Some({rs_addr(WETH)}),")
    lines.append(f"        Some({rs_addr(EXECUTOR)}),")
    lines.append(f"        Some({rs_addr(PM)}),")
    lines.append("    );")
    lines.append("    let mut table = table;")
    lines.append(f"    let _usdc = table.add({rs_addr(USDC)}).unwrap();")
    lines.append(f"    let _wbtc = table.add({rs_addr(WBTC)}).unwrap();")
    lines.append(f"    let _native = table.add({rs_addr(ZERO)}).unwrap(); // sentinel, not added")
    lines.append("")

    def emit_assert(kind: str, rust_args: list[str], expected_bytes: bytes):
        joined = ", ".join(rust_args)
        exp = rs_bytes(expected_bytes)
        if kind == "enc_set_addresses":
            # takes &AddressTable
            body = f"encoders::{kind}(&table)"
        elif kind == "enc_preamble":
            body = f"encoders::{kind}(&table)"
        elif kind == "make_pool_key":
            # custom: encode the returned V4PoolKey into the 68-byte canon form.
            body = (
                "pool_key_to_canon(&encoders::make_pool_key("
                + joined + "))"
            )
        elif kind == "pack_config":
            body = f"encoders::{kind}({joined}).unwrap().to_be_bytes::<32>().to_vec()"
        elif kind == "pack_expected_balance":
            body = f"encoders::{kind}({joined}).unwrap().to_be_bytes::<32>().to_vec()"
        elif "compact" in kind or kind in (
            "enc_v4_batch", "enc_v4_unlock", "enc_erc20_transfer",
            "enc_send_eth", "enc_v2_swap_direct",
        ):
            body = f"encoders::{kind}({joined}).unwrap()"
        else:
            body = f"encoders::{kind}({joined})"
        lines.append(f"    assert_eq!({body}, hx({exp})); // {kind}")

    for kind, rust_args, _py_args, expected in CASES:
        emit_assert(kind, rust_args, expected)

    lines.append("}")
    lines.append("")
    lines.append("/// Encode a `V4PoolKey` into the 68-byte canon form the oracle packs:")
    lines.append("/// `[currency0:20][currency1:20][fee:4][tick_spacing:4][hooks:20]`.")
    lines.append("fn pool_key_to_canon(k: &encoders::V4PoolKey) -> Vec<u8> {")
    lines.append("    let mut out = Vec::with_capacity(68);")
    lines.append("    out.extend_from_slice(k.currency0.as_slice());")
    lines.append("    out.extend_from_slice(k.currency1.as_slice());")
    lines.append("    out.extend_from_slice(&k.fee.to_be_bytes());")
    lines.append("    out.extend_from_slice(&k.tick_spacing.to_be_bytes());")
    lines.append("    out.extend_from_slice(k.hooks.as_slice());")
    lines.append("    out")
    lines.append("}")
    return "\n".join(lines) + "\n"


OUT = REPO_ROOT / "rust/crates/degenbot-executor/tests/encoders_parity.rs"
OUT.write_text(emit())
print(f"wrote {OUT.relative_to(REPO_ROOT)} ({len(CASES)} cases)")