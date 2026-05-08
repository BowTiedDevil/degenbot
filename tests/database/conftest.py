"""
Fixtures for database tests.

Provides an in-memory SQLite database seeded with minimal test data,
wrapped in a ``DatabaseSessionManager`` for each test.
"""

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import scoped_session, sessionmaker

from degenbot.database.models import Base
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    AerodromeV2PoolTable,
    PoolManagerTable,
    UniswapV4PoolTable,
)
from degenbot.database.session_manager import DatabaseSessionManager


def _seed_test_data(session: scoped_session) -> None:
    """Insert minimal records matching the addresses used in test_pools.py."""
    with session() as s:
        # ── Tokens ──────────────────────────────────────────────────────
        # Aerodrome V2 pool token0 (test_get_pool_from_base_table)
        token_a = Erc20TokenTable(
            id=1,
            chain=8453,
            address="0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b",
            name="Token A",
            symbol="TKA",
            decimals=18,
        )
        # Aerodrome V2 pool token1
        token_b = Erc20TokenTable(
            id=2,
            chain=8453,
            address="0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA",
            name="Token B",
            symbol="TKB",
            decimals=18,
        )
        # WETH on Base (used by token-filter and paired-token queries)
        weth_base = Erc20TokenTable(
            id=3,
            chain=8453,
            address="0x4200000000000000000000000000000000000006",
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )
        # USDC on Base (V4 pool currency1)
        usdc_base = Erc20TokenTable(
            id=4,
            chain=8453,
            address="0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        # Token paired with WETH ≥2 times (for min_pairs test)
        token_c = Erc20TokenTable(
            id=5,
            chain=8453,
            address="0x000000000000000000000000000000000000DEAD",
            name="Token C",
            symbol="TKC",
            decimals=18,
        )
        # Null currency for V4 pool currency0
        null_token = Erc20TokenTable(
            id=6,
            chain=8453,
            address="0x0000000000000000000000000000000000000000",
            name="Null Currency",
            symbol="NULL",
            decimals=18,
        )
        s.add_all([weth_base, token_a, token_b, usdc_base, token_c, null_token])
        s.flush()

        # ── Exchanges ─────────────────────────────────────────────────
        aero_exchange = ExchangeTable(
            id=1,
            chain_id=8453,
            name="Aerodrome V2",
            active=True,
            factory="0x7A3A55c8cA6b6E7C2eEc74884f7379fF93aD1b7d",
            deployer="0x7A3A55c8cA6b6E7C2eEc74884f7379fF93aD1b7d",
        )
        v4_exchange = ExchangeTable(
            id=2,
            chain_id=8453,
            name="Uniswap V4",
            active=True,
            factory="0x0000000000000000000000000000000000000000",
            deployer=None,
        )
        s.add_all([aero_exchange, v4_exchange])
        s.flush()

        # ── Pools ──────────────────────────────────────────────────────
        # The pool test_get_pool_from_base_table queries
        aero_pool = AerodromeV2PoolTable(
            id=1,
            address="0x723AEf6543aecE026a15662Be4D3fb3424D502A9",
            chain=8453,
            kind="aerodrome_v2",
            token0_id=token_a.id,
            token1_id=token_b.id,
            exchange_id=aero_exchange.id,
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
            stable=False,
        )
        # WETH pairs for token filter + paired-token tests
        weth_a_pool = AerodromeV2PoolTable(
            id=2,
            address="0x000000000000000000000000000000000000AAAA",
            chain=8453,
            kind="aerodrome_v2",
            token0_id=weth_base.id,
            token1_id=token_a.id,
            exchange_id=aero_exchange.id,
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
            stable=False,
        )
        # Token C needs ≥2 WETH pairs for the min_pairs=2 query
        weth_c_pool_1 = AerodromeV2PoolTable(
            id=3,
            address="0x000000000000000000000000000000000000BBBB",
            chain=8453,
            kind="aerodrome_v2",
            token0_id=weth_base.id,
            token1_id=token_c.id,
            exchange_id=aero_exchange.id,
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
            stable=False,
        )
        weth_c_pool_2 = AerodromeV2PoolTable(
            id=4,
            address="0x000000000000000000000000000000000000CCCC",
            chain=8453,
            kind="aerodrome_v2",
            token0_id=token_c.id,
            token1_id=weth_base.id,
            exchange_id=aero_exchange.id,
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
            stable=True,
        )
        s.add_all([aero_pool, weth_a_pool, weth_c_pool_1, weth_c_pool_2])
        s.flush()

        # ── V4 Pool Manager + Pool ─────────────────────────────────────
        v4_manager = PoolManagerTable(
            id=1,
            address="0x498581fF718922c3f8e6A244956aF099B2652b2b",
            chain=8453,
            kind="uniswap_v4",
            state_view=None,
            exchange_id=v4_exchange.id,
        )
        s.add(v4_manager)
        s.flush()

        v4_pool = UniswapV4PoolTable(
            id=1,
            kind="uniswap_v4",
            manager_id=v4_manager.id,
            pool_hash="0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a",
            hooks="0x0000000000000000000000000000000000000000",
            currency0_id=null_token.id,
            currency1_id=usdc_base.id,
            fee_currency0=0,
            fee_currency1=0,
            fee_denominator=1000000,
            tick_spacing=60,
            liquidity_update_block=None,
            liquidity_update_log_index=None,
        )
        s.add(v4_pool)
        s.commit()


@pytest.fixture
def test_db():
    """
    Provide an in-memory SQLite database seeded with test data,
    wrapped in a ``DatabaseSessionManager``.
    """
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    test_session = scoped_session(sessionmaker(bind=engine))

    _seed_test_data(test_session)

    db_mgr = DatabaseSessionManager(test_session)
    yield db_mgr

    test_session.remove()
    engine.dispose()
