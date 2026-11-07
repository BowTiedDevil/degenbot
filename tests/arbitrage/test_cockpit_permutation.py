"""Permutation flows from config into the engine's crawl configuration.

The config is the single path: ``build_paths`` resolves the filter from its
(config-derived) parameter, and the resolved values enter the engine's crawl
configuration as data.
"""

from __future__ import annotations

from typing import Any

from degenbot.pathfinding import PoolKind
from degenbot.runner.build_paths import (
    _parse_permutation_filter,
    _pool_types_from_filter,
    crawl_config,
)
from tests.helpers.erc20_factory import make_native_token
from tests.helpers.seam import Bot as SeamBot


class _Context:
    """The construction-context slice the configuration resolution reads."""

    chain_id = 1

    class construction_route:
        factories = ()

        generic = True

    class bot:
        """The native-token seam the configuration reads (ADR-097)."""

        @staticmethod
        def get_native_token():

            return make_native_token(SeamBot(chain_id=1))

    def __init__(self) -> None:

        import pathlib  # ruff:ignore[import-outside-top-level] -- a fixed fixture path

        self.database_path = pathlib.Path("/nonexistent/permutation.sqlite")


class _Registry:
    verify_snapshot_block = None
    path_predicate = None


def _config(perms: set[str] | None) -> dict[str, Any]:
    depth = _parse_permutation_filter(perms)
    return crawl_config(
        context=_Context(),
        engine_registry=_Registry(),
        max_paths=0,
        discovery_batch_size=1000,
        retry_policy=None,
        pool_types=_pool_types_from_filter(perms),
        pool_type_per_depth=depth,
    )


class TestPermutation:
    def test_the_filter_resolves_per_depth(self) -> None:
        assert _parse_permutation_filter({"V3-V4-V3"}) == [
            {PoolKind.V3},
            {PoolKind.V4},
            {PoolKind.V3},
        ]
        assert set(_pool_types_from_filter({"V3-V4-V3"})) == {PoolKind.V3, PoolKind.V4}

    def test_the_config_carries_the_filter_as_data(self) -> None:
        config = _config({"V3-V4-V3"})
        assert config["pool_type_per_depth"] == [
            [PoolKind.V3],
            [PoolKind.V4],
            [PoolKind.V3],
        ]
        assert set(config["pool_kinds"]) == {PoolKind.V3, PoolKind.V4}

    def test_without_a_filter_every_family_is_allowed(self) -> None:
        config = _config(None)
        assert config["pool_type_per_depth"] is None
        assert set(config["pool_kinds"]) == {PoolKind.V2, PoolKind.V3, PoolKind.V4}
