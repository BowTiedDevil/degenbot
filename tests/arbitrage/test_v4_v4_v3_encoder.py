"""Regression tests for the V4-V4-V3 three-hop encoder.

V4-V4-V3 reverted on mainnet with ``Comp::_transferTokens: transfer amount
exceeds balance`` (14 reverts in ``perm-V4-V4-V3.log``) because the encoder
routed the intermediate token (forward_b = COMP) through EXECUTOR CUSTODY:
``V4_TAKE_COMPACT(forward_b, executor, out_b)`` sent forward_b to the executor,
then ``ERC20_TRANSFER(forward_b, v3c, out_b)`` forwarded it to V3c during the
callback. When the actual forward_b (V4b's dynamic output) was less than the
static ``out_b``, the executor held less COMP than ``out_b`` → the
``ERC20_TRANSFER(out_b)`` exceeded the executor's COMP balance →
``Comp::_transferTokens: transfer amount exceeds balance``.

The cmd_executor is designed to NEVER custody intermediate (non-profit)
tokens. The fix matches the executor's own ``TestV4V4V3``
(``~/code/executor/tests/test_cmd_executor_three_hop_optimized.py``): send
forward_b DIRECTLY to V3c via ``V4_TAKE`` as the V3c swap's forward_data, so
the V3c pool's optimistic-input check is satisfied by the actual forward_b
delivered during the callback — the executor never touches the intermediate
token.

NOTE: this encoder fix eliminates the executor-custody design violation + the
``Comp::_transferTokens`` revert. It uses static ``out_b`` (matching the V3c
swap amount), so it is exact when V4b's actual output == ``out_b`` (V4 cl-math
matches onchain when no drift). The V4a hop[0] drift (sqrt_price/tick
divergence — a V4 state-sync lag) is a SEPARATE root cause that must be
resolved for the static amounts to agree onchain; promoted as a follow-up.
"""

from web3 import Web3

from examples.cmd_stream import (
    BEGIN_EXECUTION,
    CMD_ERC20_TRANSFER,
    CMD_V4_SETTLE_ALL,
    CMD_V4_SWAP_COMPACT,
    CMD_V4_SWAP_DYNAMIC,
    CMD_V4_TAKE_COMPACT,
    CMD_V4_UNLOCK,
    SENTINEL_SELF,
)
from examples.eth_backrun_helpers import (
    PathInfo,
    V3HopInfo,
    V4HopInfo,
    _3hop_v4_v4_v3,
)

WETH = Web3.to_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
USDC = Web3.to_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
WBTC = Web3.to_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
POOL_MANAGER = Web3.to_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
EXECUTOR = Web3.to_checksum_address("0x1111111111111111111111111111111111111111")
V3C = Web3.to_checksum_address("0x3333333333333333333333333333333333333333")


def _path_v4_v4_v3() -> PathInfo:
    """V4a: WETH→USDC, V4b: USDC→WBTC, V3c: WBTC→WETH."""
    ha = V4HopInfo(
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "00" * 32,
        currency0_address=WETH,
        currency1_address=USDC,
        fee=3000,
        tick_spacing=60,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,  # currency0(WETH) in, currency1(USDC) out
    )
    hb = V4HopInfo(
        pool_manager_address=POOL_MANAGER,
        pool_id_hex="0x" + "11" * 32,
        currency0_address=USDC,
        currency1_address=WBTC,
        fee=500,
        tick_spacing=10,
        hook_address="0x0000000000000000000000000000000000000000",
        zfo=True,  # currency0(USDC) in, currency1(WBTC) out
    )
    hc = V3HopInfo(
        pool_address=V3C,
        token0_address=WBTC,
        token1_address=WETH,
        fee=100,
        zfo=True,  # token0(WBTC) in, token1(WETH) out
    )
    return PathInfo(hops=[ha, hb, hc])


def _after_execution(cmd: bytes) -> bytes:
    pos = cmd.index(BEGIN_EXECUTION) + 1
    return cmd[pos:]


def _find_v4_unlock_inner(buf: bytes) -> bytes:
    """Locate the V4_UNLOCK inner payload (V4-V4-V3 wraps everything in one unlock)."""
    unlock_pos = buf.index(CMD_V4_UNLOCK[0])
    inner_len = buf[unlock_pos + 1]
    inner_start = unlock_pos + 2
    return buf[inner_start : inner_start + inner_len]


