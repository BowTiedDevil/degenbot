"""Sync/async provider surface parity (ADR-086).

The two provider trees are two call-convention adapters over ONE contract:
the sync mixins + :class:`AlloyProvider` and the async twins must present the
same method surface — same names, same parameters, same defaults — modulo the
``await`` keyword. The agreement is pinned here, never maintained by hand: a
new method added to one tree and not the other, or a signature that drifts,
fails red.

The intentional differences are an explicit, reviewed list below — the
call-convention asymmetries (sync context manager, async ``create`` /
``as_async_alloy``) and the sync-only renamed duplicates scheduled for
deletion (``chain_id`` / ``block_number`` properties, ADR-086 D3).
"""

from __future__ import annotations

import inspect
from collections.abc import Callable

from degenbot.provider.async_provider import AsyncAlloyProvider
from degenbot.provider.sync import AlloyProvider, LogFilter

# Methods that exist on exactly one tree by design — the call-convention
# asymmetries, not drift. Anything beyond this list fails the parity walk.
_SYNC_ONLY_INTENTIONAL = frozenset({"__enter__", "__exit__", "to_alloy_provider"})
_ASYNC_ONLY_INTENTIONAL = frozenset({"create", "as_async_alloy"})


def _public_methods(cls: type) -> dict[str, Callable]:
    """The public callables a tree presents, keyed by name.

    Walks the MRO (the mixin trees) so a method defined on any mixin counts
    once, under its name. Dunders except the intentional context-manager pair
    are excluded — they are Python surface, not the provider contract.
    """
    methods: dict[str, Callable] = {}
    for klass in cls.__mro__:
        if klass is object:
            continue
        for name, member in vars(klass).items():
            if name.startswith("_") and name not in _SYNC_ONLY_INTENTIONAL:
                continue
            if isinstance(member, (staticmethod, classmethod)):
                member = member.__func__
            if isinstance(member, property):
                member = member.fget
            if inspect.isfunction(member):
                methods.setdefault(name, member)
    return methods


def _signature_of(fn: Callable) -> inspect.Signature:
    return inspect.signature(fn)


class TestSurfaceParity:
    """The two trees present the same method names and signatures."""

    def test_method_names_agree(self) -> None:
        sync = set(_public_methods(AlloyProvider))
        async_ = set(_public_methods(AsyncAlloyProvider))

        sync_only = sync - async_
        async_only = async_ - sync

        expected_sync_only = set(_SYNC_ONLY_INTENTIONAL)
        assert sync_only == expected_sync_only, (
            f"unexpected sync-only methods: {sync_only - expected_sync_only}; "
            f"missing: {expected_sync_only - sync_only}"
        )
        assert async_only == set(_ASYNC_ONLY_INTENTIONAL), (
            f"unexpected async-only methods: {async_only - set(_ASYNC_ONLY_INTENTIONAL)}; "
            f"missing: {set(_ASYNC_ONLY_INTENTIONAL) - async_only}"
        )

    def test_shared_signatures_agree(self) -> None:
        sync = _public_methods(AlloyProvider)
        async_ = _public_methods(AsyncAlloyProvider)
        shared = (set(sync) & set(async_)) - {"__enter__", "__exit__"}

        mismatches = {
            name: (_signature_of(sync[name]), _signature_of(async_[name]))
            for name in shared
            if _signature_of(sync[name]) != _signature_of(async_[name])
        }
        assert not mismatches, "signature drift between the trees: " + "; ".join(
            f"{name}: sync {s} vs async {a}" for name, (s, a) in mismatches.items()
        )


class TestGetLogsContractParity:
    """get_logs answers ONE contract on both trees (ADR-086 D2): a LogFilter
    or keyword args, with the range validated once."""

    def test_async_get_logs_accepts_a_logfilter(self) -> None:
        sig = _signature_of(AsyncAlloyProvider.get_logs)
        # The async twin accepts the same LogFilter-or-kwargs contract as the
        # sync tree — the parameter exists on both.
        assert "filter_param" in sig.parameters, (
            f"async get_logs does not accept filter_param; signature: {sig}"
        )

    def test_logfilter_validates_the_range_once(self) -> None:
        # The range validation lives on the LogFilter value both trees accept,
        # so a reversed range is refused identically on either tree.
        import pytest

        with pytest.raises(ValueError, match="to_block must be >= from_block"):
            LogFilter(from_block=100, to_block=50)
