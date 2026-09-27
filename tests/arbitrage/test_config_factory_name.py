"""The config factory's name states what it builds from.

``ArbitrageConfig.build`` assembles from the resolved verdict, the CLI flags,
and the process-environment identity; it no longer resolves the operational
stances itself. The name says so, and the old environment-implying name is
gone rather than aliased.
"""

from __future__ import annotations

from degenbot.runner.config import ArbitrageConfig


def test_the_factory_is_named_build() -> None:
    assert callable(ArbitrageConfig.build)


def test_the_old_from_env_name_is_absent() -> None:
    assert not hasattr(ArbitrageConfig, "from_env")
