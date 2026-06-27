"""Aerodrome V3 on-chain parity — golden record/replay (T7 tracer bullet).

First Base-chain proof of the ``GoldenOracle`` harness
(``docs/architecture/golden-onchain-parity.md``). The Aerodrome V3 cbETH/WETH
(0.01%) pool's ``quoteExactInputSingle`` is recorded once into a golden file
(``tests/golden/data/...``), then asserted on replay with **no RPC and no
secrets**.

Unlike the Camelot V2 tracer (reserves-only), a V3 (concentrated-liquidity)
pool needs tick state to simulate a swap, so the pool is built I/O-free
(ADR-005) from a **tick-state cassette** recorded at the pinned block —
``tests/fixtures/chain_data/8453/aerodrome_v3_cbeth_weth_block_46875151.json``
(scalars ``sqrt_price_x96``/``tick``/``liquidity``/``fee``/``tick_spacing``
plus 116 initialized ticks). Neither mode needs a fork for *construction*.

- **Replay mode** (default, CI): reads the recorded int; the deferred
  ``contract=`` callable is never invoked, so the Anvil fork is never created.
  Fully offline.
- **Record mode** (``--golden-mode=record``): the deferred callable spins an
  Anvil fork of Base at the pinned block and calls the Aerodrome V3 quoter's
  ``quoteExactInputSingle``; writes the golden file. The test's own ``assert``
  still runs, so a record run is also a live parity gate.

Pinned to Base block ``AERODROME_V3_PARITY_BLOCK`` (tip minus 1M), well inside
``https://mainnet.base.org``'s keyless archive window (it serves deep-archive
``eth_call`` incl. simulated nonpayable quoter calls; ``base-rpc.publicnode.com``
prunes history and the keyed ``base.llamarpc.com`` is down). The golden stays
re-recordable.
"""

from __future__ import annotations

import json
import pathlib
from typing import Any

import pytest

from degenbot.aerodrome.pools import AerodromeV3Pool
from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from degenbot.uniswap.v3_libraries.tick_math import MIN_SQRT_RATIO
from tests.aerodrome.test_aerodrome_pools import AERODROME_V3_QUOTER_ABI
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

# Pinned well inside mainnet.base.org's keyless archive window (tip ~47.8M).
AERODROME_V3_PARITY_BLOCK = 46_875_151
BASE_RPC_URI = "https://mainnet.base.org"

AERODROME_V3_CBETH_WETH_POOL_ADDRESS = get_checksum_address(
    "0x47cA96Ea59C13F72745928887f84C9F52C3D7348",
)
AERODROME_V3_FACTORY_ADDRESS = get_checksum_address(
    "0x5e7BB104d84c7CB9B682AaC2F3d509f5F406809A",
)
AERODROME_V3_QUOTER_ADDRESS = get_checksum_address(
    "0x254cF9E1E6e233aa1AC962CB9B05b2cfeAaE15b0",
)
_CBETH_ADDRESS = "0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22"
_WETH_ADDRESS = "0x4200000000000000000000000000000000000006"

_CASSETTE_PATH = (
    pathlib.Path(__file__).resolve().parents[1]
    / "fixtures"
    / "chain_data"
    / "8453"
    / "aerodrome_v3_cbeth_weth_block_46875151.json"
)

# Amount in = int(0.001 * token0 balance at the pinned block). Recorded as a
# constant so replay is fully deterministic (the balance is on-chain state).
_AMOUNT_IN_CBETH = 562_672_230_181_659_840

_ORACLE_KEY = (
    f"{AERODROME_V3_CBETH_WETH_POOL_ADDRESS}|quoteExactInputSingle|cbETH->WETH|{_AMOUNT_IN_CBETH}"
)

_PYBOT = PyBot()


def _load_cassette() -> dict[str, Any]:
    return json.loads(_CASSETTE_PATH.read_bytes())


