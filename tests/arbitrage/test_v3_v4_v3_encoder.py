"""Regression tests for the V3-V4-V3 three-hop encoder.

V3-V4-V3 reverted on mainnet with ``CurrencyNotSettled()`` (permutation row #17,
~50% simulatable) because the V4 swap used a *static* solver amount
(``V4_SWAP_COMPACT(out_a)``) while V3a delivers its actual output to the
PoolManager only at swap time, and the V4 swap's actual output (forward_b) was
taken with a static ``V4_TAKE_COMPACT(out_b)``. Whenever the on-chain V4 output
differed from the solver's ``out_b`` (or V3a's deposit differed from ``out_a``),
a residual PM delta survived at unlock-end → ``CurrencyNotSettled``.

The fix mirrors the V2-V2-V4 precedent (commit 2e505536): read the *actual*
settled input via ``V4_SWAP_DYNAMIC``, take the *actual* produced output via
``V4_TAKE_DELTA``, and ``V4_SETTLE_ALL`` sweeps residual dust. The nesting is
unchanged — V3's optimistic output transfer delivers forward_a to PM before
V3a's callback runs the V4 unlock, so the unlock sees the deposit.
"""

from web3 import Web3

from contracts.cmd_stream import (
    BEGIN_EXECUTION,
    CMD_ERC20_TRANSFER,
    CMD_V3_SWAP_COMPACT,
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
    _3hop_v3_v4_v3,
)

WETH = Web3.to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC = Web3.to_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WBTC = Web3.to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
POOL_MANAGER = Web3.to_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
EXECUTOR = Web3.to_checksum_address("0x1111111111111111111111111111111111111111")
V3A = Web3.to_checksum_address("0x2222222222222222222222222222222222222222")
V3C = Web3.to_checksum_address("0x3333333333333333333333333333333333333333")

# V3_SWAP_COMPACT layout: [0x30][pool:1][zfo:1][amount:12][recipient:1][flen:1][fwd:N]
_V3_HDR = 1 + 1 + 1 + 12 + 1 + 1  # 17 bytes before forward_data
# ERC20_TRANSFER layout: [0x10][token:1][recipient:1][amount:12]
_ERC20_LEN = 1 + 1 + 1 + 12  # 15 bytes


def _path_v3_v4_v3() -> PathInfo:
    """V3a: WETH→USDC, V4b: USDC→WBTC, V3c: WBTC→WETH."""
    ha = V3HopInfo(
        pool_address=V3A,
        token0_address=WETH,
        token1_address=USDC,
        fee=500,
        zfo=True,  # WETH(token0)→USDC(token1)
    )
    hb = V4HopInfo(
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "00" * 32,
        currency0_address=USDC,
        currency1_address=WBTC,
        fee=500,
        tick_spacing=10,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,  # USDC(currency0)→WBTC(currency1)
    )
    hc = V3HopInfo(
        pool_address=V3C,
        token0_address=WBTC,
        token1_address=WETH,
        fee=100,
        zfo=True,  # WBTC(token0)→WETH(token1)
    )
    return PathInfo(hops=[ha, hb, hc])


def _after_execution(cmd: bytes) -> bytes:
    """Return the execution section (after BEGIN_EXECUTION)."""
    pos = cmd.index(BEGIN_EXECUTION) + 1
    return cmd[pos:]


def _parse_v3_swap(buf: bytes, pos: int) -> tuple[int, int, int]:
    """Parse a V3_SWAP_COMPACT at ``pos``; return (pool_idx, recipient_idx, fwd_start).

    Layout: [0x30][pool:1][zfo:1][amt:12][rcpt:1][flen:1][fwd:N]; fwd starts at
    pos + 17 (_V3_HDR). flen is not needed to locate the forward_data start.
    """
    assert buf[pos] == CMD_V3_SWAP_COMPACT[0], f"expected V3_SWAP_COMPACT at {pos}"
    pool_idx = buf[pos + 1]  # zfo at pos+2, amount at pos+3..+14
    recipient_idx = buf[pos + 15]
    return pool_idx, recipient_idx, pos + _V3_HDR


