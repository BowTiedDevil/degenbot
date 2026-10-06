"""The incident probes are declared keys, configured through the verdict.

Each of ``DEGENBOT_TRACEMALLOC_SECS`` / ``DEGENBOT_PROCMEM_SECS`` /
``DEGENBOT_PROCMEM_CSV`` / ``DEGENBOT_FAULTHANDLER_TIMEOUT_SECS`` is a
``config_schema!`` key now. They used to be read off the example dotenv
mapping alone, so an OS export never reached them and the defaults were
written down twice — beside the reader and in the schema. One
declaration answers for both, and the operator file reaches the probes as
readily as the environment.
"""

from __future__ import annotations

import os
import time

import pytest

from degenbot.runner.diag import DiagConfig, arm_diagnostics
from tests.helpers import verdict_probe as probe
from tests.helpers.rpc_env import rpc_env


@pytest.fixture(autouse=True)
def _rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    rpc_env(monkeypatch)


def _full_env() -> dict[str, str]:
    return {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECTED_EXECUTOR_ADDRESS": "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
        "EXECUTOR_OWNER_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
    }


#: The four probe fields, as the config names them.
_DIAG_FIELDS = tuple(
    f"diag.{name}"
    for name in (
        "tracemalloc_secs",
        "procmem_secs",
        "procmem_csv",
        "faulthandler_timeout_secs",
    )
)

#: Every probe declared off, spelled out because ``DiagConfig`` carries no
#: defaults of its own: the schema declaration owns them.
_PROBES_OFF = {
    "tracemalloc_secs": 0.0,
    "procmem_secs": 0.0,
    "procmem_csv": "logs/procmem.csv",
    "faulthandler_timeout_secs": 0.0,
}


class TestDiagConfigFromEnv:
    """The probe intervals are declared keys, so every layer reaches them."""

    def test_the_declared_defaults_arm_nothing(self) -> None:
        values = probe.config_values(_DIAG_FIELDS)

        assert values == {f"diag.{name}": value for name, value in _PROBES_OFF.items()}

    def test_the_env_layer_reaches_every_probe(self) -> None:
        values = probe.config_values(
            _DIAG_FIELDS,
            env={
                "DEGENBOT_TRACEMALLOC_SECS": "30",
                "DEGENBOT_PROCMEM_SECS": "5",
                "DEGENBOT_PROCMEM_CSV": "custom/procmem.csv",
                "DEGENBOT_FAULTHANDLER_TIMEOUT_SECS": "60",
            },
        )

        assert values["diag.tracemalloc_secs"] == pytest.approx(30.0)
        assert values["diag.procmem_secs"] == pytest.approx(5.0)
        assert values["diag.procmem_csv"] == "custom/procmem.csv"
        assert values["diag.faulthandler_timeout_secs"] == pytest.approx(60.0)

    def test_the_file_layer_reaches_every_probe(self) -> None:
        """Reach the old dotenv-only chain could not offer."""

        body = """[diagnostics]
tracemalloc_secs = 30.0
procmem_secs = 5.0
procmem_csv = 'custom/procmem.csv'
faulthandler_timeout_secs = 60.0
 """
        with probe.operator_file(body) as written:
            values = probe.config_values(_DIAG_FIELDS, operator_file=written)

        assert values["diag.tracemalloc_secs"] == pytest.approx(30.0)
        assert values["diag.procmem_secs"] == pytest.approx(5.0)
        assert values["diag.procmem_csv"] == "custom/procmem.csv"
        assert values["diag.faulthandler_timeout_secs"] == pytest.approx(60.0)

    def test_a_non_numeric_interval_is_refused_at_boot(self) -> None:
        """A typo'd interval is a loud refusal, not a silent default.

        The loader owns the parse now, so the refusal is the process refusing
        to start at all.

        """

        # Process-level: the refusal is the process exit code at boot.
        completed = probe.run("import degenbot", env={"DEGENBOT_TRACEMALLOC_SECS": "banana"})

        assert completed.returncode == 2, completed.stderr
        assert "diagnostics.tracemalloc_secs" in completed.stderr, completed.stderr
        assert "banana" in completed.stderr, completed.stderr


class TestArmDiagnostics:
    """``arm_diagnostics`` arms only configured probes, each exactly once."""

    def test_zero_config_arms_nothing(self) -> None:
        assert arm_diagnostics(DiagConfig(**_PROBES_OFF)) == []

    def test_detached_probes_start(self, tmp_path) -> None:
        cfg = DiagConfig(
            tracemalloc_secs=3600,
            procmem_secs=3600,
            procmem_csv=str(tmp_path / "procmem.csv"),
            faulthandler_timeout_secs=3600,
        )
        armed = arm_diagnostics(cfg)
        assert armed == ["tracemalloc", "procmem", "faulthandler"]

    def test_procmem_sampler_parses_the_injected_proc_root(self, tmp_path) -> None:
        """The sampler reads ``proc_root`` — a fixture directory shaped like
        ``/proc/self`` — so the CSV row's parsed fields are assertable without
        a fresh interpreter.

        ``stat``'s post-parenthesis fields 10 and 12 (min_flt, maj_flt),
        ``statm``'s resident word, and ``status``'s ``VmHWM`` line each land
        in the row under their own name.

        """
        proc_root = tmp_path / "proc"
        proc_root.mkdir()
        (proc_root / "stat").write_bytes(b"1 (python) R 0 1 1 1 1 4194304 42 7 99 0")
        (proc_root / "statm").write_bytes(b"1000 200 100 50 0 100 0")
        (proc_root / "status").write_bytes(b"VmHWM:\t1234 kB\n")
        csv_path = tmp_path / "procmem.csv"

        cfg = DiagConfig(
            tracemalloc_secs=0.0,
            procmem_secs=0.01,
            procmem_csv=str(csv_path),
            faulthandler_timeout_secs=0.0,
        )
        armed = arm_diagnostics(cfg, proc_root=proc_root)
        assert armed == ["procmem"]

        deadline = time.monotonic() + 5.0
        rows: list[str] = []
        while time.monotonic() < deadline:
            rows = csv_path.read_text(encoding="utf-8").splitlines() if csv_path.exists() else []
            if len(rows) >= 2:
                break
            time.sleep(0.01)
        assert len(rows) >= 2, f"sampler wrote no CSV row in time: {rows!r}"

        header = rows[0].split(",")
        assert header == ["t_epoch", "t_mono", "rss_kb", "hwm_kb", "min_flt", "maj_flt"]
        row = rows[1].split(",")
        assert int(row[2]) == 200 * (os.sysconf("SC_PAGE_SIZE") // 1024)
        assert int(row[3]) == 1234
        assert int(row[4]) == 42
        assert int(row[5]) == 99