def test_v4_v4_v3_forward_b_goes_directly_to_v3c_not_executor() -> None:
    """Regression: forward_b (intermediate) must NOT pass through executor custody.

    The ``Comp::_transferTokens: transfer amount exceeds balance`` root cause:
    the old encoder did ``V4_TAKE_COMPACT(forward_b, executor, out_b)`` then
    ``ERC20_TRANSFER(forward_b, v3c, out_b)`` — routing the intermediate COMP
    through the executor, which holds 0 of it by design (no-custody). When
    actual forward_b < static out_b, the ERC20_TRANSFER exceeded the executor's
    balance → revert.

    The fix (matching the executor's own TestV4V4V3): ``V4_TAKE`` sends
    forward_b DIRECTLY to V3c as the V3c swap's forward_data — the executor
    never touches the intermediate token.
    """
    cmd = _3hop_v4_v4_v3(
        _path_v4_v4_v3(),
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

    # The encoder must NOT emit any ERC20_TRANSFER of the intermediate token
    # (forward_b) — the executor never custodies it.
    assert CMD_ERC20_TRANSFER[0] not in inner, (
        "ERC20_TRANSFER present — the executor is custoding/routing the "
        "intermediate forward_b, violating the no-custody design "
        "(Comp::_transferTokens: transfer amount exceeds balance)"
    )

    # V4_TAKE_COMPACT must target V3c (recipient = the V3c pool index), NOT
    # the executor (SENTINEL_SELF). The take is the V3c swap's forward_data.
    assert CMD_V4_TAKE_COMPACT[0] in inner
    take_pos = inner.index(CMD_V4_TAKE_COMPACT[0])
    # Layout: [0x52][currency:1][recipient:1][amount:12]
    take_recipient = inner[take_pos + 2]
    assert take_recipient != SENTINEL_SELF, (
        "V4_TAKE sends forward_b to the executor (custody!) — must send "
        "directly to V3c as the V3c swap's forward_data"
    )


def test_v4_v4_v3_take_is_v3c_swap_forward_data() -> None:
    """The V4_TAKE (forward_b → V3c) must be the V3c swap's forward_data.

    During V3c's callback, the V4_TAKE delivers forward_b to the V3c pool,
    satisfying V3's optimistic-input balance check (balance_before + input <=
    balance_after, where balance_after includes the take's deposit). This is
    the no-custody settlement: forward_b flows PM → V3c directly, never via
    the executor's balance.
    """
    from examples.cmd_stream import CMD_V3_SWAP_COMPACT

    cmd = _3hop_v4_v4_v3(
        _path_v4_v4_v3(),
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

    # The inner sequence: V4a swap, V4b swap (dynamic), V3c swap (with
    # forward_data = V4_TAKE), SETTLE_ALL.
    assert CMD_V4_SWAP_COMPACT[0] in inner  # V4a (static optimal_input)
    assert CMD_V4_SWAP_DYNAMIC[0] in inner  # V4b (dynamic forward_a)
    v3c_pos = inner.index(CMD_V3_SWAP_COMPACT[0])
    take_pos = inner.index(CMD_V4_TAKE_COMPACT[0])
    # The V4_TAKE must come AFTER the V4b swap (which produces forward_b) and
    # is the V3c swap's forward_data (so it runs during the V3c callback, i.e.
    # AFTER the V3c swap command starts).
    v4b_pos = inner.index(CMD_V4_SWAP_DYNAMIC[0])
    assert v4b_pos < take_pos, "V4_TAKE must follow V4b (which produces forward_b)"
    assert take_pos > v3c_pos or take_pos == v3c_pos + 17, (
        "V4_TAKE is the V3c swap's forward_data (runs during the V3c callback)"
    )
    assert inner[-1] == CMD_V4_SETTLE_ALL[0]


def test_v4_v4_v3_no_erc20_transfer_of_any_token() -> None:
    """The executor never custodies ANY intermediate token in V4-V4-V3.

    The no-custody design: every intermediate token routes pool-to-pool via V4
    deltas. The only ERC20_TRANSFER the executor makes is WETH (the flash-
    borrowed input + profit), which is handled implicitly by V4 settlement,
    not an explicit ERC20_TRANSFER in the inner sequence.
    """
    cmd = _3hop_v4_v4_v3(
        _path_v4_v4_v3(),
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
    assert CMD_ERC20_TRANSFER[0] not in inner, (
        "V4-V4-V3 must use V4 delta netting (no ERC20_TRANSFER custody of any "
        "intermediate token) — executor custodies only WETH via V4 settlement"
    )