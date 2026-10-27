"""Sync/async/offline provider surface parity (ADR-086).

The provider adapters are call-convention shells over ONE contract: the sync
mixins + :class:`AlloyProvider`, the async twins, and :class:`OfflineProvider`
-- a third adapter at the same seam, which is itself a real Rust
``AlloyProvider`` over an in-memory recorded transport. They must present the
same method surface: same names, same parameters, same defaults, modulo the
``await`` keyword. The agreement is pinned here, never maintained by hand: a
new method added to one adapter and not the others, or a signature that
drifts, fails red.

The same seam exists one layer down, in the Rust pair
``degenbot._ffi.provider.AlloyProvider`` / ``AsyncAlloyProvider``. The async
pyclass answers with a coroutine (``pyo3_async_runtimes::future_into_py``), so
its return annotation -- carried by ``#[pyo3(signature = (...) -> "...")]`` --
spells the awaited type, and that awaited type is the sync pyclass's return
type. The pyi is ``just gen-stubs`` output (drift-gated by
``tests/ffi/test_ffi_stub_drift.py``), so reading it here pins the pair itself.

The intentional differences are explicit, reviewed lists below: the
call-convention asymmetries (sync context manager, async ``create`` /
``as_async_alloy``), the sync-only renamed duplicate ``to_alloy_provider``, the
live-RPC surface the recorded transport does not serve, the recorded-data
introspection on :class:`OfflineProvider`, and the pyclass members only one
side of the Rust pair has.
"""

from __future__ import annotations

import ast
import inspect
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

import degenbot
from degenbot.provider.async_provider import AsyncAlloyProvider
from degenbot.provider.offline_provider import OfflineProvider
from degenbot.provider.sync import AlloyProvider, LogFilter

if TYPE_CHECKING:
    from collections.abc import Callable

# Methods that exist on exactly one alloy tree by design — the call-convention
# asymmetries, not drift. Anything beyond this list fails the parity walk.
_SYNC_ONLY_INTENTIONAL = frozenset({"__enter__", "__exit__", "to_alloy_provider"})
_ASYNC_ONLY_INTENTIONAL = frozenset({"create", "as_async_alloy"})

# :class:`OfflineProvider` presents the recorded-data surface plus the two
# paths the alloy trees have no analogue for: the corpus's block-number list
# and the recorded-JSON factory (its construction path).
_OFFLINE_ONLY_INTENTIONAL = frozenset({"block_numbers", "from_json_file"})

# Tree surface with no offline peer: the alloy trees' call-convention
# asymmetries, the live-node RPCs the recorded transport does not serve
# (``degenbot_rpc::offline`` answers calls, code, blocks and recorded logs
# only), and endpoint identity — an offline provider has no URL.
_OFFLINE_ABSENT_INTENTIONAL = frozenset({
    "__enter__",
    "__exit__",
    "as_async_alloy",
    "create",
    "estimate_gas",
    "get_gas_price",
    "get_transaction",
    "get_transaction_receipt",
    "rpc_url",
})

# Constructors are construction paths, not RPC contract — the walk below is the
# method surface, so ``__init__`` stays out of it. Each adapter builds
# differently by design (recorded chain data here, an endpoint URL + transport
# knobs on the alloy trees, a wrapped sync provider on the async one); the
# offline difference is pinned as a divergence instead of being a blind spot.
_OFFLINE_CONSTRUCTOR_DIVERGENCE = (
    "recorded chain data (chain_id + blocks) instead of an endpoint URL + transport knobs"
)

# ``degenbot._ffi.provider`` stub pair. Members only one pyclass has: the sync
# pyclass owns the transport (subscriptions, the offline factories, the GIL
# probe) while the async wrapper adds the awaited URL factory it needs and
# can attach to a sync provider directly.
_STUB_SYNC_ONLY_INTENTIONAL = frozenset({
    "offline_from_json_file",
    "offline_from_json_string",
    "subscribe_blocks",
    "subscribe_full_blocks",
    "subscribe_full_pending_transactions",
    "subscribe_logs",
    "subscribe_pending_transactions",
    "verify_gil_release",
})
_STUB_ASYNC_ONLY_INTENTIONAL = frozenset({"create"})

