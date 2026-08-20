"""Explicit executor-runtime bytecode resolution (epic Y7PA5A, task RI6XCY).

The driver used to walk UP the filesystem for
``contracts/cmd_executor_runtime_bytecode.txt`` — an interface that only
works inside a source checkout and breaks wheel consumers. The decision:
the executor runtime becomes an explicit driver dependency.

Resolution order (first hit wins, NO walk):
  1. ``ArbitrageConfig.executor_runtime`` (operator-explicit path; env
     ``EXECUTOR_RUNTIME`` in ``from_env``)
  2. ``$DEGENBOT_CONTRACTS_DIR`` — one explicit directory
  3. exactly one computed candidate for the source layout (a fixed-depth
     hop from the module file — not an upward search)

No live RPC / no anvil: pure file + config behavior.
"""

from __future__ import annotations

import inspect

import pytest

from degenbot.runner._dispatch import _load_executor_runtime_bytecode
from degenbot.runner.config import ArbitrageConfig

FILE = "cmd_executor_runtime_bytecode.txt"


@pytest.fixture(autouse=True)
def _no_contracts_dir(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("DEGENBOT_CONTRACTS_DIR", raising=False)


def _cfg(env: dict[str, str] | None = None) -> ArbitrageConfig:
    base: dict[str, str] = {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "11" * 32,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
    }
    base.update(env or {})
    return ArbitrageConfig.from_env(
        base,
        live=False,
        permutation=None,
        cli_http="http://localhost:8545",
        cli_ws="ws://localhost:8546",
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

    def test_env_dir_provides_the_file(self, tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("DEGENBOT_CONTRACTS_DIR", str(tmp_path))
        (tmp_path / FILE).write_text("0xabcd")
        assert _load_executor_runtime_bytecode(_cfg()) == "0xabcd"

    def test_env_dir_without_file_raises(self, tmp_path, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("DEGENBOT_CONTRACTS_DIR", str(tmp_path))
        with pytest.raises(RuntimeError, match="DEGENBOT_CONTRACTS_DIR"):
            _load_executor_runtime_bytecode(_cfg())

    def test_no_upward_walk_in_resolution(self) -> None:
        """The walk is gone: resolution is explicit paths, no directory search."""
        from degenbot.runner import _dispatch as d

        src = inspect.getsource(d._resolve_executor_runtime_path)
        assert "for candidate" not in src, "upward walk must be gone"
