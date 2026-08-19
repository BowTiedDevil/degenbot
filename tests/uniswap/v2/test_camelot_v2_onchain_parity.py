"""Camelot V2 on-chain parity — golden record/replay (T3 tracer bullet).

First end-to-end proof of the L2 ``GoldenOracle`` harness
(``docs/architecture/golden-onchain-parity.md``). The Camelot WETH/USDC volatile
pool's ``getAmountOut`` is recorded once into a golden file
(``tests/golden/data/...``), then asserted on replay with **no RPC and no
secrets** — the pool is built I/O-free (ADR-005) from the reserves recorded at
the pinned block, so neither mode needs a fork for *construction*.

- **Replay mode** (default, CI): reads the recorded int; the deferred
  ``contract=`` callable is never invoked, so the Anvil fork is never created.
  Fully offline.
- **Record mode** (``--golden-mode=record``): the deferred callable spins an
  Anvil fork of Arbitrum at the pinned block and calls the pool contract's
  ``getAmountOut``; writes the golden file. The test's own ``assert`` still
  runs, so a record run is also a live parity gate.

Pinned to Arbitrum block ``CAMELOT_PARITY_BLOCK`` (well inside
``arb1.arbitrum.io/rpc``'s keyless archive window — the publicnode RPC blocks
deep-archive ``eth_call``; arb1.arbitrum.io serves it, so the golden stays
re-recordable).
"""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot
from degenbot._ffi.dex_identity import dex_identity
from degenbot.camelot.abi import CAMELOT_POOL_ABI
from degenbot.checksum_cache import get_checksum_address
from degenbot.fork import AnvilFork
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.w3_contract import make_contract

# Pinned well inside arb1.arbitrum.io/rpc's keyless archive window.
CAMELOT_PARITY_BLOCK = 477_785_000
CAMELOT_WETH_USDC_LP_ADDRESS = get_checksum_address("0x84652bb2539513BAf36e225c930Fdd8eaa63CE27")
ARBITRUM_RPC_URI = "https://arb1.arbitrum.io/rpc"

# Token addresses at Arbitrum chain ID 42161.
_WETH_ADDRESS = "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
_USDC_ADDRESS = "0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8"

# Reserves recorded at CAMELOT_PARITY_BLOCK (captured by the live build in T2).
# These make the replay build fully I/O-free; a record-mode run does NOT refresh
# them — if the pool's reserves drift at the pinned block, the I/O-free build
# itself is wrong (the block is fixed, so reserves are fixed). Refresh only if
# the pinned block itself changes (then re-record the golden too).
_RESERVES_WETH = 41_955_211_741_551_457_825
_RESERVES_USDC = 66_259_753_282

# One oracle key: 1000 USDC (token1) in → WETH out, via Camelot getAmountOut.
_AMOUNT_IN_USDC = 1000 * 10**6
_ORACLE_KEY = f"{CAMELOT_WETH_USDC_LP_ADDRESS}|getAmountOut|USDC->WETH|{_AMOUNT_IN_USDC}"

_PYBOT = Bot()


def _build_camelot_weth_usdc_io_free() -> object:
    """Build the Camelot WETH/USDC pool I/O-free from the recorded reserves."""
    weth = make_erc20(
        _PYBOT,
        _WETH_ADDRESS,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=42161,
    )
    usdc = make_erc20(
        _PYBOT,
        _USDC_ADDRESS,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
        chain_id=42161,
    )
    return make_v2_pool(
        CAMELOT_WETH_USDC_LP_ADDRESS,
        token0=weth,
        token1=usdc,
        dex=dex_identity("camelot-v2-volatile"),
        reserves_token0=_RESERVES_WETH,
        reserves_token1=_RESERVES_USDC,
        chain_id=42161,
        state_block=CAMELOT_PARITY_BLOCK,
    )


def _record_get_amount_out() -> int:
    """Record-mode oracle: spin a pinned Arbitrum fork + call getAmountOut.

    Replay never invokes this (``golden.check`` returns the recorded int
    without calling ``contract=``), so the Anvil fork is only ever created
    under ``--golden-mode=record``.
    """
    fork = AnvilFork(
        fork_url=ARBITRUM_RPC_URI,
        fork_block=CAMELOT_PARITY_BLOCK,
        storage_caching=True,
        anvil_opts=["--accounts=0"],
    )
    try:
        w3_contract = make_contract(fork.http_url, CAMELOT_WETH_USDC_LP_ADDRESS, CAMELOT_POOL_ABI)
        return w3_contract.functions.getAmountOut(
            amountIn=_AMOUNT_IN_USDC,
            tokenIn=_USDC_ADDRESS,
        ).call()
    finally:
        fork.close()


@pytest.mark.onchain_oracle
def test_create_camelot_v2_pool(golden_factory) -> None:
    """Camelot volatile pool: local calc == golden(= on-chain getAmountOut).

    The pool is built I/O-free (no RPC in either mode). The on-chain truth is
    the pool contract's ``getAmountOut`` for 1000 USDC → WETH at the pinned
    block, recorded into the golden file; replay reads it with no fork.
    """
    lp = _build_camelot_weth_usdc_io_free()
    token_in = lp.token1  # USDC

    golden = golden_factory(chain_id=42161, block_number=CAMELOT_PARITY_BLOCK)
    oracle = golden.check(
        _ORACLE_KEY,
        contract=_record_get_amount_out,
    )
    if oracle.reverted:
        pytest.skip(f"on-chain getAmountOut reverted at record time: {oracle.exception_type}")

    calc_amount_out = lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=_AMOUNT_IN_USDC,
    )
    assert calc_amount_out == oracle.value
