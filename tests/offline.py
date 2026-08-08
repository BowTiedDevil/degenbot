"""Shared detection of the constrained (no-RPC, no-anvil) CI/CD runner.

Local dev machines have a reachable RPC + a running anvil, so live-network tests
(anvil forks, README live-RPC examples) execute and are validated. The CI/CD
``python-test`` runner has neither, so those same tests must be skipped rather
than failed.

Offline mode is the CI default (GitHub Actions exports ``CI=true``) and can be
forced either way with ``DEGENBOT_OFFLINE=1|0`` so a developer can reproduce CI
behaviour locally.
"""

import os


def is_offline() -> bool:
    """Return True when running in an environment without a live RPC/anvil node."""
    override = os.environ.get("DEGENBOT_OFFLINE")
    if override is not None:
        return override.lower() in {"1", "true", "yes"}
    return os.environ.get("CI", "").lower() == "true"