# The async pyclass's members that are not coroutine factories: closing and
# the endpoint read stay synchronous on both sides.
_STUB_ASYNC_NON_COROUTINE = frozenset({"close", "rpc_url"})

# The one recorded parameter divergence between the pyclasses: the async
# wrapper is constructed around an existing sync provider (``create`` is its
# URL factory), so ``__new__`` cannot share the sync constructor's shape.
_STUB_CONSTRUCTOR_DIVERGENCE = "__new__"

_STUB_FILE = Path(degenbot.__file__).parent / "_ffi" / "provider.pyi"


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
        for name, attribute in vars(klass).items():
            if name.startswith("_") and name not in _SYNC_ONLY_INTENTIONAL:
                continue
            member = attribute
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
    """The two alloy trees present the same method names and signatures."""

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


class TestOfflineProviderParity:
    """The recorded-data adapter presents the same surface as the alloy trees."""

    def test_method_names_agree(self) -> None:
        offline = set(_public_methods(OfflineProvider))
        trees = set(_public_methods(AlloyProvider)) | set(_public_methods(AsyncAlloyProvider))

        offline_only = offline - trees
        absent = trees - offline

        assert offline_only == set(_OFFLINE_ONLY_INTENTIONAL), (
            f"unexpected offline-only methods: {offline_only - set(_OFFLINE_ONLY_INTENTIONAL)}; "
            f"missing: {set(_OFFLINE_ONLY_INTENTIONAL) - offline_only}"
        )
        assert absent == set(_OFFLINE_ABSENT_INTENTIONAL), (
            f"offline adapter unexpectedly silent on: {absent - set(_OFFLINE_ABSENT_INTENTIONAL)}; "
            f"recorded as absent but present: {set(_OFFLINE_ABSENT_INTENTIONAL) - absent}"
        )

    def test_shared_signatures_agree(self) -> None:
        offline = _public_methods(OfflineProvider)
        mismatches: dict[str, tuple[inspect.Signature, inspect.Signature]] = {}
        for tree_name, tree in (
            ("sync", _public_methods(AlloyProvider)),
            ("async", _public_methods(AsyncAlloyProvider)),
        ):
            for name in set(offline) & set(tree):
                if _signature_of(offline[name]) != _signature_of(tree[name]):
                    mismatches[f"{tree_name}.{name}"] = (
                        _signature_of(offline[name]),
                        _signature_of(tree[name]),
                    )

        assert not mismatches, "signature drift against the alloy trees: " + "; ".join(
            f"{name}: offline {o} vs tree {t}" for name, (o, t) in mismatches.items()
        )

    def test_constructor_divergence_is_recorded(self) -> None:
        """A divergence that closed must stop being recorded, or it hides drift."""
        assert _OFFLINE_CONSTRUCTOR_DIVERGENCE, "the recorded reason cannot be blank"
        offline = _signature_of(OfflineProvider.__init__)

        for tree in (AlloyProvider, AsyncAlloyProvider):
            assert offline != _signature_of(tree.__init__), (
                f"OfflineProvider.__init__ now matches {tree.__name__}'s constructor — "
                f"drop the recorded divergence"
            )


