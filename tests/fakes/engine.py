"""The one contract-testable engine double for the runner/registry seam.

Post-C4 the Python driver shell touches a narrow slice of the Rust
``ArbitrageEngine`` surface. Every test that drives the runner or the registry
builds its double here rather than re-declaring a private copy, so a surface
change lands in one place. :data:`RETIRED_ENGINE_MEMBERS` names the methods the
real engine removed; the parity test in
``tests/arbitrage/test_engine_fake_parity.py`` fails if a double ever regrows
one.

``EngineSeam`` is the declared contract: the union of the engine methods the
Python driver shell calls. The parity test binds the real engine, the stub, and
this fake to that one list, so the fake cannot lag or lead reality by name.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any, Protocol

from degenbot.exceptions import VerificationMismatchError

if TYPE_CHECKING:
    from degenbot._ffi import AsyncAlloyProvider


#: The engine interface the Python driver shell depends on. The parity test
#: checks this set against the real engine, the ``.pyi`` stub, and
#: :class:`FakeEngine`.
ENGINE_SEAM_MEMBERS: tuple[str, ...] = (
    "disable_strategy",
    "enable_strategy",
    "install_inline_simulator",
    "last_processed_block",
    "path_count",
    "pool_id_for_pool",
    "pool_id_for_v4_pool",
    "pump_finished_future",
    "reconcile_hosted_head",
    "register_and_solve_path",
    "release_all_v3_v4_quarantined",
    "resume",
    "run_v3_registration_lifecycle",
    "run_v3_registration_lifecycle_sync",
    "run_v3_registration_lifecycle_with_retry_sync",
    "run_v4_registration_lifecycle",
    "run_v4_registration_lifecycle_sync",
    "run_v4_registration_lifecycle_with_retry_sync",
    "set_path_cap",
    "set_verify_rpc_url",
    "set_verify_state_view",
    "snapshot_seed_block",
    "start",
    "stop",
    "strategies",
    "subscribe",
    "v2_pool_count",
    "v3_pool_count",
    "v4_pool_count",
)


#: Engine methods the real surface retired (the Phase-A snapshot-ingest ritual).
#: A double must never define one again.
RETIRED_ENGINE_MEMBERS: tuple[str, ...] = (
    "backfill_from_snapshot",
    "load_v3_snapshot_from_py",
    "load_v4_snapshot_from_py",
)


class EngineSeam(Protocol):
    """The driver-shell slice of ``ArbitrageEngine``.

    Structural only: the real pyclass satisfies it, :class:`FakeEngine`
    implements it, and the parity test reflects over it.
    """

    snapshot_seed_block: int | None

    def subscribe(self, rpc_url: str) -> int: ...
    def start(
        self, node_http: str, node_ws: str, verify_state_view: str | None = None
    ) -> int: ...
    def set_verify_rpc_url(self, rpc_url: str) -> None: ...
    def set_verify_state_view(self, address: str) -> None: ...
    def run_v3_registration_lifecycle(
        self, address: str, snapshot_block: int | None
    ) -> Any: ...
    def run_v4_registration_lifecycle(
        self, pool_manager_address: str, pool_id_hex: str, snapshot_block: int | None
    ) -> Any: ...
    def run_v3_registration_lifecycle_sync(
        self, address: str, snapshot_block: int | None
    ) -> None: ...
    def run_v4_registration_lifecycle_sync(
        self, pool_manager_address: str, pool_id_hex: str, snapshot_block: int | None
    ) -> None: ...
    def run_v3_registration_lifecycle_with_retry_sync(
        self,
        address: str,
        snapshot_block: int | None,
        max_attempts: int,
        base_delay: float,
        max_delay: float,
        jitter: float,
    ) -> None: ...
    def run_v4_registration_lifecycle_with_retry_sync(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        snapshot_block: int | None,
        max_attempts: int,
        base_delay: float,
        max_delay: float,
        jitter: float,
    ) -> None: ...
    def register_and_solve_path(
        self, pool_refs: list[tuple[int, bool]]
    ) -> tuple[int, bool]: ...
    def pool_id_for_pool(self, family_tag: str, address: str) -> int | None: ...
    def pool_id_for_v4_pool(self, pool_manager: str, pool_id_hex: str) -> int | None: ...
    def install_inline_simulator(
        self, context: object, erc6909_profit: bool  # ruff: ignore[boolean-type-hint-positional-argument]
    ) -> None: ...
    def set_path_cap(self, cap: int | None) -> None: ...
    def release_all_v3_v4_quarantined(self) -> None: ...
    def last_processed_block(self) -> int | None: ...
    def v2_pool_count(self) -> int: ...
    def v3_pool_count(self) -> int: ...
    def v4_pool_count(self) -> int: ...
    def path_count(self) -> int: ...
    def pump_finished_future(self) -> Any: ...
    def resume(self, facets: list[str]) -> None: ...
    def stop(self) -> None: ...
    def reconcile_hosted_head(
        self, provider: AsyncAlloyProvider, operator_address: str
    ) -> Any: ...
    def enable_strategy(self, name: str) -> str: ...
    def disable_strategy(self, name: str) -> None: ...
    def strategies(self) -> list[tuple[str, str, str | None]]: ...


class FakeEngine:
    """In-process stand-in for the driver-shell engine surface.

    Every call is recorded. Behaviour defaults match a healthy pre-resume
    engine: no pump, no hosted activity, empty pool/path registries.
    """

    def __init__(
        self,
        *,
        events: list[str] | None = None,
        backfill_target: int = 12_000,
        last_processed_block: int | None = 12_345,
        stop_raises: Exception | None = None,
        hosted_activity: bool = False,
    ) -> None:
        self._events = events
        self.calls: list[str] = []
        self.resumed = False
        self.stop_calls = 0
        self.stop_raises = stop_raises
        self._backfill_target = backfill_target
        self._last_processed_block = last_processed_block
        self._snapshot_seed_block: int | None = None
        self.seed_args: list[int | None] = []
        self.subscribe_calls: list[str] = []
        self.start_calls: list[tuple[str, str, str | None]] = []
        self._started = False
        self.run_calls: list[dict[str, Any]] = []
        self.register_calls: list[list[tuple[int, bool]]] = []
        self.reconcile_calls: list[dict[str, Any]] = []
        #: Counts chain reads the reconcile path would perform. Stays 0 while
        #: the guard is closed (no lease / no non-terminal record).
        self.reconcile_chain_reads = 0
        self.hosted_activity = hosted_activity
        self.fail_next: str | None = None
        self.released_quarantines = 0
        self.inline_sim_installs: list[dict[str, Any]] = []
        self.path_cap: int | None = None
        #: The declared pool identities this double answers lookups from —
        #: the double's stand-in for the shared BotState's registration
        #: tables, NOT a registry-side cache.
        self._identities: dict[tuple[str, str], int] = {}
        self._pump_finished = asyncio.Event()
        #: The facet set the last resume() received (the engine owns enabling
        #: it before driving hosted loops).
        self.resume_facets: list[str] = []
        self._strategy_records: list[tuple[str, str, str | None]] = [
            ("settlement", "registered", None),
            ("mevblocker_backrun", "registered", None),
            ("txpool_backrun", "registered", None),
        ]

    # ── recording ──────────────────────────────────────────────────

    def _record(self, name: str) -> None:
        self.calls.append(name)
        if self._events is not None:
            self._events.append(name)

    def finish_pump(self) -> None:
        """Resolve the pump-completion surface (watchdog tests)."""
        self._pump_finished.set()

    # ── lifecycle ──────────────────────────────────────────────────

    def resume(self, facets: list[str]) -> None:
        """Enable the named facets, then resume the pump (the engine-owned
        enable-then-resume ordering, mirrored in-process)."""
        self.resumed = True
        self.resume_facets = list(facets)
        for facet in facets:
            self.enable_strategy(facet)
        self._record("resume")

    def stop(self) -> None:
        self.stop_calls += 1
        self._record("stop")
        if self.stop_raises is not None:
            raise self.stop_raises

    async def pump_finished_future(self) -> None:
        await self._pump_finished.wait()

    def last_processed_block(self) -> int | None:
        return self._last_processed_block

    # ── pool / path registry ───────────────────────────────────────

    def v2_pool_count(self) -> int:
        return 0

    def v3_pool_count(self) -> int:
        return 0

    def v4_pool_count(self) -> int:
        return 0

    def path_count(self) -> int:
        return 0

    def release_all_v3_v4_quarantined(self) -> None:
        self.released_quarantines += 1
        self._record("release_all_v3_v4_quarantined")

    def set_path_cap(self, cap: int | None) -> None:
        self.path_cap = cap
        self._record("set_path_cap")

    def register_and_solve_path(
        self, pool_refs: list[tuple[int, bool]]
    ) -> tuple[int, bool]:
        self.register_calls.append(list(pool_refs))
        path_id = len(self.register_calls)
        return path_id, True

    # ── core-derived pool identity (S4) ───────────────────────────

    def register_identity(
        self, family_tag: str, address: str, pool_id: int
    ) -> None:
        """Declare a pool identity the double answers lookups from.

        The double's identity table stands in for the SHARED BotState (the
        real core's registration tables), so a test that needs a resolvable
        identity seeds it here rather than reaching into a registry-side map
        that no longer exists.
        """
        self._identities[family_tag, address.lower()] = pool_id

    def pool_id_for_pool(self, family_tag: str, address: str) -> int | None:
        self._record("pool_id_for_pool")
        return self._identities.get((family_tag, address.lower()))

    def pool_id_for_v4_pool(self, pool_manager: str, pool_id_hex: str) -> int | None:
        self._record("pool_id_for_v4_pool")
        return self._identities.get(("v4", f"{pool_manager}/{pool_id_hex}".lower()))

    # ── pre-resume ritual ──────────────────────────────────────────

    @property
    def snapshot_seed_block(self) -> int | None:
        return self._snapshot_seed_block

    @snapshot_seed_block.setter
    def snapshot_seed_block(self, value: int | None) -> None:
        self._record("set_snapshot_seed_block")
        self.seed_args.append(value)
        self._snapshot_seed_block = value

    def subscribe(self, rpc_url: str) -> int:
        self.subscribe_calls.append(rpc_url)
        self._record("subscribe")
        return self._backfill_target

    def start(
        self,
        node_http: str,
        node_ws: str,
        verify_state_view: str | None = None,
    ) -> int:
        """The one-call startup ritual (the Rust ``EngineDriver::start`` mirror):

        ``subscribe(ws)`` then verify-config, stopping before ``resume()``. The
        granular recorders below are invoked so the observed call order is the
        driver's; a re-entry is refused exactly as the driver refuses a second
        subscribe.
        """
        if self._started:
            msg = "Cannot subscribe: already subscribed. Call resume() first."
            raise RuntimeError(msg)
        self._started = True
        self.start_calls.append((node_http, node_ws, verify_state_view))
        target = self.subscribe(node_ws)
        self.set_verify_rpc_url(node_http)
        if verify_state_view is not None:
            self.set_verify_state_view(verify_state_view)
        return target

    def set_verify_rpc_url(self, rpc_url: str) -> None:
        self._record("set_verify_rpc_url")

    def set_verify_state_view(self, address: str) -> None:
        self._record("set_verify_state_view")

    def install_inline_simulator(
        self, context: object, erc6909_profit: bool  # ruff: ignore[boolean-type-hint-positional-argument]
    ) -> None:
        self.inline_sim_installs.append(
            {"context": context, "erc6909_profit": erc6909_profit}
        )

    # ── core-owned registration lifecycle ──────────────────────────

    async def run_v3_registration_lifecycle(
        self, address: str, snapshot_block: int | None
    ) -> None:
        self.run_calls.append(
            {"family": "v3", "address": address, "snapshot_block": snapshot_block}
        )
        if self.fail_next == "v3":
            msg = "synthetic V3 seed tick mismatch"
            raise VerificationMismatchError(msg)

    async def run_v4_registration_lifecycle(
        self, pool_manager_address: str, pool_id_hex: str, snapshot_block: int | None
    ) -> None:
        self.run_calls.append(
            {
                "family": "v4",
                "address": pool_manager_address,
                "pool_id": pool_id_hex,
                "snapshot_block": snapshot_block,
            }
        )
        if self.fail_next == "v4":
            msg = "synthetic V4 seed tick mismatch"
            raise VerificationMismatchError(msg)

    def run_v3_registration_lifecycle_sync(
        self, address: str, snapshot_block: int | None
    ) -> None:
        self.run_calls.append(
            {"family": "v3-sync", "address": address, "snapshot_block": snapshot_block}
        )

    def run_v4_registration_lifecycle_sync(
        self, pool_manager_address: str, pool_id_hex: str, snapshot_block: int | None
    ) -> None:
        self.run_calls.append(
            {
                "family": "v4-sync",
                "address": pool_manager_address,
                "pool_id": pool_id_hex,
                "snapshot_block": snapshot_block,
            }
        )

    def run_v3_registration_lifecycle_with_retry_sync(
        self,
        address: str,
        snapshot_block: int | None,
        max_attempts: int,
        base_delay: float,
        max_delay: float,
        jitter: float,
    ) -> None:
        self.run_calls.append(
            {
                "family": "v3-sync-with-retry",
                "address": address,
                "snapshot_block": snapshot_block,
                "max_attempts": max_attempts,
                "base_delay": base_delay,
                "max_delay": max_delay,
                "jitter": jitter,
            }
        )

    def run_v4_registration_lifecycle_with_retry_sync(
        self,
        pool_manager_address: str,
        pool_id_hex: str,
        snapshot_block: int | None,
        max_attempts: int,
        base_delay: float,
        max_delay: float,
        jitter: float,
    ) -> None:
        self.run_calls.append(
            {
                "family": "v4-sync-with-retry",
                "address": pool_manager_address,
                "pool_id": pool_id_hex,
                "snapshot_block": snapshot_block,
                "max_attempts": max_attempts,
                "base_delay": base_delay,
                "max_delay": max_delay,
                "jitter": jitter,
            }
        )

    # ── per-head hosted reconciliation ─────────────────────────────

    async def reconcile_hosted_head(
        self, provider: AsyncAlloyProvider, operator_address: str
    ) -> int:
        if not self.hosted_activity:
            # Mirrors the Rust guard: a boot with no lease and no non-terminal
            # record short-circuits before the chain read.
            self.reconcile_calls.append({"operator_address": operator_address, "folded": 0})
            return 0
        self.reconcile_chain_reads += 1
        self.reconcile_calls.append({"operator_address": operator_address, "folded": 0})
        return 0

    # ── strategy-host operator verbs ───────────────────────────────

    def enable_strategy(self, name: str) -> str:
        for index, (strategy, _state, _halt) in enumerate(self._strategy_records):
            if strategy == name:
                self._strategy_records[index] = (name, "enabled", None)
                return "enabled"
        return "unknown"

    def disable_strategy(self, name: str) -> None:
        for index, (strategy, _state, _halt) in enumerate(self._strategy_records):
            if strategy == name:
                self._strategy_records[index] = (name, "disabled", None)
                return

    def strategies(self) -> list[tuple[str, str, str | None]]:
        return list(self._strategy_records)


class FakeEngineRegistry:
    """Stand-in for :class:`EngineRegistry` at the runner's actor seam.

    ``start`` records the ritual call and returns the engine's backfill target
    without resuming (the real registry stops before ``resume()``).
    """

    def __init__(
        self,
        engine: FakeEngine | None = None,
        *,
        backfill_target: int = 12_000,
        events: list[str] | None = None,
    ) -> None:
        self.engine = engine if engine is not None else FakeEngine(
            backfill_target=backfill_target, events=events
        )
        self.start_calls: list[dict[str, Any]] = []

    def start(
        self,
        node_http: str,
        node_ws: str,
        *,
        v3_snapshot: object,
        v4_snapshot: object,
        verify_state_view: str | None = None,
    ) -> int:
        self.start_calls.append(
            {
                "node_http": node_http,
                "node_ws": node_ws,
                "v3_snapshot": v3_snapshot,
                "v4_snapshot": v4_snapshot,
                "verify_state_view": verify_state_view,
            }
        )
        return self.engine._backfill_target
