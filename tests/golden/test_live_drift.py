"""Live chain-data drift gate: the corpus recorder's ``--check``, per chain.

The offline replay suite (``test_oracle_replay_offline``) gates the committed
corpora against the parity goldens with no RPC. This module is the networked
half: it re-runs the ``record_py_oracle_corpus`` recorder's ``--check`` (two
fresh recording passes, byte-identity against the corpora on disk) against
live archive endpoints, so corpus drift is caught by a marked run instead of a
manual command. Each test costs minutes of paced live RPC and the endpoints
rate-limit, so the ``live_drift`` marker is explicit-only: a plain run skips
at the guard below without dialing, and the marker is wired into no default,
CI, or pre-push lane.
"""

from __future__ import annotations

import shutil
import subprocess  # ruff: ignore[suspicious-subprocess-import] — trusted args-list call
from dataclasses import dataclass
from pathlib import Path

import pytest

from tests.conftest import (
    ARBITRUM_FULL_NODE_HTTP_URI,
    BASE_ARCHIVE_NODE_HTTP_URI,
    BASE_FULL_NODE_HTTP_URI,
    ETHEREUM_ARCHIVE_NODE_HTTP_URI,
)

REPO_ROOT = Path(__file__).resolve().parents[2]

# publicnode's arbitrum tier rejects historical state at the camelot pin
# ("Archive requests require a personal token"); blastapi serves the pin keyless.
ARBITRUM_ARCHIVE_NODE = "https://arbitrum-one.public.blastapi.io"

# Per-tier recorder pace: mainnet.base.org answers bursts with "over rate
# limit" (1s between calls keeps it clear); the blastapi tiers tolerate more.
ETHEREUM_PACE_MS = 50
BASE_PACE_MS = 1000
ARBITRUM_PACE_MS = 100

# Whole-recording subprocess budgets: a chain's --check re-records every
# scenario twice (minutes), so the recorder child is bounded per tier via
# subprocess timeout. The tests carry no per-test pytest-timeout mark: a
# param that outruns the 300s global default fails loudly for investigation.

# Scenario lists mirror the recorder's tier wiring exactly; a scenario added
# to the recorder must be added to its chain here or its drift goes unchecked.
CHAIN_1_SCENARIOS = (
    "uniswap_v3_quoter",
    "uniswap_v4_quoter",
    "balancer_stable_given_in",
    "balancer_stable_given_out",
    "balancer_weighted_weth_bal",
    "balancer_weighted_usdc_weth",
    "balancer_weighted_weth_rpl",
    "balancer_expanded_two_token_given_in",
    "balancer_expanded_two_token_given_out",
    "balancer_expanded_multi_token_given_in",
    "balancer_expanded_multi_token_given_out",
    "curve_tripool_get_dy",
    "curve_tricrypto_get_dy",
    "curve_tripool_calc_base_pool",
    "curve_metapool_get_dy",
    "curve_metapool_multiblock",
)
CHAIN_42161_SCENARIOS = ("camelot_v2_get_amount_out",)
CHAIN_8453_SCENARIOS = (
    "aerodrome_v2_volatile_get_amount_out",
    "aerodrome_v2_stable_get_amount_out",
    "aerodrome_v3_quote",
    "pancakeswap_v2_router_get_amounts_out",
)

_NOT_SELECTED_MSG = (
    "live_drift is explicit-only: run with `-m live_drift` (or `just "
    "live-drift`); plain runs stay offline and fast"
)


@dataclass(frozen=True)
class LiveChain:
    """One chain's live ``--check`` invocation: scenarios, endpoint pool, pace."""

    name: str
    scenarios: tuple[str, ...]
    node_flag: str
    # Ordered pool - the recorder rotates to the next entry on a
    # transport-class failure (the record_errors transport class: rate limit,
    # timeout, connection) and re-records the scenario, so one flaky member
    # cannot wedge an unattended run.
    node_urls: tuple[str, ...]
    pace_ms: int
    timeout_s: int


LIVE_CHAINS = (
    pytest.param(
        LiveChain(
            name="ethereum",
            scenarios=CHAIN_1_SCENARIOS,
            node_flag="--ethereum-node",
            # The tests.env tier is the local fork node; the env carries no
            # second ethereum archive, so this tier stays a one-endpoint pool.
            node_urls=(ETHEREUM_ARCHIVE_NODE_HTTP_URI,),
            pace_ms=ETHEREUM_PACE_MS,
            timeout_s=1800,
        ),
        id="ethereum",
    ),
    pytest.param(
        LiveChain(
            name="arbitrum",
            scenarios=CHAIN_42161_SCENARIOS,
            node_flag="--arbitrum-node",
            # blastapi is the proven keyless server of the camelot pin; the
            # authenticated dRPC tier from tests.env (via conftest) is the
            # failover, its pin coverage unproven.
            node_urls=(ARBITRUM_ARCHIVE_NODE, ARBITRUM_FULL_NODE_HTTP_URI),
            pace_ms=ARBITRUM_PACE_MS,
            timeout_s=600,
        ),
        id="arbitrum",
    ),
    pytest.param(
        LiveChain(
            name="base",
            scenarios=CHAIN_8453_SCENARIOS,
            node_flag="--base-node",
            # The authenticated archive tier from tests.env (via conftest)
            # is the primary; the keyless parity fork endpoint is the
            # failover (it rate-limits bursts - see BASE_PACE_MS).
            node_urls=(BASE_ARCHIVE_NODE_HTTP_URI, BASE_FULL_NODE_HTTP_URI),
            pace_ms=BASE_PACE_MS,
            timeout_s=900,
        ),
        id="base",
    ),
)


@pytest.mark.live_drift
@pytest.mark.parametrize("chain", LIVE_CHAINS)
def test_recorder_check_matches_the_committed_corpus(
    chain: LiveChain,
    request: pytest.FixtureRequest,
) -> None:
    """--check over the chain's scenarios: disk corpus == a fresh recording."""
    selected = request.config.getoption("-m") or ""
    if "live_drift" not in selected:
        pytest.skip(_NOT_SELECTED_MSG)
    cargo = shutil.which("cargo")
    if cargo is None:
        pytest.skip("cargo is not on PATH; the recorder cannot run")
    command = [
        cargo,
        "run",
        "--locked",
        "--manifest-path",
        "rust/Cargo.toml",
        "-p",
        "degenbot",
        "--example",
        "record_py_oracle_corpus",
        "--",
    ]
    for scenario in chain.scenarios:
        command += ["--scenario", scenario]
    for url in chain.node_urls:
        command += [chain.node_flag, url]
    command += ["--pace-ms", str(chain.pace_ms)]
    proc = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] — trusted binary, args list, no shell
        command,
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=chain.timeout_s,
        check=False,
    )
    assert proc.returncode == 0, (
        f"recorder --check failed for {chain.name} (exit {proc.returncode}):\n"
        f"stdout tail: {proc.stdout[-3000:]}\n"
        f"stderr tail: {proc.stderr[-3000:]}"
    )
