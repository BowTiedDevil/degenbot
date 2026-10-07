"""Offline replay proof for the py_oracle oracle-answer corpus.

The fork-bound on-chain parity tests' oracle truth is committed in two forms:
the per-test golden ints under ``tests/golden/data`` (what the parity tests'
replay asserts against) and, for the same surfaces, the per-block
``OfflineProvider`` corpus under ``tests/fixtures/chain_data``
(``py_oracle_<scenario>_block<N>.json``, recorded by the
``record_py_oracle_corpus`` example through the recording transport). This
module is the drift gate between the two, run fully offline: every recorded
answer must decode back to the exact golden int (or the exact recorded
revert) through the real ``OfflineProvider`` runtime, and the recorded call
surface must stay exactly the surface the parity goldens pin.

The corpus is the oracle, not a re-derivation: nothing here recomputes swap
maths. It decodes wire answers and compares them to the committed golden
ints, so a mutated byte, a wrong pin, or a shrunken surface all fail loudly
- the seeded-divergence probes at the bottom make that failure mode a test.
"""

from __future__ import annotations

import json
import re
import socket
from dataclasses import dataclass
from functools import cache
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import ContractLogicError
from degenbot.provider import OfflineProvider
from tests.golden.oracle import GOLDEN_ROOT

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

REPO_ROOT = Path(__file__).resolve().parents[2]
CORPUS_ROOT = REPO_ROOT / "tests" / "fixtures" / "chain_data"

# The golden keys' pool pins (v3/v4 keys embed the pool, not the quoter the
# call targets; camelot keys embed the pool the call targets; the balancer
# keys embed the pool inside the querySwap calldata; the aerodrome v3 and
# pancakeswap keys embed the pool while their calls target the quoter/router).
V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"
V4_POOL_ID = "21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
CAMELOT_POOL_ADDRESS = "0x84652bb2539513BAf36e225c930Fdd8eaa63CE27"
CURVE_TRIPOOL_ADDRESS = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"
CURVE_TRICRYPTO_ADDRESS = "0x80466c64868E1ab14a1Ddf27A676C3fcBE638Fe5"
CURVE_METAPOOL_ADDRESS = "0x618788357D0EBd8A37e763ADab3bc575D54c2C7d"
AERODROME_V3_POOL_ADDRESS = "0x47cA96Ea59C13F72745928887f84C9F52C3D7348"
AERODROME_V2_VOLATILE_POOL_ADDRESS = "0x2722C8f9B5E2aC72D1f225f8e8c990E449ba0078"
AERODROME_V2_STABLE_POOL_ADDRESS = "0x0B25c51637c43decd6CC1C1e3da4518D54ddb528"
PANCAKE_V2_POOL_ADDRESS = "0x92363F9817f92a7ae0592A4cb29959A88d885cc8"

# Call targets (lowercase - the corpus key form).
_V3_QUOTER = "b27308f9f90d607463bb33ea1bebb41c27ce5ab6"
_V4_QUOTER = "52f0e24d1c21c8a0cb1e5a5dd6198556bd9e1203"
_CAMELOT_POOL = "84652bb2539513baf36e225c930fdd8eaa63ce27"
_BALANCER_QUERIES = "e39b5e3b6d74016b2f6a9673d7d7493b6df549d5"
_AERODROME_V3_QUOTER = "254cf9e1e6e233aa1ac962cb9b05b2cfeaae15b0"
_PANCAKE_V2_ROUTER = "8cfe327cec66d1c090dd72bd0ff11d690c33a2eb"

# The aerodrome v3 cassette carries no token addresses; the parity test pins
# them, and the corpus keys name them only as symbols.
_AERODROME_V3_TOKEN_SYMBOLS = {
    "2ae3f1ec7f1f5012cfeab0185bfc7aa3cf0dec22": "cbETH",
    "4200000000000000000000000000000000000006": "WETH",
}

# Calldata selectors the decoders branch on.
_V3_INPUT_SELECTOR = "f7729d43"
_V3_OUTPUT_SELECTOR = "30d07f21"
_V4_INPUT_SELECTOR = "aa9d21cb"
_V4_OUTPUT_SELECTOR = "58733073"
_QUERY_SWAP_SELECTOR = "e969f6b3"
_GET_DY_SELECTOR = "5e0d443f"
_GET_DY_UINT256_SELECTOR = "556d6e9f"
_GET_DY_UNDERLYING_SELECTOR = "07211ef7"
_CALC_WITHDRAW_SELECTOR = "cc2b27d7"

