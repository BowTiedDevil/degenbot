"""
ABI-fidelity tests for the fake Uniswap contracts.

The fakes mirror real V2/V3/V4 entry points whose Solidity signatures use
unbounded `bytes`. These tests pin the fakes to that permissiveness: a
payload larger than any internal cap must be accepted and reach the
callback byte-for-byte.

Real counterparts (all unbounded `bytes`):
- UniswapV2Pair.swap(uint,uint,address,bytes) + uniswapV2Call/hook/pancakeCall(...,bytes)
- UniswapV3Pool.swap(...,bytes) + uniswapV3SwapCallback/pancakeV3SwapCallback(...,bytes)
- v4 IPoolManager.unlock(bytes) returns (bytes), swap/modifyLiquidity/donate(...,bytes hookData)
"""

import pytest
from eth_utils import keccak

from .conftest_shared import _make_pool_key, _setup_v3

# fake_uniswap_v2_pair callback variants: 0 uniswapV2Call, 1 hook, 2 pancakeCall
V2_CALLBACK_VARIANTS = (0, 1, 2)
# fake_uniswap_v3_pool callback variants: 0 uniswapV3SwapCallback, 1 pancakeV3SwapCallback
V3_CALLBACK_VARIANTS = (0, 1)

MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

SALT = b"\x11" * 32


def _self_verifying_payload(n: int) -> bytes:
    """Word 0 = total length, word 1 = keccak256 of the rest except word 1."""
    filler = b"\xab" * (n - 64)
    covered = n.to_bytes(32, "big") + filler
    return n.to_bytes(32, "big") + keccak(covered) + filler


def _covered(payload: bytes) -> bytes:
    return payload[:32] + payload[64:]


class TestFakeV4PoolManagerAbi:
    def test_unlock_round_trips_oversized_data(self, project, owner_account, v4_pm, echo):
        payload = _self_verifying_payload(1200)
        receipt = echo.do_unlock(v4_pm.address, payload, sender=owner_account)
        out = receipt.return_value
        assert isinstance(out, (bytes, bytearray)) and bytes(out) == payload
        assert echo.last_len() == 1200
        assert bytes(echo.last_digest()) == keccak(_covered(payload))

    def test_swap_accepts_oversized_hook_data(self, owner_account, v4_pm, echo):
        key = _make_pool_key(v4_pm.address, echo.address)
        params = (True, 1, MIN_SQRT_RATIO + 1)
        hook_data = _self_verifying_payload(600)
        with pytest.raises(Exception, match="ManagerLocked"):
            v4_pm.swap(key, params, hook_data, sender=owner_account)

    def test_modify_liquidity_accepts_oversized_hook_data(self, owner_account, v4_pm, echo):
        key = _make_pool_key(v4_pm.address, echo.address)
        params = (-60, 60, 0, SALT)
        hook_data = _self_verifying_payload(100)
        with pytest.raises(Exception, match="ManagerLocked"):
            v4_pm.modifyLiquidity(key, params, hook_data, sender=owner_account)

    def test_donate_accepts_oversized_hook_data(self, owner_account, v4_pm, echo):
        key = _make_pool_key(v4_pm.address, echo.address)
        hook_data = _self_verifying_payload(100)
        with pytest.raises(Exception, match="ManagerLocked"):
            v4_pm.donate(key, 1, 1, hook_data, sender=owner_account)


class TestFakeV2PairAbi:
    @pytest.mark.parametrize("variant", V2_CALLBACK_VARIANTS)
    def test_swap_delivers_oversized_callback_data(
        self, project, owner_account, usdc, wbtc, echo, variant
    ):
        pair = project.fake_uniswap_v2_pair.deploy(
            usdc.address, wbtc.address, variant, 3000, sender=owner_account
        )
        usdc.mint(pair.address, 10**12, sender=owner_account)
        wbtc.mint(pair.address, 10**12, sender=owner_account)
        pair.sync(sender=owner_account)

        payload = _self_verifying_payload(600)
        with pytest.raises(Exception, match="ECHO_VERIFIED"):
            pair.swap(0, 1, echo.address, payload, sender=owner_account)


class TestFakeV3PoolAbi:
    @pytest.mark.parametrize("variant", V3_CALLBACK_VARIANTS)
    def test_swap_delivers_oversized_callback_data(
        self, project, owner_account, usdc, wbtc, echo, variant
    ):
        pool = project.fake_uniswap_v3_pool.deploy(
            usdc.address, wbtc.address, variant, 3000, sender=owner_account
        )
        amount_in = 10**9
        zfo, _ = _setup_v3(pool, usdc, wbtc, amount_in, amount_in, owner_account)
        limit = MIN_SQRT_RATIO + 1 if zfo else MAX_SQRT_RATIO - 1

        payload = _self_verifying_payload(600)
        with pytest.raises(Exception, match="ECHO_VERIFIED"):
            echo.do_swap_v3(
                pool.address, echo.address, zfo, -amount_in, limit, payload, sender=owner_account
            )


@pytest.fixture
def echo(project, owner_account):
    return project.echo_callee.deploy(sender=owner_account)