class TestRustStubPairParity:
    """The Rust pyclass pair's generated stubs answer one contract.

    ``AsyncAlloyProvider`` hands back the shared runtime's future, so each of
    its coroutine members must annotate ``Awaitable[<the sync return type>]``:
    awaiting it yields exactly what the sync pyclass returns.
    """

    @staticmethod
    def _members() -> dict[str, dict[str, ast.FunctionDef]]:
        tree = ast.parse(_STUB_FILE.read_text(encoding="utf-8"))
        return {
            node.name: {
                child.name: child for child in node.body if isinstance(child, ast.FunctionDef)
            }
            for node in tree.body
            if isinstance(node, ast.ClassDef)
        }

    @staticmethod
    def _annotation(node: ast.expr | None) -> str:
        if node is None:
            return "<none>"
        # A manual PyO3 annotation is emitted verbatim as a quoted string, so
        # its value — not its repr — is the annotation.
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            return node.value
        return ast.unparse(node)

    @classmethod
    def _parameters(cls, node: ast.FunctionDef) -> tuple[object, ...]:
        args = node.args
        positional = [*args.posonlyargs, *args.args]
        defaults = [None] * (len(positional) - len(args.defaults)) + [
            ast.unparse(default) for default in args.defaults
        ]
        return (
            tuple(
                (arg.arg, cls._annotation(arg.annotation), default)
                for arg, default in zip(positional, defaults, strict=True)
            ),
            len(args.posonlyargs),
            tuple(
                (
                    arg.arg,
                    cls._annotation(arg.annotation),
                    None if default is None else ast.unparse(default),
                )
                for arg, default in zip(args.kwonlyargs, args.kw_defaults, strict=True)
            ),
            args.vararg.arg if args.vararg else None,
            args.kwarg.arg if args.kwarg else None,
        )

    def test_stub_names_agree(self) -> None:
        members = self._members()
        sync = set(members["AlloyProvider"])
        async_ = set(members["AsyncAlloyProvider"])

        assert sync - async_ == set(_STUB_SYNC_ONLY_INTENTIONAL), (
            f"unexpected sync-only stub members: "
            f"{sync - async_ - set(_STUB_SYNC_ONLY_INTENTIONAL)}; "
            f"missing: {set(_STUB_SYNC_ONLY_INTENTIONAL) - (sync - async_)}"
        )
        assert async_ - sync == set(_STUB_ASYNC_ONLY_INTENTIONAL), (
            f"unexpected async-only stub members: "
            f"{async_ - sync - set(_STUB_ASYNC_ONLY_INTENTIONAL)}; "
            f"missing: {set(_STUB_ASYNC_ONLY_INTENTIONAL) - (async_ - sync)}"
        )

    def test_async_returns_await_to_the_sync_return(self) -> None:
        members = self._members()
        sync = members["AlloyProvider"]
        async_ = members["AsyncAlloyProvider"]
        shared = set(sync) & set(async_)
        coroutine_members = shared - _STUB_ASYNC_NON_COROUTINE - {_STUB_CONSTRUCTOR_DIVERGENCE}

        expected = {
            name: f"Awaitable[{self._annotation(sync[name].returns)}]" for name in coroutine_members
        } | {
            name: self._annotation(sync[name].returns)
            for name in shared - coroutine_members - {_STUB_CONSTRUCTOR_DIVERGENCE}
        }
        actual = {name: self._annotation(async_[name].returns) for name in expected}

        assert actual == expected, "stub return drift between the pyclasses: " + "; ".join(
            f"{name}: sync {self._annotation(sync[name].returns)} vs async {actual[name]}"
            for name in expected
            if actual[name] != expected[name]
        )

    def test_async_only_factory_is_a_coroutine(self) -> None:
        members = self._members()
        for name in _STUB_ASYNC_ONLY_INTENTIONAL:
            annotation = self._annotation(members["AsyncAlloyProvider"][name].returns)
            assert annotation.startswith("Awaitable["), (
                f"{name} must annotate the awaited value; got {annotation!r}"
            )

    def test_shared_parameters_agree(self) -> None:
        members = self._members()
        sync = members["AlloyProvider"]
        async_ = members["AsyncAlloyProvider"]
        shared = (set(sync) & set(async_)) - {_STUB_CONSTRUCTOR_DIVERGENCE}

        mismatches = {
            name: (self._parameters(sync[name]), self._parameters(async_[name]))
            for name in shared
            if self._parameters(sync[name]) != self._parameters(async_[name])
        }
        assert not mismatches, "stub parameter drift between the pyclasses: " + "; ".join(
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

    def test_offline_get_logs_accepts_a_logfilter(self) -> None:
        # The recorded-data adapter answers the same contract and refuses it,
        # rather than failing earlier with a TypeError.
        sig = _signature_of(OfflineProvider.get_logs)
        assert "filter_param" in sig.parameters, (
            f"offline get_logs does not accept filter_param; signature: {sig}"
        )

    def test_logfilter_validates_the_range_once(self) -> None:
        # The range validation lives on the LogFilter value both trees accept,
        # so a reversed range is refused identically on either tree.
        with pytest.raises(ValueError, match="to_block must be >= from_block"):
            LogFilter(from_block=100, to_block=50)
