"""Camelot V2 on-chain parity — revived under the post-collapse pool model.

The original Camelot parity tests lived in ``test_uniswap_v2_liquidity_pool.py``,
which is module-level skipped pending a rewrite off the deleted
``CamelotLiquidityPool`` subclass (see
``docs/migration-guides/dex-subclass-collapse.md``). Camelot now builds under
the canonical ``LiquidityPool`` via ``Bot.build_pool`` with
``dex.variant == "camelot-v2-volatile"`` (volatile) / ``"camelot-v2-stable"``
(stable), so the subclass import that forced the skip is gone.

This module revives the volatile WETH/USDC ``getAmountOut`` parity test against
a **pinned** Arbitrum fork. The pin is mandatory: it makes the on-chain truth
deterministic so the golden conversion (T3) can record + replay it. Block
``CAMELOT_PARITY_BLOCK`` is chosen well inside ``arb1.arbitrum.io``'s archive
window (the keyless public RPC blocks deep-archive ``eth_call`` only above ~tip),
so it remains re-recordable later when the math changes.

Live only — runs against an Anvil fork of Arbitrum. Not marked
``onchain_oracle`` yet (T3 adds the golden replay path + that marker).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.camelot.abi import CAMELOT_POOL_ABI
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider import ProviderAdapter
from tests.helpers.bot_factory import make_bot_with_provider

if TYPE_CHECKING:
    from collections.abc import Generator

# Pinned well inside arb1.arbitrum.io/rpc's keyless archive window (deep
# eth_call works here; tip-only RPCs cannot be used — they age out within
# minutes and break re-recordability).
CAMELOT_PARITY_BLOCK = 477_785_000

CAMELOT_WETH_USDC_LP_ADDRESS = get_checksum_address("0x84652bb2539513BAf36e225c930Fdd8eaa63CE27")
ARBITRUM_RPC_URI = "https://arb1.arbitrum.io/rpc"


@pytest.fixture
def fork_arbitrum_pinned() -> Generator[AnvilFork, None, None]:
    """Anvil fork of Arbitrum at the pinned parity block."""
    fork = AnvilFork(
        fork_url=ARBITRUM_RPC_URI,
        fork_block=CAMELOT_PARITY_BLOCK,
        storage_caching=True,
        anvil_opts=["--accounts=0"],
    )
    yield fork
    fork.close()


def test_create_camelot_v2_pool(fork_arbitrum_pinned: AnvilFork) -> None:
    """Camelot volatile pool: local calc == on-chain getAmountOut (pinned block).

    Builds the WETH/USDC Camelot pool via ``Bot.build_pool`` → ``LiquidityPool``
    (``dex.variant == "camelot-v2-volatile"``) and asserts the off-chain
    ``calculate_tokens_out_from_tokens_in`` matches the pool contract's
    ``getAmountOut`` at the pinned block.
    """
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_arbitrum_pinned.w3))
    lp = bot.build_pool(CAMELOT_WETH_USDC_LP_ADDRESS)

    token_in = lp.token1  # USDC
    amount_in = 1000 * 10**token_in.decimals  # 1000 USDC

    w3_contract = fork_arbitrum_pinned.w3.eth.contract(
        address=CAMELOT_WETH_USDC_LP_ADDRESS,
        abi=CAMELOT_POOL_ABI,
    )
    on_chain_amount_out = w3_contract.functions.getAmountOut(
        amountIn=amount_in,
        tokenIn=token_in.address,
    ).call()

    calc_amount_out = lp.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )
    assert calc_amount_out == on_chain_amount_out
