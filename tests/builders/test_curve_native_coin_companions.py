"""Curve's all-Es native coin resolves to the native companion in `finalize`.

The all-Es coin is Curve's PRIVATE native-coin spelling (ADR-097 D3): it is
never a token contract address, and generic construction refuses it loudly.
`CurvePoolBuilder.finalize` is the Curve call site that translates it — to the
conceptual `NativeToken` companion via `build_native` — for both the coin and
the metapool underlying lists. Every other coin takes the ordinary ERC-20
build, and zero is deliberately NOT a Curve native spelling: it stays on the
ordinary path (which refuses it) instead of being normalized.
"""

from __future__ import annotations

from unittest.mock import MagicMock

from degenbot.builders.curve_pool_builder import CurvePoolBuilder
from tests.sweep_pin import SWEEP_FORK_BLOCK

ALL_ES = "0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE"
COIN_A = "0x" + "11" * 20
COIN_B = "0x" + "22" * 20
COIN_C = "0x" + "33" * 20
LP_TOKEN = "0x" + "44" * 20
ZERO_ADDRESS = "0x" + "00" * 20


def _finalize_over(
    token_addresses: list[str],
    token_addresses_underlying: list[str] | None = None,
    lp_token: str | None = None,
) -> MagicMock:
    """Drive `finalize` over a stub Curve arm; return the recording builder."""
    erc20_builder = MagicMock()
    ctx = MagicMock()
    ctx.default_chain_id = 1
    ctx.erc20_builder = erc20_builder

    handle = MagicMock()
    arm = handle.curve.return_value
    arm.base_pool_address.return_value = None
    arm.token_addresses.return_value = token_addresses
    arm.token_addresses_underlying.return_value = token_addresses_underlying
    arm.lp_token_address.return_value = lp_token

    CurvePoolBuilder(ctx).finalize(
        handle,
        chain_id=1,
        state_block=SWEEP_FORK_BLOCK,
        silent=True,
    )
    return erc20_builder


def test_the_all_es_coin_resolves_to_the_native_companion() -> None:
    """The native coin builds as `NativeToken`; the real coin builds as ERC-20."""
    erc20_builder = _finalize_over([ALL_ES, COIN_A])

    erc20_builder.build_native.assert_called_once_with(chain_id=1, silent=True)
    erc20_builder.build.assert_called_once_with(COIN_A, chain_id=1, silent=True)


def test_an_all_es_underlying_coin_resolves_to_the_native_companion() -> None:
    """The metapool underlying list translates the same way (LP stays ERC-20)."""
    erc20_builder = _finalize_over(
        [COIN_A, COIN_B],
        token_addresses_underlying=[ALL_ES, COIN_C],
        lp_token=LP_TOKEN,
    )

    erc20_builder.build_native.assert_called_once_with(chain_id=1, silent=True)
    assert [args.args[0] for args in erc20_builder.build.call_args_list] == [
        COIN_A,
        COIN_B,
        COIN_C,
        LP_TOKEN,
    ]


def test_zero_is_not_a_curve_native_spelling() -> None:
    """The enumeration terminator stays on the ordinary path — never native."""
    erc20_builder = _finalize_over([ZERO_ADDRESS, COIN_A])

    erc20_builder.build_native.assert_not_called()
    assert [args.args[0] for args in erc20_builder.build.call_args_list] == [ZERO_ADDRESS, COIN_A]
