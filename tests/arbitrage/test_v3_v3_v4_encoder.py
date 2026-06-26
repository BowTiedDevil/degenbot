"""Regression tests for the V3-V3-V4 three-hop encoder.

V3-V3-V4 reverted on mainnet with ``ERC20: transfer amount exceeds balance``
(116 reverts in ``perm-V3-V3-V4.log``) because the V4 swap used a *static*
solver amount (``V4_SWAP_COMPACT(out_b)``) while V3b delivers its actual USDC
output to the PoolManager only at swap time. When the on-chain V3b output (X)
differed from the solver's ``out_b`` (e.g. the solver's U512 V3 math rounded
higher than EVM cl-math, so ``X < out_b``), the net USDC delta went negative —
``V4_SETTLE_ALL`` then reconciled via ``sync(USDC); transfer(USDC, PM,
out_b - X)`` from the executor, which holds 0 USDC (no-custody design) → revert.

The fix mirrors the V3-V4-V3 precedent (commit + ``test_v3_v4_v3_encoder.py``)
and the V2-V2-V4 fix (commit 2e505536): read the *actual* settled input via
``V4_SWAP_DYNAMIC``, take the *actual* produced WETH output via
``V4_TAKE_DELTA`` (→ V3a), and ``V4_SETTLE_ALL`` sweeps residual dust. The
nesting is unchanged — V3b's optimistic output transfer delivers forward_b
(USDC) to PM before V3b's callback runs the V3a swap, whose callback runs the
V4 unlock, so the unlock sees the USDC deposit.
"""

from web3 import Web3

from examples.cmd_stream import (
    BEGIN_EXECUTION,
    CMD_V4_SETTLE,
    CMD_V4_SETTLE_ALL,
    CMD_V4_SWAP_COMPACT,
    CMD_V4_SWAP_DYNAMIC,
    CMD_V4_SYNC,
    CMD_V4_TAKE_COMPACT,
    CMD_V4_TAKE_DELTA,
    CMD_V4_UNLOCK,
    SENTINEL_PM,
    SENTINEL_SELF,
)
from examples.eth_backrun_helpers import (
    PathInfo,
    V3HopInfo,
    V4HopInfo,
    _3hop_v3_v3_v4,
)

WETH = Web3.to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC = Web3.to_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WBTC = Web3.to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
POOL_MANAGER = Web3.to_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
EXECUTOR = Web3.to_checksum_address("0x1111111111111111111111111111111111111111")
V3A = Web3.to_checksum_address("0x2222222222222222222222222222222222222222")
V3B = Web3.to_checksum_address("0x3333333333333333333333333333333333333333")

# V3_SWAP_COMPACT layout: [0x30][pool:1][zfo:1][amount:12][recipient:1][flen:1][fwd:N]
_V3_HDR = 1 + 1 + 1 + 12 + 1 + 1  # 17 bytes before forward_data


def _path_v3_v3_v4() -> PathInfo:
    """V3a: WETH→WBTC, V3b: WBTC→USDC, V4c: USDC→WETH (the path-3179 layout)."""
    ha = V3HopInfo(
        pool_address=V3A,
        token0_address=WBTC,
        token1_address=WETH,
        fee=2500,
        zfo=False,  # token1(WETH) in, token0(WBTC) out
    )
    hb = V3HopInfo(
        pool_address=V3B,
        token0_address=WBTC,
        token1_address=USDC,
        fee=500,
        zfo=True,  # token0(WBTC) in, token1(USDC) out
    )
    hc = V4HopInfo(
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "00" * 32,
        currency0_address=USDC,
        currency1_address=WETH,
        fee=3000,
        tick_spacing=60,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,  # currency0(USDC) in, currency1(WETH) out
    )
    return PathInfo(hops=[ha, hb, hc])


def _after_execution(cmd: bytes) -> bytes:
    """Return the execution section (after BEGIN_EXECUTION)."""
    pos = cmd.index(BEGIN_EXECUTION) + 1
    return cmd[pos:]


def _parse_v3_swap(buf: bytes, pos: int) -> tuple[int, int, int]:
    """Parse a V3_SWAP_COMPACT at ``pos``; return (pool_idx, recipient_idx, fwd_start).

    Layout: [0x30][pool:1][zfo:1][amt:12][rcpt:1][flen:1][fwd:N]; fwd starts at
    pos + _V3_HDR.
    """
    assert buf[pos] == 0x30, f"expected V3_SWAP_COMPACT at {pos}, got {buf[pos]:#x}"
    pool_idx = buf[pos + 1]
    recipient_idx = buf[pos + 15]
    return pool_idx, recipient_idx, pos + _V3_HDR


