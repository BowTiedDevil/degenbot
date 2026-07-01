"""Rust handle exposes verified V3 identity (Fork A, P62DKO).

`register_v3_pool` now resolves the JSON-sourced deployer + init_hash at
registration and stores them on the pool identity. The `PyLiquidityPool`
handle exposes them via `init_hash` / `deployer` getters -- the source the
V3 companion reads instead of the retired `UNISWAP_V3_MAINNET_POOL_INIT_HASH`
ClassVar.

PancakeSwap V3 (separate deployer) is the load-bearing case: the handle's
`deployer` must be the separate deployer, not the factory.
"""

from __future__ import annotations

from degenbot.degenbot_rs import PyBot

UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
PANCAKESWAP_V3_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
PANCAKESWAP_V3_DEPLOYER = "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"

UNISWAP_V3_INIT_HASH = "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
PANCAKESWAP_V3_INIT_HASH = "0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2"

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"

V3_UNI_USDC_WETH_500 = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"
V3_PCS_USDC_WETH_500_SEPARATE = "0x1ac1A8FEaAEa1900C4166dEeed0C11cC10669D36"


def _register_v3(bot: PyBot, address: str, factory: str) -> int:
    return bot.register_v3_pool(
        address=address,
        token0=USDC,
        token1=WETH,
        fee=500,
        tick_spacing=10,
        factory=factory,
        sqrt_price_x96=1,
        liquidity=0,
        tick=0,
    )


def _py_pool(bot: PyBot, pool_id: int):
    pool = bot.get_pool(pool_id)
    assert pool is not None, "pool must be registered to build a handle"
    return pool


class TestV3HandleIdentity:
    """`PyLiquidityPool` exposes the verified V3 deployer + init_hash."""

    def test_uniswap_v3_init_hash_off_handle(self) -> None:
        bot = PyBot(chain_id=1)
        pid = _register_v3(bot, V3_UNI_USDC_WETH_500, UNISWAP_V3_FACTORY)
        pool = _py_pool(bot, pid)
        assert pool.init_hash == UNISWAP_V3_INIT_HASH
        # Uniswap V3 has null deployer in JSON -> effective = factory.
        assert pool.deployer == UNISWAP_V3_FACTORY

    def test_pancakeswap_v3_separate_deployer_off_handle(self) -> None:
        bot = PyBot(chain_id=1)
        pid = _register_v3(bot, V3_PCS_USDC_WETH_500_SEPARATE, PANCAKESWAP_V3_FACTORY)
        pool = _py_pool(bot, pid)
        # The load-bearing case: separate deployer, NOT the factory.
        assert pool.deployer == PANCAKESWAP_V3_DEPLOYER
        assert pool.deployer != PANCAKESWAP_V3_FACTORY
        assert pool.init_hash == PANCAKESWAP_V3_INIT_HASH

    def test_non_json_pool_falls_back_to_mainnet_init_hash(self) -> None:
        """A non-JSON V3 factory defaults to the mainnet fallback init_hash."""
        bot = PyBot(chain_id=1)
        ad_hoc_factory = "0x" + "77" * 20
        pid = _register_v3(bot, "0x" + "88" * 20, ad_hoc_factory)
        pool = _py_pool(bot, pid)
        assert pool.init_hash == UNISWAP_V3_INIT_HASH
        # Non-JSON -> deployer defaults to factory.
        assert pool.deployer == ad_hoc_factory