def test_v3_v4_v3_v3c_recipient_is_executor() -> None:
    """The outermost V3c swap must deliver its WETH output to the executor (profit)."""
    cmd = _3hop_v3_v4_v3(
        _path_v3_v4_v3(),
        optimal_input=10**18,
        hop_outputs=(2_000 * 10**6, 100 * 10**8, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    # Top-level layout: [V4_SYNC(forward_a)][V3_SWAP_COMPACT(v3c, ...)]
    assert buf[0] == CMD_V4_SYNC[0]
    v3c_pos = 2  # SYNC is 2 bytes
    pool_idx, recipient_idx, _fwd_start = _parse_v3_swap(buf, v3c_pos)
    assert pool_idx != SENTINEL_SELF  # v3c pool, not a sentinel
    assert recipient_idx == SENTINEL_SELF  # V3c output → executor


def test_v3_v4_v3_v3a_recipient_is_pool_manager() -> None:
    """V3a must deliver forward_a to the PoolManager (recipient=PM).

    V3's optimistic transfer sends the output to ``recipient`` *before* the
    callback fires, so V3a→PM lands the forward_a deposit inside the V4 unlock.
    The decoder for ``recipient_idx`` resolves the PM sentinel.
    """
    cmd = _3hop_v3_v4_v3(
        _path_v3_v4_v3(),
        optimal_input=10**18,
        hop_outputs=(2_000 * 10**6, 100 * 10**8, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    v3c_pos = 2
    _, _, c_fwd_start = _parse_v3_swap(buf, v3c_pos)
    # c_fwd is V3c's forward_data, which begins with the V3a swap command.
    _v3a_pool, a_recipient, _a_end = _parse_v3_swap(buf, c_fwd_start)
    assert a_recipient == SENTINEL_PM, "V3a must deposit forward_a to the PoolManager"


def test_v3_v4_v3_uses_dynamic_swap_and_take_delta() -> None:
    """Regression: V4 swap + take must read actual PM deltas, not static amounts.

    The CurrencyNotSettled root cause: ``V4_SWAP_COMPACT(out_a)`` consumed a
    static solver input while ``V4_TAKE_COMPACT(out_b)`` took a static solver
    output, leaving a residual PM delta whenever on-chain amounts differed from
    the solver's. The fixed encoder uses:

      V4_SETTLE          (credit V3a's actual forward_a deposit)
      V4_SWAP_DYNAMIC    (consume the actual settled forward_a)
      V4_TAKE_DELTA      (take the actual produced forward_b → V3c)
      V4_SETTLE_ALL      (sweep residual dust)
    """
    cmd = _3hop_v3_v4_v3(
        _path_v3_v4_v3(),
        optimal_input=10**18,
        hop_outputs=(2_000 * 10**6, 100 * 10**8, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    v3c_pos = 2
    _, _, c_fwd_start = _parse_v3_swap(buf, v3c_pos)
    # c_fwd (V3c's forward_data) begins with the V3a swap command; a_fwd is
    # V3a's forward_data, located just past V3a's 17-byte header.
    _v3a_pool, _a_recipient, a_fwd_start = _parse_v3_swap(buf, c_fwd_start)
    # a_fwd is ERC20_TRANSFER(WETH→V3a) then V4_UNLOCK(v4_inner).
    assert buf[a_fwd_start] == CMD_ERC20_TRANSFER[0]
    unlock_pos = a_fwd_start + _ERC20_LEN
    assert buf[unlock_pos] == CMD_V4_UNLOCK[0]
    inner_len = buf[unlock_pos + 1]
    inner_start = unlock_pos + 2
    inner = buf[inner_start : inner_start + inner_len]
    assert len(inner) == inner_len

    # Assert the fixed command ordering inside the V4 unlock inner:
    # SETTLE, then SWAP_DYNAMIC, then TAKE_DELTA, ending with SETTLE_ALL.
    assert inner[0] == CMD_V4_SETTLE[0]
    assert CMD_V4_SWAP_DYNAMIC[0] in inner
    assert CMD_V4_SWAP_COMPACT[0] not in inner, (
        "V4_SWAP_COMPACT (static solver amount) caused CurrencyNotSettled; "
        "use V4_SWAP_DYNAMIC to consume the actual settled forward_a delta"
    )
    assert CMD_V4_TAKE_DELTA[0] in inner
    assert CMD_V4_TAKE_COMPACT[0] not in inner, (
        "V4_TAKE_COMPACT (static solver amount) left a residual forward_b delta; "
        "use V4_TAKE_DELTA to take the full actual forward_b output"
    )
    assert inner[-1] == CMD_V4_SETTLE_ALL[0], (
        "V4_SETTLE_ALL must sweep residual dust before the unlock closes"
    )

    # Ordering: SETTLE runs before SWAP_DYNAMIC (so the forward_a deposit is
    # credited before the dynamic swap reads it).
    settle_idx = inner.index(CMD_V4_SETTLE[0])
    swap_dyn_idx = inner.index(CMD_V4_SWAP_DYNAMIC[0])
    take_delta_idx = inner.index(CMD_V4_TAKE_DELTA[0])
    assert settle_idx < swap_dyn_idx < take_delta_idx
