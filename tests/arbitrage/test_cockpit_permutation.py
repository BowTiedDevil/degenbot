"""Permutation flows from config, not a module global (epic Y7PA5A, task SDFQLL).

The example used to mutate ``driver_constants.PATH_PERMUTATION_FILTER``
before construction while ALSO passing ``permutation=`` into
``ArbitrageConfig.from_env`` — two paths for one value. The decision:
the config is the single path; the global is deleted and ``build_paths``
takes the filter from its (config-derived) parameter.
"""

from __future__ import annotations

from typing import Any

from degenbot.pathfinding import PoolKind
from degenbot.runner.build_paths import (
    BuildPathsOptions,
    _parse_permutation_filter,
    _pool_types_from_filter,
    build_paths,
)
from tests.fakes.engine import FakeEngine as _FakeEngine, FakeEngineRegistry as _FakeEngineRegistry

EXAMPLE = "examples/eth_settlement_arbitrage_v2_v3_v4_rust.py"


class _FakePipeline:
    def __init__(self) -> None:
        self.pool_type_per_depth: Any = None
        self.pool_types: Any = None
        self.path_count = 0
        self.skip_count = 0
        self.token_filter_count = 0
        self.engine_reject_count = 0
        self.other_exc_count = 0
        self.v4_hook_rejected = 0
        self.v4_dynamic_fee_rejected = 0
        self.dup_count = 0
        self.direction_fail_count = 0
        self.register_fail_count = 0
        self.v4_pool_count = 0
        # PRG-5: the benign path-cap stop witness (read by build_paths).
        self.capped = False

    def discovery_sweep(self) -> object:
        # The fake run_registration ignores the producer; an empty iterator
        # stands in for the (infinite in production) discovery sweep.
        return iter(())

    async def run_registration(self, *, producer: Any) -> None:
        pass

    def emit_registration_progress(self, *, force: bool = False) -> None:
        pass


class TestPermutation:
    async def test_build_paths_reads_permutation_from_param(self) -> None:
        pipe = _FakePipeline()
        await build_paths(
            bot=object(),  # type: ignore[arg-type]
            engine_registry=_FakeEngineRegistry(),  # type: ignore[arg-type]
            options=BuildPathsOptions(
                context=object(),  # type: ignore[arg-type]
                pipeline=pipe,  # type: ignore[arg-type]
                retry_policy=None,
                permutation_filter=frozenset({"V3-V4-V3"}),
            ),
        )
        assert pipe.pool_type_per_depth == [
            {PoolKind.V3},
            {PoolKind.V4},
            {PoolKind.V3},
        ]
        assert set(pipe.pool_types) == {PoolKind.V3, PoolKind.V4}

    async def test_build_paths_without_filter_uses_all_types(self) -> None:
        pipe = _FakePipeline()
        await build_paths(
            bot=object(),  # type: ignore[arg-type]
            engine_registry=_FakeEngineRegistry(),  # type: ignore[arg-type]
            options=BuildPathsOptions(
                context=object(),  # type: ignore[arg-type]
                pipeline=pipe,  # type: ignore[arg-type]
                retry_policy=None,
            ),
        )
        assert pipe.pool_type_per_depth is None
        assert set(pipe.pool_types) == {PoolKind.V2, PoolKind.V3, PoolKind.V4}

    def test_driver_constants_has_no_path_permutation_global(self) -> None:
        import degenbot.runner._driver_constants as dc

        assert not hasattr(dc, "PATH_PERMUTATION_FILTER"), (
            "the module global must be deleted; the config is the single path"
        )

    def test_example_has_no_driver_constants_import(self) -> None:
        from pathlib import Path

        src = Path(EXAMPLE).read_text(encoding="utf-8")
        assert "driver_constants" not in src
