"""The resolved init hash carries its provenance, and the chain-1 policy.

``resolve_v2_init_hash`` / ``resolve_v3_init_hash`` answer the hash a pool
registration stores. A shipped ``deployments.json`` row measures it: its own
``init_hash``, or — for an EIP-1167 clone row — the init code hash of the clone
bytecode pinned to the row's ``implementation_address`` (the Aerodrome V2/V3
rows, whose pools are minimal-proxy clones). A measured hash is a *record* and is
admitted on every chain.

An *unshipped* ``(chain, factory)`` has no measured hash, only the documented
Uniswap mainnet default the retired Python ``ClassVar`` carried. That default is
admitted on chain 1 (the measured mainnet) and on the chain-0 placeholder the
offline fixtures register under; on any real other chain the resolution REFUSES
rather than answering an unmeasured mainnet hash, because a guessed init hash
derives a wrong CREATE2 address with no signal.

A row carrying neither an ``init_hash`` nor an ``implementation_address`` (the
Balancer rows) has no address-derived identity at all, and is refused
everywhere.
"""

from __future__ import annotations

import pytest

from tests.helpers.erc20_factory import make_erc20
from tests.helpers.seam import Bot, resolve_v2_init_hash, resolve_v3_init_hash
from tests.helpers.v3_pool_factory import make_v3_pool

UNISWAP_V2_MAINNET_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
UNISWAP_V3_MAINNET_FACTORY = "0x1F98431c8aD98523631AE4a59f267346ea31F984"
PANCAKESWAP_V3_MAINNET_FACTORY = "0x0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865"
UNISWAP_V2_BASE_FACTORY = "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6"
UNISWAP_V3_BASE_FACTORY = "0x33128a8fC17869897dcE68Ed026d694621f6FDfD"

UNISWAP_V2_MAINNET_INIT_HASH = "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
UNISWAP_V3_MAINNET_INIT_HASH = "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
PANCAKESWAP_V3_MAINNET_INIT_HASH = (
    "0x6ce8eb472fa82df5469c6ab6d485f17c3ad13c8cd7af59b3d4a8026c5ce0f7e2"
)

# A chain-1 row with neither an init hash nor a clone implementation: its pools
# are not address-derived (Balancer weighted-pool factory).
BALANCER_WEIGHTED_MAINNET_FACTORY = "0x8E9aa87E45e92bad84D5F8DD1bff34Fb92637dE9"
# Base Aerodrome V2 — a shipped EIP-1167 clone row: no `init_hash`, but its
# pools are minimal-proxy clones of `implementation_address`, whose keccak is
# that deployment's measured init code hash.
AERODROME_V2_BASE_FACTORY = "0x420DD381b31aEf6683db6B902084cB0FFECe40Da"
AERODROME_V2_POOL_IMPLEMENTATION = "0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7"
# keccak256 of the EIP-1167 minimal-proxy init code pinned to that
# implementation: 0x3d602d80600a3d3981f3 ++ 0x363d3d373d3d3d363d73 ++
# implementation (20) ++ 0x5af43d82803e903d91602b57fd5bf3.
AERODROME_V2_CLONE_INIT_HASH = "0x6f178972b07752b522a4da1c5b71af6524e8b0bd6027ccb29e5312b0e5bcdc3c"

# A factory no deployments.json row names, on any chain.
UNSHIPPED_FACTORY = "0x1234567890123456789012345678901234567890"
BSC_CHAIN_ID = 56


class TestShippedRowsResolveAsTheRecord:
    """A shipped row's hash is returned unchanged, on every shipped chain."""

    @pytest.mark.parametrize(
        ("chain_id", "factory", "init_hash"),
        [
            (1, UNISWAP_V2_MAINNET_FACTORY, UNISWAP_V2_MAINNET_INIT_HASH),
            (1, UNISWAP_V3_MAINNET_FACTORY, UNISWAP_V3_MAINNET_INIT_HASH),
            (1, PANCAKESWAP_V3_MAINNET_FACTORY, PANCAKESWAP_V3_MAINNET_INIT_HASH),
            # A non-mainnet shipped row resolves as the record it is, not as an
            # assumed mainnet default.
            (8453, UNISWAP_V2_BASE_FACTORY, UNISWAP_V2_MAINNET_INIT_HASH),
            (8453, UNISWAP_V3_BASE_FACTORY, UNISWAP_V3_MAINNET_INIT_HASH),
        ],
    )
    def test_shipped_row_answers_its_record_hash(
        self,
        chain_id: int,
        factory: str,
        init_hash: str,
    ) -> None:
        assert resolve_v2_init_hash(chain_id, factory) == init_hash
        assert resolve_v3_init_hash(chain_id, factory) == init_hash


class TestForeignChainRefusesAnUnmeasuredDefault:
    """Acceptance: never a silent mainnet guess on a foreign chain."""

    def test_unshipped_v2_factory_on_bsc_refuses(self) -> None:
        with pytest.raises(ValueError, match=r"56"):
            resolve_v2_init_hash(BSC_CHAIN_ID, UNSHIPPED_FACTORY)

    def test_unshipped_v3_factory_on_bsc_refuses(self) -> None:
        with pytest.raises(ValueError, match=r"56"):
            resolve_v3_init_hash(BSC_CHAIN_ID, UNSHIPPED_FACTORY)

    def test_a_preset_factory_on_a_chain_it_is_not_shipped_on_refuses(self) -> None:
        """The Uniswap V2 factory address is unshipped on BSC: still a refusal."""

        with pytest.raises(ValueError, match=r"56"):
            resolve_v2_init_hash(BSC_CHAIN_ID, UNISWAP_V2_MAINNET_FACTORY)

    def test_an_unshipped_factory_on_base_refuses(self) -> None:
        with pytest.raises(ValueError, match=r"8453"):
            resolve_v2_init_hash(8453, UNSHIPPED_FACTORY)