_V3_SYMBOLS = {
    "2260fac5e5542a773aa44fbcfedf7c193bc2c599": "WBTC",
    "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2": "WETH",
}
_V4_PAIRS = {True: "ETH->USDC", False: "USDC->ETH"}
_CAMELOT_PAIR = "USDC->WETH"

_CALL_KEY_RE = re.compile(r"0x[0-9a-f]{40}:0x[0-9a-f]+")
_CODE_KEY_RE = re.compile(r"0x[0-9a-f]{40}")
_HEX_RE = re.compile(r"[0-9a-f]+")

_OFFLINE_DIAL_MSG = "offline replay must not dial; a network attempt is a defect"

_PENDING_CORPUS_REASON = (
    "py_oracle corpus not recorded: no configured endpoint serves the pinned "
    "state (camelot at block 477785000: the reachable publicnode endpoint "
    "rejects archive state at get_code with 'Archive requests require a "
    "personal token'; base: mainnet.base.org rate-limits bursty recording). "
    "Record with the record_py_oracle_corpus example once an endpoint does."
)


def _words(data_hex: str) -> list[str]:
    """The 32-byte ABI words after a function selector."""
    body = data_hex[8:]
    return [body[i * 64 : (i + 1) * 64] for i in range(len(body) // 64)]


@cache
def _balancer_cassettes() -> dict[str, dict]:
    """lowercase pool address -> cassette, for the golden keys' index/symbol maps."""
    index: dict[str, dict] = {}
    for path in sorted((CORPUS_ROOT / "1").glob("balancer_*.json")):
        cassette = json.loads(path.read_text())
        index[cassette["address"].lower()] = cassette
    return index


@cache
def _v2_style_symbols(cassette_path: str) -> dict[str, str]:
    """bare lowercase hex token address -> symbol, from a token0/token1 cassette."""
    cassette = json.loads(Path(cassette_path).read_text(encoding="utf-8"))
    return {
        cassette["token0"]["address"].lower().removeprefix("0x"): cassette["token0"]["symbol"],
        cassette["token1"]["address"].lower().removeprefix("0x"): cassette["token1"]["symbol"],
    }


@cache
def _curve_symbol_tables(cassette_path: str) -> tuple[tuple[str, ...], tuple[str, ...]]:
    """(pool token symbols, underlying token symbols) from a curve cassette."""
    cassette = json.loads(Path(cassette_path).read_text(encoding="utf-8"))
    owner = cassette.get("immutable", cassette)
    pool_symbols = tuple(token["symbol"] for token in owner["tokens"])
    # Only metapool cassettes carry a distinct underlying coin list; a base
    # pool's underlying coins are its pool coins. Those corpora record only
    # get_dy, and a stray get_dy_underlying decodes to a stale key the
    # key-set gate rejects.
    underlying = owner.get("tokens_underlying", owner["tokens"])
    return pool_symbols, tuple(token["symbol"] for token in underlying)


def _v3_golden_key(data_hex: str) -> str:
    words = _words(data_hex)
    method = {
        _V3_INPUT_SELECTOR: "quoteExactInputSingle",
        _V3_OUTPUT_SELECTOR: "quoteExactOutputSingle",
    }[data_hex[:8]]
    token_in = _V3_SYMBOLS[words[0][24:]]
    token_out = _V3_SYMBOLS[words[1][24:]]
    return f"{V3_POOL_ADDRESS}|{method}|{token_in}->{token_out}|{int(words[3], 16)}"


def _v4_golden_key(data_hex: str) -> str:
    words = _words(data_hex)
    method = {
        _V4_INPUT_SELECTOR: "quoteExactInputSingle",
        _V4_OUTPUT_SELECTOR: "quoteExactOutputSingle",
    }[data_hex[:8]]
    zero_for_one = int(words[6], 16) == 1
    return f"{V4_POOL_ID}|{method}|{_V4_PAIRS[zero_for_one]}|{int(words[7], 16)}"


def _camelot_golden_key(data_hex: str) -> str:
    words = _words(data_hex)
    return f"{CAMELOT_POOL_ADDRESS}|getAmountOut|{_CAMELOT_PAIR}|{int(words[0], 16)}"


def _balancer_golden_key_factory(key_style: str) -> Callable[[str], str]:
    """Decoder for querySwap calldata -> the golden file's key form.

    ``index`` keys carry ``i=<n>|j=<n>`` (stable + expanded pools); ``symbol``
    keys carry the token symbols (the three base weighted pools).
    """

    def _decode(data_hex: str) -> str:
        words = _words(data_hex)
        assert data_hex[:8] == _QUERY_SWAP_SELECTOR
        pool_addr = "0x" + words[5][:40]  # head: offset, funds, poolId
        label = "GIVEN_IN" if int(words[6], 16) == 0 else "GIVEN_OUT"
        asset_in = words[7][24:]
        asset_out = words[8][24:]
        amount = int(words[9], 16)
        cassette = _balancer_cassettes()[pool_addr]
        addresses = [
            token["address"].lower().removeprefix("0x") for token in cassette["tokens"]
        ]
        symbols = [token["symbol"] for token in cassette["tokens"]]
        token_in = addresses.index(asset_in)
        token_out = addresses.index(asset_out)
        pool = get_checksum_address(pool_addr)
        if key_style == "index":
            return f"{pool}|querySwap|{label}|i={token_in}|j={token_out}|{amount}"
        return f"{pool}|querySwap|{label}|{symbols[token_in]}->{symbols[token_out]}|{amount}"

    return _decode


def _curve_get_dy_key_factory(
    pool: str,
    cassette_path: Path,
    block_tag: int | None = None,
) -> Callable[[str], str]:
    pool_symbols, underlying_symbols = _curve_symbol_tables(str(cassette_path))

    def _decode(data_hex: str) -> str:
        words = _words(data_hex)
        token_in, token_out, amount = int(words[0], 16), int(words[1], 16), int(words[2], 16)
        tag = f"blk{block_tag}|" if block_tag is not None else ""
        if data_hex[:8] in (_GET_DY_SELECTOR, _GET_DY_UINT256_SELECTOR):
            return (
                f"{pool}|get_dy|{pool_symbols[token_in]}->{pool_symbols[token_out]}|{tag}{amount}"
            )
        return (
            f"{pool}|get_dy_underlying|"
            f"{underlying_symbols[token_in]}->{underlying_symbols[token_out]}|{tag}{amount}"
        )

    return _decode


def _curve_calc_key_factory(pool: str, n_tokens: int) -> Callable[[str], str]:
    def _decode(data_hex: str) -> str:
        words = _words(data_hex)
        if data_hex[:8] == _CALC_WITHDRAW_SELECTOR:
            return f"{pool}|calc_withdraw_one_coin|i={int(words[1], 16)}|{int(words[0], 16)}"
        slot = next(i for i in range(n_tokens) if int(words[i], 16))
        return f"{pool}|calc_token_amount|deposit|i={slot}|{int(words[slot], 16)}"

    return _decode


def _aero_v2_key_factory(pool: str, cassette_path: Path) -> Callable[[str], str]:
    symbols = _v2_style_symbols(str(cassette_path))

    def _decode(data_hex: str) -> str:
        words = _words(data_hex)
        token_in = symbols[words[1][24:]]
        # getAmountOut names only the sell token; the receive side is the
        # pool's other token.
        token_out = next(sym for sym in symbols.values() if sym != token_in)
        return f"{pool}|getAmountOut|{token_in}->{token_out}|{int(words[0], 16)}"

    return _decode


def _aero_v3_key_factory() -> Callable[[str], str]:
    def _decode(data_hex: str) -> str:
        words = _words(data_hex)
        token_in = _AERODROME_V3_TOKEN_SYMBOLS[words[0][24:]]
        token_out = _AERODROME_V3_TOKEN_SYMBOLS[words[1][24:]]
        return (
            f"{AERODROME_V3_POOL_ADDRESS}|quoteExactInputSingle|"
            f"{token_in}->{token_out}|{int(words[2], 16)}"
        )

    return _decode


def _pancake_key_factory(cassette_path: Path) -> Callable[[str], str]:
    symbols = _v2_style_symbols(str(cassette_path))

    def _decode(data_hex: str) -> str:
        words = _words(data_hex)
        # (uint256 amount, array offset, array length, tokenIn, tokenOut)
        token_in = symbols[words[3][24:]]
        token_out = symbols[words[4][24:]]
        return (
            f"{PANCAKE_V2_POOL_ADDRESS}|getAmountsOut|{token_in}->{token_out}|{int(words[0], 16)}"
        )

    return _decode


@dataclass(frozen=True)
class Scenario:
    """One parity surface: its corpus file, its golden file, and the calldata
    -> golden-key decoder its oracle implies."""

    name: str
    chain_id: int
    block: int
    golden_path: Path
    corpus_path: Path
    called_address: str
    key_decoder: Callable[[str], str]
    # Word carrying the oracle int in the recorded answer (the quoter/router
    # answers that lead with array/tuple metadata need the offset).
    answer_word: int = 0
    # Multiblock goldens tag each key's block; a block scenario's decode
    # compares against its own block's golden subset.
    golden_key_filter: Callable[[str], bool] | None = None


def _chain1_corpus(scenario: str, block: int) -> Path:
    return CORPUS_ROOT / "1" / f"py_oracle_{scenario}_block{block}.json"


def _chain8453_corpus(scenario: str, block: int) -> Path:
    return CORPUS_ROOT / "8453" / f"py_oracle_{scenario}_block{block}.json"


def _balancer_scenarios() -> list[Scenario]:
    golden_dir = GOLDEN_ROOT / "tests/balancer"
    stable_golden = golden_dir / "test_balancer_stable_onchain_parity"
    v2_golden = golden_dir / "test_balancer_v2_onchain_parity"
    return [
        Scenario(
            name="balancer_stable_given_in",
            chain_id=1,
            block=24_407_242,
            golden_path=stable_golden / "test_balancer_v2_stable_query_swap_given_in.json",
            corpus_path=_chain1_corpus("balancer_stable_given_in", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("index"),
        ),
        Scenario(
            name="balancer_stable_given_out",
            chain_id=1,
            block=24_407_242,
            golden_path=stable_golden / "test_balancer_v2_stable_query_swap_given_out.json",
            corpus_path=_chain1_corpus("balancer_stable_given_out", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("index"),
        ),
        Scenario(
            name="balancer_weighted_weth_bal",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_weth_bal_query_swap.json",
            corpus_path=_chain1_corpus("balancer_weighted_weth_bal", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("symbol"),
        ),
        Scenario(
            name="balancer_weighted_usdc_weth",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_usdc_weth_query_swap.json",
            corpus_path=_chain1_corpus("balancer_weighted_usdc_weth", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("symbol"),
        ),
        Scenario(
            name="balancer_weighted_weth_rpl",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_weth_rpl_query_swap.json",
            corpus_path=_chain1_corpus("balancer_weighted_weth_rpl", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("symbol"),
        ),
        Scenario(
            name="balancer_expanded_two_token_given_in",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_expanded_two_token_given_in.json",
            corpus_path=_chain1_corpus("balancer_expanded_two_token_given_in", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("index"),
        ),
        Scenario(
            name="balancer_expanded_two_token_given_out",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_expanded_two_token_given_out.json",
            corpus_path=_chain1_corpus("balancer_expanded_two_token_given_out", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("index"),
        ),
        Scenario(
            name="balancer_expanded_multi_token_given_in",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_expanded_multi_token_given_in.json",
            corpus_path=_chain1_corpus("balancer_expanded_multi_token_given_in", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("index"),
        ),
        Scenario(
            name="balancer_expanded_multi_token_given_out",
            chain_id=1,
            block=24_407_242,
            golden_path=v2_golden / "test_balancer_v2_expanded_multi_token_given_out.json",
            corpus_path=_chain1_corpus("balancer_expanded_multi_token_given_out", 24_407_242),
            called_address=_BALANCER_QUERIES,
            key_decoder=_balancer_golden_key_factory("index"),
        ),
    ]


def _curve_scenarios() -> list[Scenario]:
    golden_dir = GOLDEN_ROOT / "tests/curve/test_curve_onchain_parity"
    tripool_cassette = CORPUS_ROOT / "1" / "curve_tripool_block_24407242.json"
    metapool_cassette = CORPUS_ROOT / "1" / "curve_metapool_rai_3crv_block_25144000.json"
    multiblock_cassette = (
        CORPUS_ROOT / "1" / "curve_metapool_rai_3crv_multiblock_18850030_18850480.json"
    )
    scenarios = [
        Scenario(
            name="curve_tripool_get_dy",
            chain_id=1,
            block=24_407_242,
            golden_path=golden_dir / "test_curve_tripool_get_dy.json",
            corpus_path=_chain1_corpus("curve_tripool_get_dy", 24_407_242),
            called_address=CURVE_TRIPOOL_ADDRESS.lower()[2:],
            key_decoder=_curve_get_dy_key_factory(CURVE_TRIPOOL_ADDRESS, tripool_cassette),
        ),
        Scenario(
            name="curve_tricrypto_get_dy",
            chain_id=1,
            block=24_407_242,
            golden_path=golden_dir / "test_curve_tricrypto_get_dy.json",
            corpus_path=_chain1_corpus("curve_tricrypto_get_dy", 24_407_242),
            called_address=CURVE_TRICRYPTO_ADDRESS.lower()[2:],
            key_decoder=_curve_get_dy_key_factory(
                CURVE_TRICRYPTO_ADDRESS,
                tripool_cassette.parent / "curve_tricrypto_block_24407242.json",
            ),
        ),
        Scenario(
            name="curve_tripool_calc_base_pool",
            chain_id=1,
            block=24_407_242,
            golden_path=golden_dir / "test_curve_tripool_calc_withdraw_and_token_amount.json",
            corpus_path=_chain1_corpus("curve_tripool_calc_base_pool", 24_407_242),
            called_address=CURVE_TRIPOOL_ADDRESS.lower()[2:],
            key_decoder=_curve_calc_key_factory(CURVE_TRIPOOL_ADDRESS, 3),
        ),
        Scenario(
            name="curve_metapool_get_dy",
            chain_id=1,
            block=25_144_000,
            golden_path=golden_dir / "test_curve_metapool_get_dy.json",
            corpus_path=_chain1_corpus("curve_metapool_get_dy", 25_144_000),
            called_address=CURVE_METAPOOL_ADDRESS.lower()[2:],
            key_decoder=_curve_get_dy_key_factory(CURVE_METAPOOL_ADDRESS, metapool_cassette),
        ),
    ]
    multiblock_golden = golden_dir / "test_curve_metapool_multiblock_get_dy.json"
    scenarios.extend(
        Scenario(
            name="curve_metapool_multiblock",
            chain_id=1,
            block=block,
            golden_path=multiblock_golden,
            corpus_path=_chain1_corpus("curve_metapool_multiblock", block),
            called_address=CURVE_METAPOOL_ADDRESS.lower()[2:],
            key_decoder=_curve_get_dy_key_factory(
                CURVE_METAPOOL_ADDRESS, multiblock_cassette, block_tag=block
            ),
            golden_key_filter=lambda key, _block=block: f"|blk{_block}|" in key,
        )
        for block in range(18_850_030, 18_850_481, 30)
    )
    return scenarios


def _base_scenarios() -> list[Scenario]:
    golden_dir = GOLDEN_ROOT / "tests"
    aero_v2_golden = golden_dir / "aerodrome/test_aerodrome_v2_onchain_parity"
    aero_v3_golden = golden_dir / "aerodrome/test_aerodrome_v3_onchain_parity"
    pancake_golden = golden_dir / "pancakeswap/test_pancakeswap_v2_onchain_parity"
    aero_v2_cassette = CORPUS_ROOT / "8453" / "aerodrome_v2_tbtc_weth_volatile_block_46875151.json"
    aero_v2_stable_cassette = (
        CORPUS_ROOT / "8453" / "aerodrome_v2_dola_usdbc_stable_block_46875151.json"
    )
    pancake_cassette = CORPUS_ROOT / "8453" / "pancakeswap_v2_weth_usdbc_block_46875151.json"
    return [
        Scenario(
            name="aerodrome_v2_volatile_get_amount_out",
            chain_id=8453,
            block=46_875_151,
            golden_path=aero_v2_golden / "test_aerodrome_v2_volatile_get_amount_out.json",
            corpus_path=_chain8453_corpus("aerodrome_v2_volatile_get_amount_out", 46_875_151),
            called_address=AERODROME_V2_VOLATILE_POOL_ADDRESS.lower()[2:],
            key_decoder=_aero_v2_key_factory(AERODROME_V2_VOLATILE_POOL_ADDRESS, aero_v2_cassette),
        ),
        Scenario(
            name="aerodrome_v2_stable_get_amount_out",
            chain_id=8453,
            block=46_875_151,
            golden_path=aero_v2_golden / "test_aerodrome_v2_stable_get_amount_out.json",
            corpus_path=_chain8453_corpus("aerodrome_v2_stable_get_amount_out", 46_875_151),
            called_address=AERODROME_V2_STABLE_POOL_ADDRESS.lower()[2:],
            key_decoder=_aero_v2_key_factory(
                AERODROME_V2_STABLE_POOL_ADDRESS, aero_v2_stable_cassette
            ),
        ),
        Scenario(
            name="aerodrome_v3_quote",
            chain_id=8453,
            block=46_875_151,
            golden_path=aero_v3_golden / "test_aerodrome_v3_cbeth_weth_quote.json",
            corpus_path=_chain8453_corpus("aerodrome_v3_quote", 46_875_151),
            called_address=_AERODROME_V3_QUOTER,
            key_decoder=_aero_v3_key_factory(),
        ),
        Scenario(
            name="pancakeswap_v2_router_get_amounts_out",
            chain_id=8453,
            block=46_875_151,
            golden_path=pancake_golden / "test_pancakeswap_v2_router_get_amounts_out.json",
            corpus_path=_chain8453_corpus("pancakeswap_v2_router_get_amounts_out", 46_875_151),
            called_address=_PANCAKE_V2_ROUTER,
            key_decoder=_pancake_key_factory(pancake_cassette),
            answer_word=3,  # uint256[] words: offset, length, amounts[0], amounts[-1]
        ),
    ]


SCENARIOS = (
    Scenario(
        name="uniswap_v3_quoter",
        chain_id=1,
        block=24_407_242,
        golden_path=(
            GOLDEN_ROOT
            / "tests/uniswap/v3/test_uniswap_v3_onchain_parity"
            / "test_cached_calculations_v3_wbtc_weth.json"
        ),
        corpus_path=_chain1_corpus("uniswap_v3_quoter", 24_407_242),
        called_address=_V3_QUOTER,
        key_decoder=_v3_golden_key,
    ),
    Scenario(
        name="uniswap_v4_quoter",
        chain_id=1,
        block=24_407_242,
        golden_path=(
            GOLDEN_ROOT
            / "tests/uniswap/v4/test_uniswap_v4_onchain_parity"
            / "test_cached_calculations_v4_eth_usdc.json"
        ),
        corpus_path=_chain1_corpus("uniswap_v4_quoter", 24_407_242),
        called_address=_V4_QUOTER,
        key_decoder=_v4_golden_key,
    ),
    Scenario(
        name="camelot_v2_get_amount_out",
        chain_id=42161,
        block=477_785_000,
        golden_path=(
            GOLDEN_ROOT
            / "tests/uniswap/v2/test_camelot_v2_onchain_parity"
            / "test_create_camelot_v2_pool.json"
        ),
        corpus_path=CORPUS_ROOT
        / "42161"
        / "py_oracle_camelot_v2_get_amount_out_block477785000.json",
        called_address=_CAMELOT_POOL,
        key_decoder=_camelot_golden_key,
    ),
    *_balancer_scenarios(),
    *_curve_scenarios(),
    *_base_scenarios(),
)

# One representative scenario per capture family for the value-side probe:
# a mutated answer must break the decode agreement in every corpus family.
_FAMILY_PROBE_SCENARIOS = (
    SCENARIOS[0],  # uniswap_v3_quoter
    next(s for s in SCENARIOS if s.name == "balancer_stable_given_in"),
    next(s for s in SCENARIOS if s.name == "curve_tripool_get_dy"),
    next(s for s in SCENARIOS if s.name == "aerodrome_v2_volatile_get_amount_out"),
    next(s for s in SCENARIOS if s.name == "pancakeswap_v2_router_get_amounts_out"),
)

# The corpora that carry a recorded revert: the revert-side probe's targets.
_REVERT_PROBE_SCENARIOS = tuple(
    s
    for s in (SCENARIOS[1],)  # uniswap_v4_quoter
)


def _refuse_connection(*_args: object, **_kwargs: object) -> None:
    raise AssertionError(_OFFLINE_DIAL_MSG)


@pytest.fixture(autouse=True)
def _replay_makes_no_network_calls(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """Arm a hard dial-block: this module is replay-only, so any connection
    attempt means the offline contract broke."""
    monkeypatch.setattr(socket, "create_connection", _refuse_connection)
    monkeypatch.setattr(socket, "getaddrinfo", _refuse_connection)
    monkeypatch.setattr(socket.socket, "connect", _refuse_connection)
    monkeypatch.setattr(socket.socket, "connect_ex", _refuse_connection)


def _decoded_amount(result_hex: str, scenario: Scenario) -> int:
    """The oracle int in a recorded answer: a bare uint256, or the word the
    scenario's answer shape names (the V4 quoter's leading tuple member, the
    router's post-array-length element)."""
    body = result_hex[scenario.answer_word * 64 :]
    return int(body[:64], 16)


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", SCENARIOS, ids=lambda s: s.name)
def test_corpus_decodes_to_the_parity_golden(scenario: Scenario) -> None:
    """Every corpus answer decodes to the exact golden int (or revert).

    Set-equality closes the drift a one-way lookup cannot see: a shrunken
    corpus or a stale golden must fail here, not silently pass."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)
    golden = json.loads(scenario.golden_path.read_text())["entries"]
    if scenario.golden_key_filter is not None:
        golden = {key: entry for key, entry in golden.items() if scenario.golden_key_filter(key)}
    recorded = json.loads(scenario.corpus_path.read_text())
    assert recorded["chain_id"] == scenario.chain_id
    assert recorded["block_number"] == scenario.block

    decoded: dict[str, str | None] = {}
    for call_key, result_hex in recorded["calls"].items():
        to, data_hex = call_key.split(":0x")
        assert to == f"0x{scenario.called_address}"
        decoded[scenario.key_decoder(data_hex)] = result_hex

    assert set(decoded) == set(golden), (
        f"stale={sorted(set(decoded) - set(golden))} missing={sorted(set(golden) - set(decoded))}"
    )
    for key, result_hex in decoded.items():
        entry = golden[key]
        if result_hex is None:
            assert entry.get("reverted") is True, f"{key}: recorded revert, golden holds a value"
        else:
            assert entry.get("reverted") is not True, f"{key}: golden revert, corpus holds a value"
            assert _decoded_amount(result_hex, scenario) == entry["value"], key


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", SCENARIOS, ids=lambda s: s.name)
def test_offline_provider_replays_the_recorded_calls(scenario: Scenario) -> None:
    """The recorded answers replay through the real OfflineProvider runtime.

    Loads the corpus via ``from_json_file`` and drives every recorded call
    through the in-memory transport: values return the exact recorded bytes,
    recorded reverts raise :class:`ContractLogicError` — the same surface a
    live provider presents."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)
    recorded = json.loads(scenario.corpus_path.read_text())
    provider = OfflineProvider.from_json_file(scenario.corpus_path)
    assert provider.chain_id == scenario.chain_id
    assert provider.block_number == scenario.block

    for call_key, result_hex in recorded["calls"].items():
        to, data_hex = call_key.split(":0x")
        calldata = bytes.fromhex(data_hex)
        if result_hex is None:
            with pytest.raises(ContractLogicError):
                provider.call(to, calldata, scenario.block)
        else:
            assert provider.call(to, calldata, scenario.block) == bytes.fromhex(result_hex)


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", SCENARIOS, ids=lambda s: s.name)
def test_corpus_shape_matches_the_offline_provider_wire(scenario: Scenario) -> None:
    """The corpus file keeps the per-block OfflineProvider wire shape.

    Exactly the five established keys; call keys are ``0x<to>:0x<data>`` with
    lowercase hex and no ``0x`` on the recorded results; the code map holds
    the called contract(s)."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)
    recorded = json.loads(scenario.corpus_path.read_text())
    assert set(recorded) == {"chain_id", "block_number", "timestamp", "calls", "code"}
    assert isinstance(recorded["timestamp"], int)
    assert recorded["timestamp"] > 0
    call_targets = {key.split(":")[0] for key in recorded["calls"]}
    assert f"0x{scenario.called_address}" in call_targets
    for key, value in recorded["calls"].items():
        assert _CALL_KEY_RE.fullmatch(key), key
        assert value is None or _HEX_RE.fullmatch(value), key
    assert recorded["code"], "the called contract's runtime code should be recorded"
    for addr, code_hex in recorded["code"].items():
        assert _CODE_KEY_RE.fullmatch(addr), addr
        assert _HEX_RE.fullmatch(code_hex), addr


@pytest.mark.onchain_oracle
def test_same_block_corpus_files_agree_on_the_block_timestamp() -> None:
    """Corpus files pinned to one (chain, block) record one timestamp."""
    stamps: dict[tuple[int, int], set[int]] = {}
    for scenario in SCENARIOS:
        if not scenario.corpus_path.exists():
            continue
        recorded = json.loads(scenario.corpus_path.read_text())
        stamps.setdefault((scenario.chain_id, scenario.block), set()).add(recorded["timestamp"])
    ethereum_pin = stamps.get((1, 24_407_242), set())
    assert ethereum_pin, "no ethereum corpus recorded at the parity pin"
    for (chain_id, block), seen in stamps.items():
        assert len(seen) == 1, f"chain {chain_id} block {block}: corpus files disagree: {seen}"


def _mutated_copy(
    scenario: Scenario,
    tmp_path: Path,
    mutate: Callable[[dict], str],
) -> tuple[Path, str]:
    recorded = json.loads(scenario.corpus_path.read_text())
    key = mutate(recorded)
    path = tmp_path / f"mutated_{scenario.name}.json"
    path.write_text(json.dumps(recorded))
    return path, key


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", _FAMILY_PROBE_SCENARIOS, ids=lambda s: s.name)
def test_seeded_answer_divergence_breaks_the_golden_agreement(
    scenario: Scenario, tmp_path: Path
) -> None:
    """Negative probe (value side): one flipped answer digit must break the
    decode agreement — a comparison that cannot fail is not a test."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)

    def _flip_one_answer(recorded: dict) -> str:
        key = next(k for k, v in recorded["calls"].items() if v is not None)
        original = recorded["calls"][key]
        flipped = original[:-1] + ("0" if original[-1] != "0" else "1")
        assert flipped != original
        recorded["calls"][key] = flipped
        return key

    path, key = _mutated_copy(scenario, tmp_path, _flip_one_answer)
    provider = OfflineProvider.from_json_file(path)
    to, data_hex = key.split(":0x")
    answer = provider.call(to, bytes.fromhex(data_hex), scenario.block)
    golden = json.loads(scenario.golden_path.read_text())["entries"]
    golden_key = scenario.key_decoder(data_hex)
    assert _decoded_amount(answer.hex(), scenario) != golden[golden_key]["value"]


@pytest.mark.onchain_oracle
@pytest.mark.parametrize("scenario", _REVERT_PROBE_SCENARIOS, ids=lambda s: s.name)
def test_seeded_revert_divergence_breaks_the_revert_agreement(
    scenario: Scenario, tmp_path: Path
) -> None:
    """Negative probe (revert side): a recorded revert replaced by a bogus
    value must change the replay behaviour and desynchronize from the golden
    revert entry."""
    if not scenario.corpus_path.exists():
        pytest.skip(_PENDING_CORPUS_REASON)

    def _fill_one_revert(recorded: dict) -> str:
        key = next(k for k, v in recorded["calls"].items() if v is None)
        recorded["calls"][key] = "ab" * 32
        return key

    path, key = _mutated_copy(scenario, tmp_path, _fill_one_revert)
    provider = OfflineProvider.from_json_file(path)
    to, data_hex = key.split(":0x")
    # A replay-time value where record time reverted: the call now returns
    # instead of raising, so any consumer branching on the revert skips on
    # the wrong path — the desync the drift gate exists to catch.
    answer = provider.call(to, bytes.fromhex(data_hex), scenario.block)
    assert answer == bytes.fromhex("ab" * 32)
    golden = json.loads(scenario.golden_path.read_text())["entries"]
    assert golden[scenario.key_decoder(data_hex)].get("reverted") is True
