"""Dual-driver parity test for the prototype structural Pool handle (V2)."""

from __future__ import annotations

import pytest

from degenbot.registry.deployment_records import load_deployments
from degenbot.uniswap.v2_functions import generate_v2_pool_address
from tests.helpers.pool_handle_factory import TOKEN0, TOKEN1, make_v2_handle

#: SushiSwap V2's mainnet factory — a shipped row, so its pairs' addresses are
#: measured and the fixture registers the derivation, never an arbitrary address.
SUSHI_V2_FACTORY = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"


def _sushi_v2_pair_address() -> str:
    """The CREATE2 address SushiSwap V2 derives for the fixture's token pair."""
    [record] = [
        row for row in load_deployments() if row.chain_id == 1 and row.factory == SUSHI_V2_FACTORY
    ]
    assert record.init_hash is not None
    return generate_v2_pool_address(
        record.deployer or record.factory,
        (TOKEN0, TOKEN1),
        record.init_hash,
    )


@pytest.fixture
def v2_pool():
    return make_v2_handle()


def test_structure_and_identity(v2_pool) -> None:
    assert v2_pool.structure() == "reserve_pair"
    identity, variant = v2_pool.identity()
    assert identity == "reserve_pair"
    assert variant == "uniswap_v2"


def test_reserve_pair_view(v2_pool) -> None:
    rp = v2_pool.reserve_pair()

    assert rp.token0.lower() == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    assert rp.token1.lower() == "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    assert rp.reserve0 == 1_000_000_000
    assert rp.reserve1 == 2_000_000_000


def test_calculate_tokens_out(v2_pool) -> None:
    out = v2_pool.calculate_tokens_out(True, 1_000_000)

    assert out is not None
    assert out > 0


def test_dex_name_resolved_from_known_sushiswap_deployment() -> None:
    # SushiSwap V2 mainnet factory (chain 1) → "sushiswap".
    handle = make_v2_handle(
        address=_sushi_v2_pair_address(),
        factory=SUSHI_V2_FACTORY,
    )
    assert handle.dex_name == "sushiswap"


def test_dex_name_unknown_deployment_is_none() -> None:
    # Synthetic factory absent from deployments.json → None (generic, no error).
    handle = make_v2_handle(
        address="0x4444444444444444444444444444444444444444",
        factory="0x1111111111111111111111111111111111111111",
    )
    assert handle.dex_name is None
