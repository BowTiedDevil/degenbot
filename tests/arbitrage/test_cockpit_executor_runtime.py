"""The cockpit's executor-runtime artifact boundary.

The engine owns the executor-runtime artifact: resolution precedence (an
explicit path, then the declared ``dispatch.contracts_dir`` key, then the
source-layout candidate a checkout carries), the file read, and the 0x-hex
validation all live in ``degenbot-arbitrage``. Those rules are pinned by that
crate's unit tests; this file pins the Python side of the seam:

- the driver config still carries the two operator values
  (``executor_runtime`` from ``EXECUTOR_RUNTIME``, ``contracts_dir`` from the
  declared ``dispatch.contracts_dir`` key), and
- the FFI boundary hands them to the engine unchanged — a bad path refuses at
  construction with the engine's own message, which names the file it tried.

No live RPC / no anvil: pure file + config behavior.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from tests.helpers import verdict_probe as probe
from tests.helpers.identity_env import identity_env
from tests.helpers.seam import AlloyProvider, AsyncAlloyProvider, SimulateContext

FILE = "cmd_executor_runtime_bytecode.txt"

# Canonical mainnet addresses (parity corpus constants).
OWNER = "0x9c56a29c7231974c269e24f9fb3c29203039089e"
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"
MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"

# A dead port: alloy's transport is lazy, so construction dials nothing.
_RPC_URL = "http://127.0.0.1:1"


def _cfg(env: dict[str, str] | None = None) -> ArbitrageConfig:
    base: dict[str, str] = {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "11" * 32,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
    }
    base.update(env or {})
    with identity_env(base):
        return ArbitrageConfig.build(
            live=False,
            permutation=None,
            rpc=RpcCascadeOverrides(node="ws://localhost:8546"),
        )


def _ctx(*, executor_runtime_path: str | None = None, contracts_dir: str | None = None):
    """Build a sim context over the two operator values the shell carries."""
    return SimulateContext(
        provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        executor_owner=OWNER,
        executor_address=EXECUTOR,
        weth_address=WETH,
        pool_manager_address=POOL_MANAGER,
        multicall3_address=MULTICALL3,
        inject_code=False,
        executor_runtime_path=executor_runtime_path,
        contracts_dir=contracts_dir,
    )


class TestExecutorRuntimeConfig:
    """The driver config carries the operator values; the engine resolves them."""

    def test_config_defaults_to_none(self) -> None:
        assert _cfg().executor_runtime is None

    def test_config_carries_executor_runtime_from_env(self, tmp_path) -> None:
        p = tmp_path / "rt.txt"
        p.write_text("0x1234")
        assert _cfg(env={"EXECUTOR_RUNTIME": str(p)}).executor_runtime == str(p)

    def test_config_carries_the_contracts_dir_key(self, tmp_path) -> None:
        # The declared key resolves through the config cascade, so the probe
        # installs it as the env layer rather than mutating the process verdict.
        cfg = probe.build_config(env={"DEGENBOT_CONTRACTS_DIR": str(tmp_path)})

        assert cfg.contracts_dir == str(tmp_path)


class TestTheEngineLoadsTheArtifact:
    """The boundary hands the engine the path; the engine reads + validates."""

    def test_an_explicit_artifact_path_is_loaded(self, tmp_path) -> None:
        artifact = tmp_path / "rt.txt"
        artifact.write_text("0xdeadbeef")

        assert isinstance(_ctx(executor_runtime_path=str(artifact)), SimulateContext)

    def test_a_missing_explicit_artifact_names_the_file(self, tmp_path) -> None:
        missing = tmp_path / "missing.txt"

        with pytest.raises(RuntimeError, match="executor runtime bytecode not found") as excinfo:
            _ctx(executor_runtime_path=str(missing))

        message = str(excinfo.value)
        assert str(missing) in message
        assert "dispatch.contracts_dir" in message

    def test_the_contracts_dir_key_supplies_the_artifact(self, tmp_path) -> None:
        (tmp_path / FILE).write_text("0xabcd")

        assert isinstance(_ctx(contracts_dir=str(tmp_path)), SimulateContext)

    def test_a_malformed_artifact_in_the_contracts_dir_names_that_file(self, tmp_path) -> None:
        """The declared directory wins over the checkout candidate — proof by error.

        A body the engine cannot decode refuses with the path it read, which is
        the declared directory's canonical filename and not the source-layout
        candidate a checkout carries.
        """
        declared = tmp_path / FILE
        declared.write_text("not-hex")

        with pytest.raises(RuntimeError, match="not valid 0x-hex text") as excinfo:
            _ctx(contracts_dir=str(tmp_path))

        assert str(declared) in str(excinfo.value)

    def test_an_unset_path_falls_through_to_the_checkout_candidate(self) -> None:
        """Neither key set: the engine's source-layout candidate resolves.

        The candidate is the committed artifact in this checkout, so the
        default configuration boots without any operator input.
        """
        assert Path(__file__).resolve().parents[2].joinpath("contracts", FILE).exists()
        assert isinstance(_ctx(), SimulateContext)
