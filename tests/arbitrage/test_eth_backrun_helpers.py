"""Regression tests for example command-stream encoders.

These tests focus on command-stream *structure* for path types that have
historically produced settlement reverts. They do not simulate on-chain
execution, but they lock in the encoder ordering that the executor contract
expects.
"""

from web3 import Web3

from contracts.cmd_stream import (
    BEGIN_EXECUTION,
    CMD_V2_SWAP_CALC,
    CMD_V4_SETTLE,
    CMD_V4_SETTLE_ALL,
    CMD_V4_SWAP_COMPACT,
    CMD_V4_SWAP_DYNAMIC,
    CMD_V4_SYNC,
    CMD_V4_TAKE_COMPACT,
    CMD_V4_UNLOCK,
)
from examples.eth_backrun_helpers import PathInfo, V2HopInfo, V4HopInfo, _3hop_v2_v2_v4

WETH = Web3.to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC = Web3.to_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
COMP = Web3.to_checksum_address("0xc00e94Cb662C3520282E6f5717214004A7f26888")
POOL_MANAGER = Web3.to_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
EXECUTOR = Web3.to_checksum_address("0x1111111111111111111111111111111111111111")
V2A = Web3.to_checksum_address("0x2222222222222222222222222222222222222222")
V2B = Web3.to_checksum_address("0x3333333333333333333333333333333333333333")


def _extract_v4_unlock_inner(cmd_stream: bytes) -> bytes:
    """Return the payload inside the first V4_UNLOCK command."""
    exec_pos = cmd_stream.index(BEGIN_EXECUTION)
    unlock_pos = exec_pos + 1
    assert cmd_stream[unlock_pos] == CMD_V4_UNLOCK[0]
    inner_len = cmd_stream[unlock_pos + 1]
    inner_start = unlock_pos + 2
    inner = cmd_stream[inner_start : inner_start + inner_len]
    assert len(inner) == inner_len
    return inner


def test_v2_v2_v4_final_v4_swap_is_dynamic_and_settled() -> None:
    """Regression for V2-V2-V4 CurrencyNotSettled reverts.

    The V4 final hop must be dynamic (it consumes whatever V2b actually
    delivers to the PoolManager) and the forward_b currency must be
    synced+settled before that dynamic swap.
    """
    ha = V2HopInfo(
        pool_key=1,
        pool_address=V2A,
        token0_address=COMP,
        token1_address=WETH,
        fee=30,
        zfo=False,
    )
    hb = V2HopInfo(
        pool_key=2,
        pool_address=V2B,
        token0_address=USDC,
        token1_address=COMP,
        fee=30,
        zfo=False,
    )
    hc = V4HopInfo(
        pool_key=3,
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "00" * 32,
        currency0_address=USDC,
        currency1_address=WETH,
        fee=500,
        tick_spacing=10,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,
    )
    path_info = PathInfo(hops=[ha, hb, hc])

    cmd = _3hop_v2_v2_v4(
        path_info,
        optimal_input=10**18,
        hop_outputs=(10**18, 2_000_000, 2 * 10**18),
        executor_address=EXECUTOR,
        pool_manager_address=POOL_MANAGER,
        weth_address=WETH,
    )
    assert cmd is not None

    inner = _extract_v4_unlock_inner(cmd)

    # Expected layout inside the V4 unlock:
    # [SYNC forward_b][TAKE WETH->V2a][V2a SWAP_CALC][V2b SWAP_CALC]
    # [SETTLE forward_b][V4 SWAP_DYNAMIC][SETTLE_ALL]
    assert inner[0] == CMD_V4_SYNC[0]
    assert inner[2] == CMD_V4_TAKE_COMPACT[0]
    assert inner[17] == CMD_V2_SWAP_CALC[0]
    assert inner[23] == CMD_V2_SWAP_CALC[0]
    assert inner[29] == CMD_V4_SETTLE[0]
    assert inner[30] == CMD_V4_SWAP_DYNAMIC[0]
    assert inner[39] == CMD_V4_SETTLE_ALL[0]

    # The exact-input V4 compact opcode must not appear — using a static solver
    # amount for the final V4 hop caused CurrencyNotSettled when the actual V2b
    # output differed by rounding or stale reserves.
    assert CMD_V4_SWAP_COMPACT not in inner