def _find_v4_unlock_inner(buf: bytes) -> bytes:
    """Locate the V4_UNLOCK inner payload in a V3-V3-V4 cmd stream.

    Top-level: [V4_SYNC][V3_SWAP_COMPACT(v3b, ...fwd=b_fwd)]
      b_fwd = [V3_SWAP_COMPACT(v3a, ...fwd=a_fwd)]
        a_fwd = [V4_UNLOCK(inner)]
    """
    # Skip SYNC (2 bytes) → V3b swap (the outermost).
    v3b_pos = 2
    _, _, b_fwd_start = _parse_v3_swap(buf, v3b_pos)
    # b_fwd begins with the V3a swap command.
    _, _, a_fwd_start = _parse_v3_swap(buf, b_fwd_start)
    # a_fwd is the V4_UNLOCK(inner).
    assert buf[a_fwd_start] == CMD_V4_UNLOCK[0], f"expected V4_UNLOCK at {a_fwd_start}"
    inner_len = buf[a_fwd_start + 1]
    inner_start = a_fwd_start + 2
    return buf[inner_start : inner_start + inner_len]


def test_v3_v3_v4_v3b_recipient_is_pool_manager() -> None:
    """V3b must deliver forward_b (USDC) to the PoolManager (recipient=PM).

    V3's optimistic transfer sends the output to ``recipient`` *before* the
    callback fires, so V3b→PM lands the forward_b (USDC) deposit that the V4
    unlock inside V3a's callback will consume.
    """
    cmd = _3hop_v3_v3_v4(
        _path_v3_v3_v4(),
        optimal_input=10**18,
        hop_outputs=(165, 102_180, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    # Top-level: [V4_SYNC(forward_b)][V3_SWAP_COMPACT(v3b, recipient=PM)]
    assert buf[0] == CMD_V4_SYNC[0]
    v3b_pos = 2
    _v3b_pool, b_recipient, _ = _parse_v3_swap(buf, v3b_pos)
    assert b_recipient == SENTINEL_PM, "V3b must deposit forward_b to the PoolManager"


def test_v3_v3_v4_uses_dynamic_swap_and_take_delta() -> None:
    """Regression: V4 swap + take must read actual PM deltas, not static amounts.

    The ``ERC20: transfer amount exceeds balance`` root cause:
    ``V4_SWAP_COMPACT(out_b)`` consumed a static solver input (the engine's
    ``out_b``), while V3b delivered its ACTUAL on-chain USDC output to PM. When
    the actual output (X) < ``out_b``, the net USDC delta went negative →
    ``V4_SETTLE_ALL`` reconciled via an executor→PM USDC transfer, but the
    executor holds 0 USDC (no-custody design) → revert.

    The fixed encoder uses (mirroring V3-V4-V3):

      V4_SETTLE          (credit V3b's actual forward_b deposit)
      V4_SWAP_DYNAMIC    (consume the actual settled forward_b)
      V4_TAKE_DELTA      (take the actual produced WETH → V3a)
      V4_SETTLE_ALL      (sweep residual dust)
    """
    cmd = _3hop_v3_v3_v4(
        _path_v3_v3_v4(),
        optimal_input=10**18,
        hop_outputs=(165, 102_180, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    inner = _find_v4_unlock_inner(buf)
    assert len(inner) > 0

    # Assert the fixed command ordering inside the V4 unlock inner:
    # SETTLE, then SWAP_DYNAMIC, then TAKE_DELTA, ending with SETTLE_ALL.
    assert inner[0] == CMD_V4_SETTLE[0]
    assert CMD_V4_SWAP_DYNAMIC[0] in inner
    assert CMD_V4_SWAP_COMPACT[0] not in inner, (
        "V4_SWAP_COMPACT (static solver out_b) caused the residual USDC delta "
        "→ 'transfer amount exceeds balance'; use V4_SWAP_DYNAMIC to consume "
        "the actual settled forward_b delta"
    )
    assert CMD_V4_TAKE_DELTA[0] in inner
    assert CMD_V4_TAKE_COMPACT[0] not in inner, (
        "V4_TAKE_COMPACT (static optimal_input) left a residual WETH delta; "
        "use V4_TAKE_DELTA to take the actual produced WETH → V3a"
    )
    assert inner[-1] == CMD_V4_SETTLE_ALL[0], (
        "V4_SETTLE_ALL must sweep residual dust before the unlock closes"
    )

    # Ordering: SETTLE before SWAP_DYNAMIC (deposit credited before consumed),
    # SWAP_DYNAMIC before TAKE_DELTA (output produced before taken).
    settle_idx = inner.index(CMD_V4_SETTLE[0])
    swap_dyn_idx = inner.index(CMD_V4_SWAP_DYNAMIC[0])
    take_delta_idx = inner.index(CMD_V4_TAKE_DELTA[0])
    assert settle_idx < swap_dyn_idx < take_delta_idx