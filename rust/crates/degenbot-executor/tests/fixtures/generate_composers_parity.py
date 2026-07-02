"""Generate the Rust byte-parity test for the command-stream composers.

Calls :func:`examples.eth_backrun_helpers.encode_cmd_stream` over representative
inputs across every 2-hop path type (+ V2N) and every native/WETH/ERC20
variant the oracle distinguishes, plus the
``examples.cmd_stream`` payload builders (:class:`V4V4ArbitragePayload`,
:class:`V4V3ArbitragePayload`, :class:`CmdExecutorComposer`).

Emits ``tests/composers_parity.rs`` whose assertions pin byte-exact output.
Re-run with::

    uv run python rust/crates/degenbot-executor/tests/fixtures/generate_composers_parity.py

The generated file references ``degenbot_executor::composers::*`` and asserts
the Rust composer output equals the Python oracle's output. Regenerate
whenever the oracle or the composer semantics change; the diff is the parity
evidence (§4.2 red-green).
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO_ROOT / "examples"))
sys.path.insert(0, str(REPO_ROOT / "src"))

import cmd_stream as cs  # noqa: E402
import eth_backrun_helpers as ebh  # noqa: E402
from degenbot.arbitrage.hop_info import (  # noqa: E402
    PathInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
)

# ── Addresses (centrally-defined) ──
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"
WBTC = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
EXECUTOR = "0xDeAd0000000000000000000000000000000000Be"
PM = "0x000000000004444c5dc75cB358380D2e3dE08A90"
ZERO = "0x0000000000000000000000000000000000000000"
# A non-WETH, non-USDC, non-zero ERC-20 for cross-currency output tests.
DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"

# Amounts (all < 2^127 so the int128 guard passes).
ONE_E18 = 1 * 10**18
TWO_K_USDC = 2_000 * 10**6
TWO_K_ONE_USDC = 2_001 * 10**6  # profit
TWO_E18 = 2 * 10**18
TWO_OH_ONE_E18 = 2_001_000_000_000_000_000  # profit over 1e18
MID = 1_234_567_890_123_456_789

HOOKS = ZERO


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
        v2_rust(h) if isinstance(h, V2HopInfo) else v3_rust(h) if isinstance(h, V3HopInfo) else v4_rust(h)
        for h in hops
    )
    return f"PathInfo::new(vec![{body}])"


CASES: list[tuple[str, str, str]] = []  # (label, rust_call_expr, expected_hex)


def rs_bytes(b: bytes) -> str:
    if not b:
        return 'b""'
    return 'b"' + "".join(f"\\x{x:02x}" for x in b) + '"'


def enc_stream_case(label: str, hops: list, optimal_input: int, hop_outputs: tuple, opts: dict | None = None):
    """Call the Python encode_cmd_stream oracle and record the expected bytes."""
    path_info = PathInfo(hops=list(hops))
    kwargs = {
        "executor_address": EXECUTOR,
        "pool_manager_address": PM,
        "weth_address": WETH,
    }
    if opts:
        kwargs.update(opts)
    expected = ebh.encode_cmd_stream(
        path_info=path_info,
        optimal_input=optimal_input,
        hop_outputs=hop_outputs,
        **kwargs,
    )
    if expected is None:
        # Python returned None — record it as a Rust None assertion so the test
        # range is honest (we exercise the unsupported side too).
        rust = "None"
    else:
        rust_path = path_rust(hops)
        outs = ", ".join(f"{o}u128" for o in hop_outputs)
        rust_opts = ""
        if opts:
            # Both fields populated — EncodeOptions has no Default-skip.
            parts = []
            parts.append(f"erc6909_profit:{str(opts.get('erc6909_profit', False)).lower()}")
            parts.append(f"use_v4_batch:{str(opts.get('use_v4_batch', False)).lower()}")
            rust_opts = f",EncodeOptions{{{','.join(parts)}}}"
        else:
            rust_opts = ",EncodeOptions::default()"
        rust = (
            f"encode_cmd_stream(&{rust_path},{optimal_input}u128,&[{outs}],"
            f"{addr_rust(EXECUTOR)},{addr_rust(PM)},{addr_rust(WETH)}{rust_opts})"
        )
    CASES.append((label, rust, expected))


# V4 pool id (0x + 64 hex) is opaque to the composer; use a fixed dummy.
V4PID = "0x" + "11" * 32

# ── V4-V4 ─────────────────────────────────────────────────────────────────────
# Same intermediate currency (WETH↔WETH, no wrap/unwrap) — the canonical case.
enc_stream_case(
    "v4v4_same_currency",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True),
     v4(PM, V4PID, USDC, WETH, 500, 10, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# Native-ETH intermediate (pool A outputs ETH, pool B needs WETH → wrap).
enc_stream_case(
    "v4v4_native_to_weth_wrap",
    [v4(PM, V4PID, ZERO, USDC, 3000, 60, False),   # currency1=USDC is output (zfo=False→output=c0=native)
     v4(PM, V4PID, WETH, USDC, 500, 10, False)],   # currency0=WETH is input (zfo=False→output=c1=USDC) -- but pool B mid_currency_b=c0=WETH so needs WETH; a outputs native
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# WETH-to-native intermediate (pool A outputs WETH, pool B needs native → unwrap).
# pool A: c0=USDC,c1=WETH,zfo=True → output=c1=WETH (so a_outputs_native=False);
# pool B: c0=USDC,c1=ZERO → mid_currency_b=c0=USDC (zfo=True→needs c0=USDC, not native).
# To force needs_unwrap: pool B mid_currency_b must be ZERO (native).
enc_stream_case(
    "v4v4_weth_to_native_unwrap",
    [v4(PM, V4PID, USDC, WETH, 3000, 60, True),    # output c1=WETH (a_outputs_native=False)
     v4(PM, V4PID, ZERO, USDC, 500, 10, True)],    # mid_b=c0=ZERO (b_needs_native=True) → unwrap
    TWO_K_USDC, (ONE_E18, TWO_OH_ONE_E18),
)
# Same-currency with use_v4_batch.
enc_stream_case(
    "v4v4_same_currency_batch",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True),
     v4(PM, V4PID, USDC, WETH, 500, 10, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
    opts={"use_v4_batch": True},
)
# Same-currency with erc6909_profit (WETH profit → V4_MINT_COMPACT).
enc_stream_case(
    "v4v4_same_currency_erc6909",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True),
     v4(PM, V4PID, USDC, WETH, 500, 10, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
    opts={"erc6909_profit": True},
)
# Cross-currency WETH→native with batch (batch auto-settles WETH+native; ERC20 take)
enc_stream_case(
    "v4v4_weth_to_native_unwrap_batch",
    [v4(PM, V4PID, USDC, WETH, 3000, 60, True),
     v4(PM, V4PID, ZERO, USDC, 500, 10, True)],
    TWO_K_USDC, (ONE_E18, TWO_OH_ONE_E18),
    opts={"use_v4_batch": True},
)

# ── V4-V3 ─────────────────────────────────────────────────────────────────────
# Native ETH output (V4 outputs ETH, V3 needs WETH → WETH_DEPOSIT bridge).
enc_stream_case(
    "v4v3_native_out_deposit",
    [v4(PM, V4PID, USDC, ZERO, 500, 10, True),     # zfo=True→output=c1=ZERO=native
     v3("0x1111111111111111111111111111111111111111", WETH, USDC, 3000, True)],
    TWO_K_USDC, (ONE_E18, TWO_OH_ONE_E18),
)
# ERC-20 output (WETH→USDC at V4; V3 takes USDC auto-pay; WETH input debt settle).
# V4 input is WETH (not native), V4 output ERC20 → no unwrap.
enc_stream_case(
    "v4v3_erc20_out_autopay",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True),    # zfo=True→output=c1=USDC
     v3("0x2222222222222222222222222222222222222222", USDC, WETH, 500, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# ERC-20 output with V4 input native ETH (must WETH_WITHDRAW before settling).
enc_stream_case(
    "v4v3_erc20_out_v4_in_native_unwrap",
    [v4(PM, V4PID, ZERO, USDC, 3000, 60, True),     # zfo=True→output=c1=USDC; input=c0=ZERO=native
     v3("0x3333333333333333333333333333333333333333", USDC, WETH, 500, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V3-V4 ─────────────────────────────────────────────────────────────────────
# V4 input is WETH (standard sync+transfer+settle+swap+take).
enc_stream_case(
    "v3v4_v4_in_weth",
    [v3("0x4444444444444444444444444444444444444444", WETH, USDC, 3000, True),
     v4(PM, V4PID, USDC, WETH, 500, 10, True)],    # V4 input c0=USDC (not native)
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# V4 input is native ETH (WETH_WITHDRAW bridge before V4 swap).
enc_stream_case(
    "v3v4_v4_in_native",
    [v3("0x5555555555555555555555555555555555555555", WETH, USDC, 3000, True),
     v4(PM, V4PID, ZERO, WETH, 500, 10, True)],    # V4 input c0=ZERO=native
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V4-V2 ─────────────────────────────────────────────────────────────────────
# Native ETH output (V4 outputs ETH, V2 needs WETH → WETH_DEPOSIT + callback).
enc_stream_case(
    "v4v2_native_out_deposit",
    [v4(PM, V4PID, USDC, ZERO, 500, 10, True),     # zfo=True→output=c1=ZERO=native
     v2("0x6666666666666666666666666666666666666666", WETH, USDC, 30, True)],
    TWO_K_USDC, (ONE_E18, TWO_OH_ONE_E18),
)
# ERC-20 output direct custody (V4 outputs USDC, V2_SWAP_CALC reads excess).
# V4 input WETH (not native) → sync+transfer+settle path.
enc_stream_case(
    "v4v2_erc20_out_direct",
    [v4(PM, V4PID, WETH, USDC, 3000, 60, True),    # zfo=True→output=c1=USDC
     v2("0x7777777777777777777777777777777777777777", USDC, WETH, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# ERC-20 output with V4 input native ETH (must WETH_WITHDRAW before settling).
enc_stream_case(
    "v4v2_erc20_out_v4_in_native",
    [v4(PM, V4PID, ZERO, USDC, 3000, 60, True),    # zfo=True→output=c1=USDC; input=native
     v2("0x8888888888888888888888888888888888888888", USDC, WETH, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V2-V4 ─────────────────────────────────────────────────────────────────────
# V4 input WETH (V4 out native → wrap to WETH before paying V2).
# pool V2: USDC->WETH (zfo=True→forward=c1=WETH).  V4: input c0=USDC? No, need V4 native input.
# To exercise _v4_in_native=False path with _v4_out_native=True (deposit wrap in callback):
enc_stream_case(
    "v2v4_v4_out_native",
    [v2("0x9999999999999999999999999999999999999999", WETH, USDC, 30, True),  # forward=c1=USDC
     v4(PM, V4PID, USDC, ZERO, 500, 10, True)],   # V4 out c1=ZERO=native, input c0=USDC (not native)
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# V4 input is native ETH (WETH_WITHDRAW before V4 swap).
enc_stream_case(
    "v2v4_v4_in_native",
    [v2("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", WETH, USDC, 30, True),
     v4(PM, V4PID, ZERO, USDC, 500, 10, True)],   # V4 input c0=ZERO=native
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V3-V3 ─────────────────────────────────────────────────────────────────────
enc_stream_case(
    "v3v3_forward_order",
    [v3("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", WETH, USDC, 3000, True),
     v3("0xcccccccccccccccccccccccccccccccccccccccc", USDC, WETH, 500, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V2-V3 ─────────────────────────────────────────────────────────────────────
enc_stream_case(
    "v2v3_callback_forward_data",
    [v2("0xdddddddddddddddddddddddddddddddddddddddd", WETH, USDC, 30, True),
     v3("0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", USDC, WETH, 500, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V3-V2 ─────────────────────────────────────────────────────────────────────
enc_stream_case(
    "v3v2_callback_nested",
    [v3("0xf111111111111111111111111111111111111111", WETH, USDC, 3000, True),
     v2("0xf222222222222222222222222222222222222222", USDC, WETH, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)

# ── V2 N-hop ──────────────────────────────────────────────────────────────────
# 2-hop V2 (matches the all-V2 dispatch → V2N).
enc_stream_case(
    "v2_n_hop_2",
    [v2("0xf333333333333333333333333333333333333333", WETH, USDC, 30, True),
     v2("0xf444444444444444444444444444444444444444", USDC, WETH, 30, True)],
    ONE_E18, (TWO_K_USDC, TWO_OH_ONE_E18),
)
# 3-hop V2.
enc_stream_case(
    "v2_n_hop_3",
    [v2("0xf555555555555555555555555555555555555555", WETH, USDC, 30, True),
     v2("0xf666666666666666666666666666666666666666", USDC, WBTC, 30, True),
     v2("0xf777777777777777777777777777777777777777", WBTC, WETH, 30, True)],
    ONE_E18, (TWO_K_USDC, ONE_E18 // 1000, TWO_OH_ONE_E18),
)


# ── Payload builders (cmd_stream.py) ──────────────────────────────────────────
def payload_v4v4(label: str, *, profit_currency=None, batch=False):
    p = cs.V4V4ArbitragePayload(pool_manager=PM, weth=WETH, executor=EXECUTOR)
    p.set_pool_a(currency0=WETH, currency1=USDC, fee=3000, tick_spacing=60,
                 amount_in=ONE_E18, amount_out=TWO_K_USDC)
    p.set_pool_b(currency0=USDC, currency1=WETH, fee=500, tick_spacing=10,
                 amount_in=TWO_K_USDC, amount_out=TWO_OH_ONE_E18)
    if profit_currency is not None:
        p.profit_currency = profit_currency
    expected = p.encode_batch() if batch else p.encode()
    # Build the matching Rust builder expression.
    rust = (
        "V4V4ArbitragePayload::new("
        f"{addr_rust(PM)},{addr_rust(WETH)},{addr_rust(EXECUTOR)})"
        f".set_pool_a({addr_rust(WETH)},{addr_rust(USDC)},3000u32,60i32,"
        f"{addr_rust(ZERO)},{ONE_E18}u128,{TWO_K_USDC}u128,None)"
        f".set_pool_b({addr_rust(USDC)},{addr_rust(WETH)},500u32,10i32,"
        f"{addr_rust(ZERO)},{TWO_K_USDC}u128,{TWO_OH_ONE_E18}u128,None)"
    )
    # set_pool_a/set_pool_b return (); chain via a let-binding pattern instead.
    # We emit a closure-style expression building then encoding.
    rust_expr = (
        "{ let mut p = V4V4ArbitragePayload::new("
        f"{addr_rust(PM)},{addr_rust(WETH)},{addr_rust(EXECUTOR)});"
        f" p.set_pool_a({addr_rust(WETH)},{addr_rust(USDC)},3000u32,60i32,"
        f"{addr_rust(ZERO)},{ONE_E18}u128,{TWO_K_USDC}u128,None);"
        f" p.set_pool_b({addr_rust(USDC)},{addr_rust(WETH)},500u32,10i32,"
        f"{addr_rust(ZERO)},{TWO_K_USDC}u128,{TWO_OH_ONE_E18}u128,None);"
        + (f" p.profit_currency = Some({addr_rust(profit_currency)});" if profit_currency else "")
        + (" p.encode_batch()" if batch else " p.encode()")
        + " }"
    )
    _ = rust  # legacy
    CASES.append((label, rust_expr, expected))


payload_v4v4("v4v4_payload_encode_same_currency")
payload_v4v4("v4v4_payload_encode_native_profit", profit_currency=ZERO)
payload_v4v4("v4v4_payload_encode_batch", batch=True)


def payload_v4v3(label: str, *, with_forward_data: bool):
    p = cs.V4V3ArbitragePayload(
        pool_manager=PM, weth=WETH, executor=EXECUTOR,
        v3_pool="0xf888888888888888888888888888888888888888",
        intermediate_token=USDC,
    )
    p.set_v4_pool(currency0=WETH, currency1=USDC, fee=3000, tick_spacing=60,
                  amount_in=ONE_E18, amount_out=TWO_K_USDC)
    p.set_v3_pool(amount_in=TWO_K_USDC, amount_out=TWO_OH_ONE_E18, zero_for_one=True)
    expected = p.encode_with_forward_data() if with_forward_data else p.encode()
    v3_pool = "0xf888888888888888888888888888888888888888"
    rust_expr = (
        "{ let mut p = V4V3ArbitragePayload::new("
        f"{addr_rust(PM)},{addr_rust(WETH)},{addr_rust(EXECUTOR)},"
        f"{addr_rust(v3_pool)},{addr_rust(USDC)});"
        f" p.set_v4_pool({addr_rust(WETH)},{addr_rust(USDC)},3000u32,60i32,"
        f"{addr_rust(ZERO)},{ONE_E18}u128,{TWO_K_USDC}u128,None);"
        f" p.set_v3_pool({TWO_K_USDC}u128,{TWO_OH_ONE_E18}u128,true);"
        + (" p.encode_with_forward_data()" if with_forward_data else " p.encode()")
        + " }"
    )
    CASES.append((label, rust_expr, expected))


payload_v4v3("v4v3_payload_encode_autopay", with_forward_data=False)
payload_v4v3("v4v3_payload_encode_forward_data", with_forward_data=True)


def payload_cmd_executor(label: str):
    """CmdExecutorComposer.compose over a 2-pool V4→V4 path (the 9th two-hop)."""
    composer = cs.CmdExecutorComposer(pool_manager=PM, weth=WETH, executor=EXECUTOR)
    # Build a same-currency WETH→USDC→WETH path with minimal SwapAmounts fields.
    from degenbot.arbitrage.types import UniswapV4PoolSwapAmounts, V4PoolKey
    from degenbot.checksum_cache import get_checksum_address

    key_a = V4PoolKey(
        currency0=get_checksum_address(WETH),
        currency1=get_checksum_address(USDC),
        fee=3000, tick_spacing=60, hooks=ZERO,
    )
    key_b = V4PoolKey(
        currency0=get_checksum_address(USDC),
        currency1=get_checksum_address(WETH),
        fee=500, tick_spacing=10, hooks=ZERO,
    )
    swap_a = UniswapV4PoolSwapAmounts(
        address=get_checksum_address(EXECUTOR),
        id=b"\x00"*32,
        pool_key=key_a, amount_in=ONE_E18, amount_out=TWO_K_USDC,
        amount_specified=1, zero_for_one=True, sqrt_price_limit_x96=0,
    )
    swap_b = UniswapV4PoolSwapAmounts(
        address=get_checksum_address(EXECUTOR),
        id=b"\x00"*32,
        pool_key=key_b, amount_in=TWO_K_USDC, amount_out=TWO_OH_ONE_E18,
        amount_specified=1, zero_for_one=True, sqrt_price_limit_x96=0,
    )
    calls = composer.compose((swap_a, swap_b), config=0)
    # The composer returns a 1-element list with EncodedCall(to, data, value).
    assert len(calls) == 1, calls
    expected = calls[0].data  # the execute(bytes,uint256) calldata
    # Rust: build V4SwapAmounts and call CmdExecutorComposer::compose(...).unwrap().unwrap()
    rust_expr = (
        "{ let c = CmdExecutorComposer::new("
        f"{addr_rust(PM)},{addr_rust(WETH)},{addr_rust(EXECUTOR)});"
        " let sa = V4SwapAmounts{pool_key:V4PoolKeyConfig{"
        f"currency0:{addr_rust(WETH)},currency1:{addr_rust(USDC)},fee:3000u32,"
        f"tick_spacing:60i32,hooks:{addr_rust(ZERO)}}},zero_for_one:true,"
        f"amount_in:{ONE_E18}u128,amount_out:{TWO_K_USDC}u128}};"
        " let sb = V4SwapAmounts{pool_key:V4PoolKeyConfig{"
        f"currency0:{addr_rust(USDC)},currency1:{addr_rust(WETH)},fee:500u32,"
        f"tick_spacing:10i32,hooks:{addr_rust(ZERO)}}},zero_for_one:true,"
        f"amount_in:{TWO_K_USDC}u128,amount_out:{TWO_OH_ONE_E18}u128}};"
        " Some(c.compose(&[sa,sb],U256::ZERO).unwrap().unwrap().data) }"
    )
    CASES.append((label, rust_expr, expected))


payload_cmd_executor("cmd_executor_compose_v4v4")

# ── encode_execute_call (ABI wrap) standalone parity vs eth_abi.encode ─
# Constructs commands via the v4v4 payload (same simplified path), then wraps
# both sides in execute(bytes, uint256). Pins the degenbot-abi ─ eth_abi
# byte-equivalence for this exact type tuple.
import eth_abi  # noqa: E402

_v4v4 = cs.V4V4ArbitragePayload(pool_manager=PM, weth=WETH, executor=EXECUTOR)
_v4v4.set_pool_a(currency0=WETH, currency1=USDC, fee=3000, tick_spacing=60,
                 amount_in=ONE_E18, amount_out=TWO_K_USDC)
_v4v4.set_pool_b(currency0=USDC, currency1=WETH, fee=500, tick_spacing=10,
                 amount_in=TWO_K_USDC, amount_out=TWO_OH_ONE_E18)
_cmds = _v4v4.encode()
_py_exec_data = bytes(cs.Web3.keccak(text="execute(bytes,uint256)")[:4]) + eth_abi.encode(
    ["bytes", "uint256"], [_cmds, 0]
)
_cmds_hex = rs_bytes(_cmds)
_rust_exec_expr = (
    "{ Some({ let cmds = hx(" + _cmds_hex + "); "
    " encode_execute_call(" + addr_rust(EXECUTOR) + ", &cmds, U256::ZERO).unwrap().data }) }"
)
CASES.append(("encode_execute_call_wrap", _rust_exec_expr, _py_exec_data))

# ── execute() ABI wrap standalone (selector + bytes,uint256) ──
# Independent parity check for encode_execute_call vs Web3.keccak selector.
from web3 import Web3  # noqa: E402

sel = Web3.keccak(text="execute(bytes,uint256)")[:4]
assert bytes(EXECUTE_SELECTOR_check := [0xab, 0x58, 0x98, 0xe8]) == sel, (
    f"selector mismatch: rust={[hex(b) for b in EXECUTE_SELECTOR_check]} py={sel.hex()}"
)


# ── Emit the Rust test file ──
def emit() -> str:
    lines: list[str] = []
    lines.append("// AUTO-GENERATED by tests/fixtures/generate_composers_parity.py.")
    lines.append("// Byte-exact parity vs the Python oracle:")
    lines.append("//   - examples/eth_backrun_helpers.py::encode_cmd_stream (the 2-hop + V2N dispatch)")
    lines.append("//   - examples/cmd_stream.py payload builders (V4V4ArbitragePayload,")
    lines.append("//     V4V3ArbitragePayload, CmdExecutorComposer).")
    lines.append("// Do not edit by hand — re-run the generator to refresh (§4.2 gate).")
    lines.append("")
    lines.append("#![allow(")
    lines.append("    clippy::too_many_lines,")
    lines.append("    clippy::unreadable_literal,")
    lines.append("    clippy::needless_pass_by_value,")
    lines.append(")]")
    lines.append("")
    lines.append("use alloy::primitives::{address, U256};")
    lines.append("use degenbot_executor::composers::{")
    lines.append("    self, CmdExecutorComposer, EncodeOptions, HopInfo, PathInfo,")
    lines.append("    V2HopInfo, V3HopInfo, V4HopInfo, V4PoolKeyConfig, V4SwapAmounts,")
    lines.append("    V4V3ArbitragePayload, V4V4ArbitragePayload, encode_cmd_stream,")
    lines.append("    encode_execute_call,")
    lines.append("};")
    lines.append("")
    lines.append("fn hx(s: &[u8]) -> Vec<u8> {")
    lines.append("    s.to_vec()")
    lines.append("}")
    lines.append("")

    # One test per case keeps failures localized + avoids one giant fn.
    for label, rust_expr, expected in CASES:
        lines.append("#[test]")
        lines.append(f"fn parity_{label}() {{")
        if expected is None:
            lines.append(f"    // Python oracle returned None — unsupported path.")
            lines.append(f"    let rust = {rust_expr};")
            lines.append("    let py: Option<Vec<u8>> = None;")
            lines.append("    assert_eq!(rust, py);")
        else:
            lines.append(f"    let rust = {rust_expr};")
            lines.append(f"    assert_eq!(rust, Some(hx({rs_bytes(expected)})));")
        lines.append("}")
        lines.append("")

    # Selector assertion.
    lines.append("#[test]")
    lines.append("fn parity_execute_selector() {")
    lines.append("    assert_eq!(&composers::EXECUTE_SELECTOR, &[0xab, 0x58, 0x98, 0xe8]);")
    lines.append("    // Matches Web3.keccak(text=\"execute(bytes,uint256)\")[:4].")
    lines.append("}")
    lines.append("")
    return "\n".join(lines) + "\n"


OUT = REPO_ROOT / "rust/crates/degenbot-executor/tests/composers_parity.rs"
OUT.write_text(emit())
print(f"wrote {OUT.relative_to(REPO_ROOT)} ({len(CASES)} cases)")