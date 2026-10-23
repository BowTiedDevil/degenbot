"""The one home for the pinned sweep block and the pinned-fork hygiene legs.

Every registry/factory compliance sweep replays against one archive block, so the
pin is a single constant instead of a number copied into each module: advancing
it is one deliberate edit here, and ``tests/test_sweep_pin_census.py`` fails if
the number is spelled out anywhere else under ``tests/``.

Why this block: it was the local archive node's stage-complete head when the
sweeps were pinned, so the node answers for it consistently and the sweep's
expected values are that block's state. Advance it deliberately and record the
reason in this comment — a pin advanced without a note silently changes the
premise of every sweep assertion.

Why the hygiene legs: a pinned fork's whole value is that its block's state is
immutable, so a fork that drifted to the node's head would replay every leg
against moving state. And anvil's fork storage cache is one on-disk file per
block shared by every anvil instance; an entry written while the archive node
could not answer consistently for that block persists, and makes a later fork at
that block serve state the block's header never had. ``tests/conftest.py::_fork``
already forces ``storage_caching=False`` for a pinned block, so the legs below
assert that rather than assume it.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from degenbot.fork import AnvilFork

SWEEP_FORK_BLOCK = 26204126


def assert_fork_at_sweep_pin(fork: AnvilFork) -> None:
    """The fork serves ``SWEEP_FORK_BLOCK``, not the archive node's head."""
    block_number = fork.provider.get_block_number()
    assert block_number == SWEEP_FORK_BLOCK, (
        f"fork is at block {block_number}, not the sweep pin {SWEEP_FORK_BLOCK}: "
        f"a fork that drifted to the node's head replays these legs against moving "
        f"state, so the pin is asserted rather than assumed"
    )


def assert_pinned_fork_storage_cache_disabled(fork: AnvilFork) -> None:
    """A pinned fork must not read anvil's shared on-disk per-block storage cache."""
    assert fork._launch.storage_caching is False, (
        "a pinned fork must not read anvil's per-block storage cache: an entry "
        "written while the archive node could not answer consistently for this "
        "block persists, and the sweep would read state the block's header never had"
    )


class SweepPinHygiene:
    """Pinned-fork hygiene legs a sweep suite inherits.

    A suite parametrizes its own ``TestPinnedForkHygiene(SweepPinHygiene)`` class
    over ``fork_mainnet_archive``; the pin is part of the expected value of every
    assertion in the suite, so these legs assert it instead of assuming it.
    """

    def test_fork_is_pinned_to_the_sweep_block(self, fork_mainnet_archive: AnvilFork) -> None:
        """The fork block is part of the expected value of every assertion here."""
        assert_fork_at_sweep_pin(fork_mainnet_archive)

    def test_pinned_fork_disables_the_storage_cache(self, fork_mainnet_archive: AnvilFork) -> None:
        """A pinned fork must not read anvil's on-disk per-block storage cache."""
        assert_pinned_fork_storage_cache_disabled(fork_mainnet_archive)