class TestAnEip1167CloneRowResolvesItsCloneHash:
    """A clone row IS CREATE2-deployed: its init code is the clone bytecode of
    its ``implementation_address``, so resolution answers a measured record."""

    def test_the_aerodrome_v2_row_resolves_its_clone_init_code_hash(self) -> None:
        assert resolve_v2_init_hash(8453, AERODROME_V2_BASE_FACTORY) == (
            AERODROME_V2_CLONE_INIT_HASH
        )
        assert resolve_v3_init_hash(8453, AERODROME_V2_BASE_FACTORY) == (
            AERODROME_V2_CLONE_INIT_HASH
        )

    def test_the_clone_row_is_admitted_on_its_own_chain(self) -> None:
        """Measured ⇒ admitted on the chain the row names (no refusal)."""

        assert resolve_v2_init_hash(8453, AERODROME_V2_BASE_FACTORY) == (
            AERODROME_V2_CLONE_INIT_HASH
        )
        # The "a measured hash is admitted on every chain" rule is a policy fact
        # pinned by the core test (`an_eip1167_clone_row_resolves_its_clone_init_code_hash`);
        # through the seam the lookup is keyed by (chain, factory), so the same
        # factory address on a chain that does not ship it is still unshipped.
        with pytest.raises(ValueError, match=r"56"):
            resolve_v2_init_hash(BSC_CHAIN_ID, AERODROME_V2_BASE_FACTORY)


class TestABalancerRowHasNoAddressDerivedIdentity:
    """A row with neither an init hash nor a clone implementation is refused
    everywhere — including on chain 1 (there is no hash to complete)."""

    def test_a_balancer_row_refuses_even_on_mainnet(self) -> None:
        with pytest.raises(ValueError, match=r"CREATE2"):
            resolve_v2_init_hash(1, BALANCER_WEIGHTED_MAINNET_FACTORY)


# The offline/ad-hoc placeholder chain: `Bot()` is `PyBot::new(chain_id = 0)`,
# the scope the test fixtures register synthetic factories under. No network
# exists there, so a CREATE2 address can never be misderived against it.
PLACEHOLDER_CHAIN_ID = 0


class TestTheAdmittedChainsKeepTheDocumentedAdHocDefault:
    """Chain 1 (the measured mainnet) and the chain-0 placeholder admit the
    documented default — as an ASSUMED value, never a verified record."""

    @pytest.mark.parametrize("chain_id", [1, PLACEHOLDER_CHAIN_ID])
    def test_unshipped_v2_factory_resolves_the_default(self, chain_id: int) -> None:
        assert resolve_v2_init_hash(chain_id, UNSHIPPED_FACTORY) == UNISWAP_V2_MAINNET_INIT_HASH

    @pytest.mark.parametrize("chain_id", [1, PLACEHOLDER_CHAIN_ID])
    def test_unshipped_v3_factory_resolves_the_default(self, chain_id: int) -> None:
        assert resolve_v3_init_hash(chain_id, UNSHIPPED_FACTORY) == UNISWAP_V3_MAINNET_INIT_HASH

    def test_a_refusal_names_the_chains_that_do_admit(self) -> None:
        """A real network's refusal says where the default belongs."""

        with pytest.raises(ValueError) as excinfo:
            resolve_v2_init_hash(BSC_CHAIN_ID, UNSHIPPED_FACTORY)
        message = str(excinfo.value)
        assert "chain 1" in message, message
        assert "chain-0" in message, message


class TestTheOfflineFixturePathRegisters:
    """The chain-0 fixture path (`tests/helpers/*_pool_factory.py`) registers
    its synthetic pools with the admitted default, as the offline suite needs."""

    def test_a_chain_zero_v3_fixture_pool_registers(self) -> None:
        py_bot = Bot()
        token0 = make_erc20(
            py_bot,
            "0x" + "aa" * 20,
            name="Token A",
            symbol="TKA",
            decimals=18,
            chain_id=PLACEHOLDER_CHAIN_ID,
        )
        token1 = make_erc20(
            py_bot,
            "0x" + "bb" * 20,
            name="Token B",
            symbol="TKB",
            decimals=18,
            chain_id=PLACEHOLDER_CHAIN_ID,
        )
        pool = make_v3_pool(
            "0x" + "cc" * 20,
            token0=token0,
            token1=token1,
            # The Uniswap V3 mainnet factory, unshipped on chain 0 — the exact
            # shape the offline fixtures register.
            factory=UNISWAP_V3_MAINNET_FACTORY,
            fee=3000,
            tick_spacing=60,
            sqrt_price_x96=1 << 96,
            tick=0,
            liquidity=1_000_000,
            py_bot=py_bot,
        )
        assert pool.init_hash == UNISWAP_V3_MAINNET_INIT_HASH
