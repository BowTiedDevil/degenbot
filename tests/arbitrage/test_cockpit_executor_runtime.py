"""Explicit executor-runtime bytecode resolution.

The driver used to walk UP the filesystem for
``contracts/cmd_executor_runtime_bytecode.txt`` — an interface that only
works inside a source checkout and breaks wheel consumers. The decision:
the executor runtime becomes an explicit driver dependency.

Resolution order (first hit wins, NO walk):
  1. ``ArbitrageConfig.executor_runtime`` (operator-explicit path; the
     ``EXECUTOR_RUNTIME`` dotenv key)
  2. the declared ``dispatch.contracts_dir`` key (env
     ``DEGENBOT_CONTRACTS_DIR``) — one explicit directory
  3. exactly one computed candidate for the source layout (a fixed-depth
     hop from the module file — not an upward search)

No live RPC / no anvil: pure file + config behavior.
"""

from __future__ import annotations

import dataclasses

import pytest

from degenbot.runner import _dispatch as dispatch_module
from degenbot.runner._dispatch import _load_executor_runtime_bytecode
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides
from tests.helpers import verdict_probe as probe
from tests.helpers.identity_env import identity_env

FILE = "cmd_executor_runtime_bytecode.txt"


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


class TestExecutorRuntime:
    def test_config_defaults_to_none(self) -> None:
        assert _cfg().executor_runtime is None

    def test_config_carries_executor_runtime_from_env(self, tmp_path) -> None:
        p = tmp_path / "rt.txt"
        p.write_text("0x1234")
        assert _cfg(env={"EXECUTOR_RUNTIME": str(p)}).executor_runtime == str(p)

    def test_loader_reads_explicit_path(self, tmp_path) -> None:
        p = tmp_path / "rt.txt"
        p.write_text("0x1234abcd")
        cfg = _cfg(env={"EXECUTOR_RUNTIME": str(p)})
        assert _load_executor_runtime_bytecode(cfg) == "0x1234abcd"

    def test_missing_explicit_file_raises_named_error(self, tmp_path) -> None:
        cfg = _cfg(env={"EXECUTOR_RUNTIME": str(tmp_path / "missing.txt")})
        with pytest.raises(RuntimeError, match="executor_runtime"):
            _load_executor_runtime_bytecode(cfg)

    def test_the_contracts_dir_key_provides_the_file(self, tmp_path) -> None:
        (tmp_path / FILE).write_text("0xabcd")

        cfg = probe.build_config(env={"DEGENBOT_CONTRACTS_DIR": str(tmp_path)})

        assert _load_executor_runtime_bytecode(cfg) == "0xabcd"

    def test_a_contracts_dir_without_the_file_raises(self, tmp_path) -> None:
        cfg = probe.build_config(env={"DEGENBOT_CONTRACTS_DIR": str(tmp_path)})

        with pytest.raises(RuntimeError, match="DEGENBOT_CONTRACTS_DIR"):
            _load_executor_runtime_bytecode(cfg)

    def test_resolution_prefers_explicit_keys_over_the_computed_candidate(self, tmp_path) -> None:
        """The explicit keys win; the computed candidate is the last resort.

        Resolution is explicit-path-first, so an operator-set ``executor_runtime``
        or ``dispatch.contracts_dir`` decides the path outright — the computed
        source-layout candidate is only reached when neither is set.
        """
        d = dispatch_module

        explicit_file = tmp_path / "explicit.txt"
        contracts_dir = tmp_path / "contracts"
        contracts_dir.mkdir()

        base = probe.build_config(env={"DEGENBOT_CONTRACTS_DIR": str(contracts_dir)})

        # 1. The operator's explicit path wins outright.
        operator_path = dataclasses.replace(base, executor_runtime=explicit_file)
        assert d._resolve_executor_runtime_path(operator_path) == explicit_file

        # 2. No explicit path: the declared contracts_dir, with the fixed filename.
        assert d._resolve_executor_runtime_path(base) == (contracts_dir / d._EXECUTOR_RUNTIME_FILE)

        # 3. Neither key set: the fixed-depth source-layout candidate, which lands
        #    in the checkout's contracts/ directory.
        bare = dataclasses.replace(base, contracts_dir="")
        resolved = d._resolve_executor_runtime_path(bare)
        assert resolved.parent.name == "contracts"
        assert resolved.name == d._EXECUTOR_RUNTIME_FILE