def _build_aerodrome_v3_cbeth_weth_io_free() -> AerodromeV3Pool:
    """Build the Aerodrome V3 cbETH/WETH pool I/O-free from the tick cassette.

    The cassette holds the scalar state + 116 initialized ticks recorded at
    ``AERODROME_V3_PARITY_BLOCK`` via ``Bot.build_pool``. Both record and
    replay modes build identically — no fork for construction.
    """
    cassette = _load_cassette()
    scalars = cassette["scalars"]
    tick_data = {int(tick): tuple(vals) for tick, vals in cassette["tick_data"].items()}

    cbeth = make_erc20(
        _PYBOT,
        _CBETH_ADDRESS,
        name="Coinbase Wrapped Staked ETH",
        symbol="cbETH",
        decimals=18,
        chain_id=8453,
    )
    weth = make_erc20(
        _PYBOT,
        _WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=8453,
    )
    return make_v3_pool(
        AERODROME_V3_CBETH_WETH_POOL_ADDRESS,
        token0=cbeth,
        token1=weth,
        factory=AERODROME_V3_FACTORY_ADDRESS,
        fee=scalars["fee"],
        tick_spacing=scalars["tick_spacing"],
        sqrt_price_x96=scalars["sqrt_price_x96"],
        tick=scalars["tick"],
        liquidity=scalars["liquidity"],
        chain_id=8453,
        state_block=cassette["block"],
        tick_data=tick_data,
        pool_class=AerodromeV3Pool,
    )


def _record_quote_exact_input_single() -> int:
    """Record-mode oracle: spin a pinned Base fork + call the quoter.

    Replay never invokes this (``golden.check`` returns the recorded int
    without calling ``contract=``), so the Anvil fork is only ever created
    under ``--golden-mode=record``.
    """
    fork = AnvilFork(
        fork_url=BASE_RPC_URI,
        fork_block=AERODROME_V3_PARITY_BLOCK,
        storage_caching=True,
        anvil_opts=["--accounts=0", "--optimism"],
    )
    try:
        quoter = fork.w3.eth.contract(
            address=AERODROME_V3_QUOTER_ADDRESS,
            abi=AERODROME_V3_QUOTER_ABI,
        )
        amount_out, *_ = quoter.functions.quoteExactInputSingle(
            [
                _CBETH_ADDRESS,  # tokenIn (cbETH, token0)
                _WETH_ADDRESS,  # tokenOut (WETH, token1)
                _AMOUNT_IN_CBETH,  # amountIn
                1,  # tickSpacing (0.01% tier)
                MIN_SQRT_RATIO + 1,  # sqrtPriceLimitX96 (full range, zero-for-one)
            ],
        ).call()
        return amount_out
    finally:
        fork.close()


@pytest.mark.base
@pytest.mark.onchain_oracle
def test_aerodrome_v3_cbeth_weth_quote(golden_factory) -> None:
    """Aerodrome V3 cbETH/WETH: local calc == golden(= on-chain quoter).

    Pool built I/O-free from the tick cassette (no RPC in either mode). The
    on-chain truth is the quoter's ``quoteExactInputSingle`` for ~0.056 cbETH
    -> WETH at the pinned block, recorded into the golden file; replay reads
    it with no fork.
    """
    lp = _build_aerodrome_v3_cbeth_weth_io_free()

    golden = golden_factory(chain_id=8453, block_number=AERODROME_V3_PARITY_BLOCK)
    oracle = golden.check(
        _ORACLE_KEY,
        contract=_record_quote_exact_input_single,
    )
    if oracle.reverted:
        pytest.skip(
            f"on-chain quoteExactInputSingle reverted at record time: {oracle.exception_type}",
        )

    calc_amount_out = lp.calculate_tokens_out_from_tokens_in(
        token_in=lp.token0,
        token_in_quantity=_AMOUNT_IN_CBETH,
    )
    assert calc_amount_out == oracle.value
