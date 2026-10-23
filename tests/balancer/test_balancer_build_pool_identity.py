"""`Bot.build_pool` resolves Balancer identity; the builder derives the
MetaStablePool invariant version from the pool's shape.

The two legs are one regression pair, both measured against the pinned block:

- `Bot.build_pool` refused every Balancer address as "pool build refused: out
  of spec". Not a builder defect: the resolver could not classify the address
  (a deployed Balancer pool has no `factory()` getter, and Balancer V2 has no
  enumerable registry), so `build_pool` took the generic Curve arm and
  `build_curve_pool` refused the pool for having fewer than two coins.
- The builder derived the StableMath invariant version from the Vault pool-id
  specialization, which is a token-count artifact: the 2-token wstETH/WETH
  MetaStablePool carries `specialization = 0x0002` (`TwoToken`) and was read as
  V1, while the deployed contract rounds `D_P` up (V2).

Identity is asserted through `Bot.build_pool` itself, so these legs cover the
resolver, the construction arm, and the registration seam in one call.
"""

import pytest

from degenbot.balancer.stable_pools import (
    INVARIANT_V1,
    INVARIANT_V2,
    BalancerV2StablePool,
)
from degenbot.fork import AnvilFork
from tests.sweep_pin import SWEEP_FORK_BLOCK, assert_fork_at_sweep_pin

pytestmark = pytest.mark.online_rpc

# ComposableStablePool (Factory rev3) — lists its own BPT as Vault token 0, so
# the shape resolves to INVARIANT_V1.
_BB_S_USD = "0x779d01F939D78a918A3de18cC236ee89221dfd4E"
# MetaStablePool — no BPT in the token list, and its factory is not one of the
# shipped registry rows, so the identity comes from the on-chain shape probe.
_WSTETH_WETH = "0x32296969Ef14EB0c6d29669C550D4a0449130230"


@pytest.mark.parametrize("fork_mainnet_archive", [SWEEP_FORK_BLOCK], indirect=True)
class TestBuildPoolBalancerIdentity:
    """`Bot.build_pool` resolves a Balancer address to its own family."""

    def test_composable_stable_pool_resolves_and_builds(
        self,
        bot_mainnet_archive,
        fork_mainnet_archive: AnvilFork,
    ) -> None:
        assert_fork_at_sweep_pin(fork_mainnet_archive)

        pool = bot_mainnet_archive.build_pool(_BB_S_USD, state_block=SWEEP_FORK_BLOCK)

        assert isinstance(pool, BalancerV2StablePool)
        assert pool.bpt_idx == 0
        assert pool.invariant_version == INVARIANT_V1

    def test_metastable_pool_builds_with_the_round_up_invariant(
        self,
        bot_mainnet_archive,
    ) -> None:
        """The shape — no BPT — is what separates MetaStable from composable.

        This is the leg the pool-id specialization read backwards: `0x0002` is
        `TwoToken`, not a family marker.
        """
        pool = bot_mainnet_archive.build_pool(_WSTETH_WETH, state_block=SWEEP_FORK_BLOCK)

        assert isinstance(pool, BalancerV2StablePool)
        assert pool.bpt_idx is None
        assert pool.invariant_version == INVARIANT_V2
