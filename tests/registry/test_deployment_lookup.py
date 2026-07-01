"""Rust-side ``(chain_id, factory)`` deployment-identity lookup (Fork A, 7FA5EZ).

The canonical ``deployments.json`` is embedded into the Rust binary via
``include_str!`` + parsed once into a ``OnceLock<HashMap<(u64, Address), Record>>``.
Two thin Python-seam functions surface the CREATE2-critical fields:

- ``init_hash_for(chain_id, factory)`` → the init code hash (or ``None`` when
  the row carries no CREATE2 / the ``(chain, factory)`` is unknown).
- ``deployer_for(chain_id, factory)`` → the *effective* CREATE2 deployer
  (``record.deployer.unwrap_or(record.factory)``), or ``None`` when unknown.

These replace the per-class ``ClassVar`` init-hash / ``_verified_address``
second copies with the JSON single source. The separate-deployer case
(PancakeSwap V3, ``deployer ≠ factory``) is the load-bearing reason a
``(chain, factory)``-keyed lookup is required.
"""

from __future__ import annotations

import pytest

from degenbot.degenbot_rs import deployer_for, init_hash_for

UNISWAP_V2_MAINNET_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
PANCAKESWAP_V3_MAINNET_DEPLOYER = "0x41ff9AA7e16B8B1a8a8dc4f0eFacd93D02d071c9"

UNISWAP_V2_MAINNET_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)
PANCAKESWAP_V3_MAINNET_INIT_HASH = (
    "0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2"
)

# A chain-1 row with no CREATE2 (Balancer weighted-pool factory).
BALANCER_WEIGHTED_MAINNET_FACTORY = "0x8E9aa87E45e92bad84D5F8DD1bff34Fb92637dE9"


class TestInitHashFor:
    """``init_hash_for`` resolves the CREATE2 init hash from the JSON embed."""

    def test_uniswap_v2_mainnet_init_hash(self) -> None:
        assert (
            init_hash_for(1, UNISWAP_V2_MAINNET_FACTORY)
            == UNISWAP_V2_MAINNET_INIT_HASH
        )

    def test_pancakeswap_v3_mainnet_init_hash(self) -> None:
        assert (
            init_hash_for(1, PANCAKESWAP_V3_MAINNET_FACTORY)
            == PANCAKESWAP_V3_MAINNET_INIT_HASH
        )

    def test_lowercase_factory_matches(self) -> None:
        """Address lookup is case-insensitive (Address equality, not string)."""
        assert (
            init_hash_for(1, UNISWAP_V2_MAINNET_FACTORY.lower())
            == UNISWAP_V2_MAINNET_INIT_HASH
        )

    def test_unknown_chain_factory_returns_none(self) -> None:
        assert init_hash_for(999_999, UNISWAP_V2_MAINNET_FACTORY) is None

    def test_unknown_factory_returns_none(self) -> None:
        assert init_hash_for(1, "0x" + "00" * 20) is None

    def test_no_create2_row_returns_none(self) -> None:
        """Balancer/Aerodrome rows carry ``init_hash=null`` → None."""
        assert init_hash_for(1, BALANCER_WEIGHTED_MAINNET_FACTORY) is None


class TestDeployerFor:
    """``deployer_for`` returns the *effective* CREATE2 deployer."""

    def test_uniswap_v2_mainnet_deployer_defaults_to_factory(self) -> None:
        """``deployer=null`` in JSON → effective deployer is the factory."""
        assert deployer_for(1, UNISWAP_V2_MAINNET_FACTORY) == UNISWAP_V2_MAINNET_FACTORY

    def test_pancakeswap_v3_mainnet_separate_deployer(self) -> None:
        """PancakeSwap V3 uses a separate deployer (the load-bearing case)."""
        assert deployer_for(1, PANCAKESWAP_V3_MAINNET_FACTORY) == PANCAKESWAP_V3_MAINNET_DEPLOYER

    def test_unknown_returns_none(self) -> None:
        assert deployer_for(999_999, "0x" + "00" * 20) is None