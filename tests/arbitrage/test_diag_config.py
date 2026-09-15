"""C6: diagnostic harnesses move from the example entrypoint to the driver
cockpit (degenbot.runner.diag), configured through the typed loader
(``ArbitrageConfig.from_env`` — the only env-reading site, KAHU5W).

The example used to read ``DEGENBOT_TRACEMALLOC_SECS`` /
``DEGENBOT_PROCMEM_SECS`` / ``DEGENBOT_PROCMEM_CSV`` /
``DEGENBOT_FAULTHANDLER_TIMEOUT_SECS`` with raw ``os.environ.get`` calls.
"""

from __future__ import annotations

import pytest

from degenbot.runner.config import ArbitrageConfig
from degenbot.runner.diag import DiagConfig, arm_diagnostics


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "https://eth.example.com")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "wss://ws.eth.example.com")


def _full_env() -> dict[str, str]:
    return {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
        "INJECTED_EXECUTOR_ADDRESS": "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
        "EXECUTOR_OWNER_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
    }


class TestDiagConfigFromEnv:
    """``ArbitrageConfig.from_env`` resolves the diag knobs off the env mapping."""

    def test_defaults_all_off(self) -> None:
        cfg = ArbitrageConfig.from_env(_full_env(), live=False, permutation=None)
        assert cfg.diag == DiagConfig()

    def test_env_keys_resolved(self) -> None:
        env = _full_env() | {
            "DEGENBOT_TRACEMALLOC_SECS": "30",
            "DEGENBOT_PROCMEM_SECS": "5",
            "DEGENBOT_PROCMEM_CSV": "custom/procmem.csv",
            "DEGENBOT_FAULTHANDLER_TIMEOUT_SECS": "60",
        }
        cfg = ArbitrageConfig.from_env(env, live=False, permutation=None)
        assert cfg.diag.tracemalloc_secs == pytest.approx(30.0)
        assert cfg.diag.procmem_secs == pytest.approx(5.0)
        assert cfg.diag.procmem_csv == "custom/procmem.csv"
        assert cfg.diag.faulthandler_timeout_secs == pytest.approx(60.0)

    def test_non_numeric_value_raises(self) -> None:
        env = _full_env() | {"DEGENBOT_TRACEMALLOC_SECS": "banana"}
        with pytest.raises(ValueError, match="DEGENBOT_TRACEMALLOC_SECS"):
            ArbitrageConfig.from_env(env, live=False, permutation=None)


class TestArmDiagnostics:
    """``arm_diagnostics`` arms only configured probes, each exactly once."""

    def test_zero_config_arms_nothing(self) -> None:
        assert arm_diagnostics(DiagConfig()) == []

    def test_detached_probes_start(self, tmp_path) -> None:
        cfg = DiagConfig(
            tracemalloc_secs=3600,
            procmem_secs=3600,
            procmem_csv=str(tmp_path / "procmem.csv"),
            faulthandler_timeout_secs=3600,
        )
        armed = arm_diagnostics(cfg)
        assert armed == ["tracemalloc", "procmem", "faulthandler"]
