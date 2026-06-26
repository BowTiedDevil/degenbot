"""Regression tests for the V3-V4-V2 three-hop encoder.

V3-V4-V2 reverted on mainnet with ``ERC20: transfer amount exceeds balance``
(54 reverts in ``perm-V3-V4-V2.log``) for the same root cause as V3-V3-V4:
the V4 swap used a *static* solver amount (``V4_SWAP_COMPACT(out_a)``) while
V3a delivers its actual forward_a output to the PoolManager only at swap time.
When the on-chain V3a output differed from ``out_a``, the net forward_a delta
went negative → ``V4_SETTLE_ALL`` reconciled via an executor→PM transfer of an
intermediate token the executor holds 0 of (no-custody design) → revert.

The fix mirrors V3-V3-V4 / V3-V4-V3: ``V4_SWAP_DYNAMIC`` (consume the actual
settled forward_a) + ``V4_TAKE_DELTA`` (take the actual produced forward_b →
V2c), eliminating the residual delta.
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
)
from examples.eth_backrun_helpers import (
    PathInfo,
    V2HopInfo,
    V3HopInfo,
    V4HopInfo,
    _3hop_v3_v4_v2,
)

WETH = Web3.to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC = Web3.to_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WBTC = Web3.to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
POOL_MANAGER = Web3.to_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
EXECUTOR = Web3.to_checksum_address("0x1111111111111111111111111111111111111111")
V3A = Web3.to_checksum_address("0x2222222222222222222222222222222222222222")
V2C = Web3.to_checksum_address("0x4444444444444444444444444444444444444444")

# V3_SWAP_COMPACT layout: [0x30][pool:1][zfo:1][amount:12][recipient:1][flen:1][fwd:N]
_V3_HDR = 1 + 1 + 1 + 12 + 1 + 1  # 17 bytes before forward_data


def _path_v3_v4_v2() -> PathInfo:
    """V3a: WETH→USDC, V4b: USDC→WBTC, V2c: WBTC→WETH."""
    ha = V3HopInfo(
        pool_address=V3A,
        token0_address=WETH,
        token1_address=USDC,
        fee=500,
        zfo=True,  # token0(WETH) in, token1(USDC) out
    )
    hb = V4HopInfo(
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "00" * 32,
        currency0_address=USDC,
        currency1_address=WBTC,
        fee=500,
        tick_spacing=10,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,  # currency0(USDC) in, currency1(WBTC) out
    )
    hc = V2HopInfo(
        pool_address=V2C,
        token0_address=WBTC,
        token1_address=WETH,
        fee=30,
        zfo=True,  # token0(WBTC) in, token1(WETH) out
    )
    return PathInfo(hops=[ha, hb, hc])


def _after_execution(cmd: bytes) -> bytes:
    pos = cmd.index(BEGIN_EXECUTION) + 1
    return cmd[pos:]


def _parse_v3_swap(buf: bytes, pos: int) -> tuple[int, int, int]:
    assert buf[pos] == 0x30, f"expected V3_SWAP_COMPACT at {pos}, got {buf[pos]:#x}"
    pool_idx = buf[pos + 1]
    recipient_idx = buf[pos + 15]
    return pool_idx, recipient_idx, pos + _V3_HDR


def _find_v4_unlock_inner(buf: bytes, v3_hdr_len: int = _V3_HDR) -> bytes:
    """Locate the V4_UNLOCK inner payload in a V3-V4-V2 cmd stream.

    Top-level: [V4_SYNC][V3_SWAP_COMPACT(v3a, recipient=PM, ...fwd=a_fwd)]
      a_fwd = [V4_UNLOCK(inner)] + [V2_SWAP_DIRECT] + [ERC20_TRANSFER]
    """
    assert buf[0] == CMD_V4_SYNC[0]
    v3a_pos = 2
    _, _, a_fwd_start = _parse_v3_swap(buf, v3a_pos)
    assert buf[a_fwd_start] == CMD_V4_UNLOCK[0], f"expected V4_UNLOCK at {a_fwd_start}"
    inner_len = buf[a_fwd_start + 1]
    inner_start = a_fwd_start + 2
    return buf[inner_start : inner_start + inner_len]


def test_v3_v4_v2_v3a_recipient_is_pool_manager() -> None:
    """V3a must deliver forward_a to the PoolManager (recipient=PM)."""
    cmd = _3hop_v3_v4_v2(
        _path_v3_v4_v2(),
        optimal_input=10**18,
        hop_outputs=(2_000 * 10**6, 100 * 10**8, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    assert buf[0] == CMD_V4_SYNC[0]
    v3a_pos = 2
    _v3a_pool, a_recipient, _ = _parse_v3_swap(buf, v3a_pos)
    assert a_recipient == SENTINEL_PM, "V3a must deposit forward_a to the PoolManager"


def test_v3_v4_v2_uses_dynamic_swap_and_take_delta() -> None:
    """Regression: V4 swap + take must read actual PM deltas, not static amounts.

    The ``ERC20: transfer amount exceeds balance`` root cause (same as
    V3-V3-V4): ``V4_SWAP_COMPACT(out_a)`` consumed a static solver input
    while V3a delivered its ACTUAL on-chain forward_a output to PM. When the
    actual output < ``out_a``, the net forward_a delta went negative →
    ``V4_SETTLE_ALL`` reconciled via an executor→PM transfer of an
    intermediate token the executor holds 0 of → revert.

    The fixed encoder uses (mirroring V3-V3-V4 / V3-V4-V3):

      V4_SETTLE          (credit V3a's actual forward_a deposit)
      V4_SWAP_DYNAMIC    (consume the actual settled forward_a)
      V4_TAKE_DELTA      (take the actual produced forward_b → V2c)
      V4_SETTLE_ALL      (sweep residual dust)
    """
    cmd = _3hop_v3_v4_v2(
        _path_v3_v4_v2(),
        optimal_input=10**18,
        hop_outputs=(2_000 * 10**6, 100 * 10**8, 11 * 10**17),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None
    buf = _after_execution(cmd)
    inner = _find_v4_unlock_inner(buf)
    assert len(inner) > 0

    assert inner[0] == CMD_V4_SETTLE[0]
    assert CMD_V4_SWAP_DYNAMIC[0] in inner
    assert CMD_V4_SWAP_COMPACT[0] not in inner, (
        "V4_SWAP_COMPACT (static out_a) caused the residual forward_a delta "
        "→ 'transfer amount exceeds balance'; use V4_SWAP_DYNAMIC to consume "
        "the actual settled forward_a"
    )
    assert CMD_V4_TAKE_DELTA[0] in inner
    assert CMD_V4_TAKE_COMPACT[0] not in inner, (
        "V4_TAKE_COMPACT (static out_b) left a residual forward_b delta; "
        "use V4_TAKE_DELTA to take the actual produced forward_b → V2c"
    )
    assert inner[-1] == CMD_V4_SETTLE_ALL[0]

    settle_idx = inner.index(CMD_V4_SETTLE[0])
    swap_dyn_idx = inner.index(CMD_V4_SWAP_DYNAMIC[0])
    take_delta_idx = inner.index(CMD_V4_TAKE_DELTA[0])
    assert settle_idx < swap_dyn_idx < take_delta_idx