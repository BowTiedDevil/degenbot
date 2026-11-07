"""Camelot V2 construction and fee seam over recorded chain data.

Both Camelot pools build through the real ``Bot.build_pool`` path over their
per-block ``OfflineProvider`` corpora under ``tests/fixtures/chain_data/42161``
(``construction_*``, machine-captured by
``scripts/record_construction_cassettes.py``). The recorded ``getAmountOut``
answer is the pool contract's own quote at the pinned block — the on-chain
half of the parity assertion, replayed from the corpus with no RPC. The
``replay_makes_no_network_calls`` import arms the replay dial-block: a socket
dial during these tests is a defect.
"""

from __future__ import annotations

import json
from fractions import Fraction
from pathlib import Path

from degenbot.abi import decode as abi_decode
from degenbot.checksum_cache import get_checksum_address
from degenbot.crypto import function_selector
from degenbot.provider import OfflineProvider
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.golden.oracle import replay_makes_no_network_calls  # ruff: ignore[unused-import]
from tests.helpers.bot_factory import make_bot_with_provider

CAMELOT_V2_FACTORY_ADDRESS = get_checksum_address("0x6EcCab422D763aC031210895C81787E87B43A652")
CAMELOT_WETH_USDC_LP_ADDRESS = get_checksum_address("0x84652bb2539513BAf36e225c930Fdd8eaa63CE27")
CAMELOT_MIM_USDC_LP_ADDRESS = get_checksum_address("0x68A0859de50B4Dfc6EFEbE981cA906D38Cdb0D1F")


_CORPUS_DIR = Path(__file__).resolve().parents[2] / "fixtures" / "chain_data" / "42161"
_WETH_USDC_CORPUS = _CORPUS_DIR / "construction_camelot_v2_weth_usdc_block477785000.json"
_MIM_USDC_CORPUS = _CORPUS_DIR / "construction_camelot_v2_mim_usdc_block477785000.json"

_ARBITRUM_BLOCK = 477_785_000


def _recorded_get_amount_out(
    provider: OfflineProvider,
    pool_address: str,
    token_in: str,
    amount_in: int,
) -> int:
    """The pool contract's recorded ``getAmountOut`` answer for one input."""

    calldata = encode_function_calldata("getAmountOut(uint256,address)", [amount_in, token_in])
    (amount_out,) = abi_decode(
        types=["uint256"],
        data=provider.call(pool_address.lower(), calldata, block=_ARBITRUM_BLOCK),
    )
    return amount_out


_CAMELOT_FEE_SCALE = 100_000  # the pools' recorded FEE_DENOMINATOR()


def _recorded_reserve_fee_words(corpus_path: Path, pool_address: str) -> tuple[int, int]:
    """The ``_token0FeePercent``/``_token1FeePercent`` words of the recorded ``getReserves()``."""

    calls = json.loads(corpus_path.read_text(encoding="utf-8"))["calls"]
    key = f"{pool_address.lower()}:0x{function_selector('getReserves()').hex()}"
    raw = bytes.fromhex(calls[key])
    return int.from_bytes(raw[2 * 32 : 3 * 32], "big"), int.from_bytes(raw[3 * 32 : 4 * 32], "big")


def _recorded_uint(corpus_path: Path, pool_address: str, signature: str) -> int:
    """The decoded word of one recorded no-arg getter."""

    calls = json.loads(corpus_path.read_text(encoding="utf-8"))["calls"]
    key = f"{pool_address.lower()}:0x{function_selector(signature).hex()}"
    return int.from_bytes(bytes.fromhex(calls[key]), "big")


def test_create_camelot_v2_stable_pool() -> None:
    """The MIM/USDC stable pair resolves to the camelot-v2-stable preset.

    The fee is per-pool state, not the preset's: the recorded
    ``getReserves()`` answers (40, 40) over the pool's fee denominator, i.e.
    0.04%. A pool carrying the preset's "read the pool" marker would charge
    100% and quote zero for every input.
    """

    provider = OfflineProvider.from_json_file(_MIM_USDC_CORPUS)
    lp = make_bot_with_provider(provider).build_pool(CAMELOT_MIM_USDC_LP_ADDRESS)
    assert isinstance(lp, UniswapV2Pool)
    assert lp.dex.variant == "camelot-v2-stable"
    assert lp.fee_token0 == Fraction(40, 100_000)
    assert lp.fee_token1 == Fraction(40, 100_000)
    assert lp.fee_denominator == 100_000

    token_in = lp.token0  # MIM token

    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    contract_amount = _recorded_get_amount_out(provider, lp.address, token_in.address, amount_in)
    assert contract_amount > 0
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )


def test_create_camelot_v2_pool() -> None:
    """The WETH/USDC volatile pair resolves to the camelot-v2-volatile preset."""

    provider = OfflineProvider.from_json_file(_WETH_USDC_CORPUS)
    lp = make_bot_with_provider(provider).build_pool(CAMELOT_WETH_USDC_LP_ADDRESS)
    assert isinstance(lp, UniswapV2Pool)
    assert lp.dex.variant == "camelot-v2-volatile"

    token_in = lp.token1  # USDC

    amount_in = 1000 * 10**token_in.decimals  # nominal value of $1000

    contract_amount = _recorded_get_amount_out(provider, lp.address, token_in.address, amount_in)
    assert contract_amount == lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )


def test_camelot_fee_fields_are_per_pool_state_fractions() -> None:
    """The fee seam: per-token fee Fractions and the stable scale are pool state.

    ``stable_swap`` selects the stable calc branch; the per-token fees are
    Fractions of the Camelot fee scale read off the recorded ``getReserves()``
    fee words — never the preset's ``(0, denom)`` "read the pool" marker, which
    would charge 100% and quote zero for every input. ``fee_denominator`` is
    the stable math's integer fee scale: recorded on the stable pair, absent
    on the volatile one (its calc ignores the scale).
    """

    assert (
        _recorded_uint(_MIM_USDC_CORPUS, CAMELOT_MIM_USDC_LP_ADDRESS, "FEE_DENOMINATOR()")
        == _CAMELOT_FEE_SCALE
    )

    for corpus_path, pool_address, stable, fee_denominator in (
        (_MIM_USDC_CORPUS, CAMELOT_MIM_USDC_LP_ADDRESS, True, _CAMELOT_FEE_SCALE),
        (_WETH_USDC_CORPUS, CAMELOT_WETH_USDC_LP_ADDRESS, False, None),
    ):
        provider = OfflineProvider.from_json_file(corpus_path)
        lp = make_bot_with_provider(provider).build_pool(pool_address)
        fee_percent0, fee_percent1 = _recorded_reserve_fee_words(corpus_path, pool_address)

        assert lp.stable_swap is stable
        assert lp.fee_denominator == fee_denominator
        assert isinstance(lp.fee_token0, Fraction)
        assert isinstance(lp.fee_token1, Fraction)
        assert lp.fee_token0 == Fraction(fee_percent0, _CAMELOT_FEE_SCALE)
        assert lp.fee_token1 == Fraction(fee_percent1, _CAMELOT_FEE_SCALE)
        assert 0 < lp.fee_token0 < Fraction(1, 100)
        assert 0 < lp.fee_token1 < Fraction(1, 100)
