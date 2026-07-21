"""Rust-builder CREATE2 verification at registration (Fork A, JC6OFG).

The `register_v2_pool` / `register_v3_pool` PyBot seams now recompute the
CREATE2 address from the JSON-sourced deployer + init hash and reject a
mismatch before registering. This is the load-bearing correctness guard:

- the *right* deployer per ``(chain, factory)`` — covering PancakeSwap V3's
  separate deployer (``0x41ff9…``, distinct from the factory);
- a wrong pool address is rejected with a clear ``ValueError``;
- ad-hoc / non-JSON ``(chain, factory)`` registrations still work
  (verification skipped — preserves the manual path).

Addresses are cross-checked against the Python ``generate_v2/v3_pool_address``
reference + on-chain pools.
"""

from __future__ import annotations

import pytest

from degenbot.bot import PyBot

# Cross-checked vectors (Python generate_v2/v3_pool_address + on-chain).
UNISWAP_V2_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNISWAP_V3_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
PANCAKESWAP_V3_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"

WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
DAI = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
USDC = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"

V2_DAI_WETH_ADDR = "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"
V3_UNI_USDC_WETH_500_ADDR = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"
V3_PCS_USDC_WETH_500_SEPARATE_ADDR = "0x1ac1A8FEaAEa1900C4166dEeed0C11cC10669D36"
# The address PancakeSwap V3 *would* compute with the factory as deployer
# (wrong — proves the separate-deployer distinction is enforced).
V3_PCS_USDC_WETH_500_FACTORY_ADDR_WRONG = "0xc1CaD0F6b1Cc9124D71DE161a7F133da6aF93c0D"


class TestV2RegistrationVerify:
    """``register_v2_pool`` verifies the address against JSON CREATE2."""

    def test_uniswap_v2_mainnet_correct_address_registers(self) -> None:
        bot = PyBot(chain_id=1)
        pool_id = bot.register_v2_pool(
            address=V2_DAI_WETH_ADDR,
            token0=WETH,
            token1=DAI,
            reserve0=1,
            reserve1=2,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=UNISWAP_V2_FACTORY,
        )
        assert pool_id == 1

    def test_uniswap_v2_mainnet_wrong_address_rejected(self) -> None:
        bot = PyBot(chain_id=1)
        wrong = "0x" + "ab" * 20
        with pytest.raises(ValueError, match="CREATE2"):
            bot.register_v2_pool(
                address=wrong,
                token0=WETH,
                token1=DAI,
                reserve0=1,
                reserve1=2,
                gamma_numer0=997,
                fee_denom0=1000,
                gamma_numer1=997,
                fee_denom1=1000,
                factory=UNISWAP_V2_FACTORY,
            )

    def test_ad_hoc_factory_skips_verification(self) -> None:
        """A (chain, factory) not in the shipped JSON → verification skipped."""
        bot = PyBot(chain_id=1)
        # chain 1 mainnet but an unrecognized factory + a fabricated address.
        ad_hoc_factory = "0x" + "12" * 20
        pool_id = bot.register_v2_pool(
            address="0x" + "cd" * 20,
            token0=WETH,
            token1=DAI,
            reserve0=1,
            reserve1=2,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=ad_hoc_factory,
        )
        assert pool_id == 1


class TestV3RegistrationVerify:
    """``register_v3_pool`` verifies the address against JSON CREATE2."""

    def test_uniswap_v3_mainnet_correct_address_registers(self) -> None:
        bot = PyBot(chain_id=1)
        pool_id = bot.register_v3_pool(
            address=V3_UNI_USDC_WETH_500_ADDR,
            token0=USDC,
            token1=WETH,
            fee=500,
            tick_spacing=10,
            factory=UNISWAP_V3_FACTORY,
            sqrt_price_x96=1 << 96,
            liquidity=0,
            tick=0,
        )
        assert pool_id == 1

    def test_pancakeswap_v3_separate_deployer_correct_address_registers(self) -> None:
        """The load-bearing case: the separate-deployer address verifies."""
        bot = PyBot(chain_id=1)
        pool_id = bot.register_v3_pool(
            address=V3_PCS_USDC_WETH_500_SEPARATE_ADDR,
            token0=USDC,
            token1=WETH,
            fee=500,
            tick_spacing=10,
            factory=PANCAKESWAP_V3_FACTORY,
            sqrt_price_x96=1 << 96,
            liquidity=0,
            tick=0,
        )
        assert pool_id == 1

    def test_pancakeswap_v3_factory_as_deployer_address_rejected(self) -> None:
        """Computing with the factory as deployer yields a wrong address.

        The JSON row pins the separate deployer, so the factory-derived address
        must FAIL verification — proving the per-(chain, factory) deployer is
        enforced, not the variant's canonical mainnet factory.
        """
        bot = PyBot(chain_id=1)
        with pytest.raises(ValueError, match="CREATE2"):
            bot.register_v3_pool(
                address=V3_PCS_USDC_WETH_500_FACTORY_ADDR_WRONG,
                token0=USDC,
                token1=WETH,
                fee=500,
                tick_spacing=10,
                factory=PANCAKESWAP_V3_FACTORY,
                sqrt_price_x96=1 << 96,
                liquidity=0,
                tick=0,
            )

    def test_uniswap_v3_wrong_address_rejected_with_chain_context(self) -> None:
        bot = PyBot(chain_id=1)
        wrong = "0x" + "ef" * 20
        with pytest.raises(ValueError, match="chain 1"):
            bot.register_v3_pool(
                address=wrong,
                token0=USDC,
                token1=WETH,
                fee=500,
                tick_spacing=10,
                factory=UNISWAP_V3_FACTORY,
                sqrt_price_x96=1 << 96,
                liquidity=0,
                tick=0,
            )

    def test_ad_hoc_factory_skips_verification(self) -> None:
        bot = PyBot(chain_id=1)
        ad_hoc_factory = "0x" + "34" * 20
        pool_id = bot.register_v3_pool(
            address="0x" + "56" * 20,
            token0=USDC,
            token1=WETH,
            fee=500,
            tick_spacing=10,
            factory=ad_hoc_factory,
            sqrt_price_x96=1 << 96,
            liquidity=0,
            tick=0,
        )
        assert pool_id == 1


class TestAdHocChainSkipsVerification:
    """chain_id=0 (the bare fixture default) is not in the JSON → skipped."""

    def test_chain_zero_does_not_verify(self) -> None:
        bot = PyBot()  # chain_id=0 default
        # A fabricated address that would fail on any known chain, but chain 0
        # isn't a shipped deployment → verification skipped → registers fine.
        pool_id = bot.register_v2_pool(
            address="0x" + "00" * 20,
            token0=WETH,
            token1=DAI,
            reserve0=1,
            reserve1=2,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=UNISWAP_V2_FACTORY,
        )
        assert pool_id == 1
