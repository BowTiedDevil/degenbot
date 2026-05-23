"""Tests for Curve pool A ramping parameter detection."""

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.detection.a_ramping import detect_a_ramping
from degenbot.provider.call_helpers import encode_function_calldata
from tests.curve.detection.fake_provider import make_fake_pool_io

POOL_ADDR = get_checksum_address("0xbEbc44782C7DB0a1A60Cb6fe97d0b483032FF1C7")

# Selectors
INITIAL_A = encode_function_calldata("initial_A()", [])[:4]
INITIAL_A_TIME = encode_function_calldata("initial_A_time()", [])[:4]
FUTURE_A = encode_function_calldata("future_A()", [])[:4]
FUTURE_A_TIME = encode_function_calldata("future_A_time()", [])[:4]


def _encode_uint256(val: int) -> bytes:
    return eth_abi.abi.encode(["uint256"], [val])


class TestDetectARamping:
    def test_no_ramping_parameters(self):
        """Pool that doesn't support A ramping functions returns has_ramping=False."""
        io = make_fake_pool_io({})  # All calls revert

        result = detect_a_ramping(io, POOL_ADDR, block_identifier=18_000_000)
        assert not result.has_ramping
        assert result.initial_a is None
        assert result.initial_a_time is None
        assert result.future_a is None
        assert result.future_a_time is None

    def test_active_ramping(self):
        """Pool with all four A ramping parameters returns has_ramping=True."""
        io = make_fake_pool_io({
            INITIAL_A: _encode_uint256(1000),
            INITIAL_A_TIME: _encode_uint256(1700000000),
            FUTURE_A: _encode_uint256(2000),
            FUTURE_A_TIME: _encode_uint256(1700086400),
        })

        result = detect_a_ramping(io, POOL_ADDR, block_identifier=18_000_000)
        assert result.has_ramping
        assert result.initial_a == 1000
        assert result.initial_a_time == 1700000000
        assert result.future_a == 2000
        assert result.future_a_time == 1700086400

    def test_partial_ramping_returns_no_ramping(self):
        """If any of the four ramping calls reverts, has_ramping is False."""
        # Only provide 3 of 4 ramping selectors
        io = make_fake_pool_io({
            INITIAL_A: _encode_uint256(1000),
            INITIAL_A_TIME: _encode_uint256(1700000000),
            FUTURE_A: _encode_uint256(2000),
            # Missing FUTURE_A_TIME — will revert
        })

        result = detect_a_ramping(io, POOL_ADDR, block_identifier=18_000_000)
        assert not result.has_ramping
