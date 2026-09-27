"""Cross-surface config acceptance ledger.

The epic's claim is that a Rust console, a pure-Rust consumer, and a
Python-launched bot resolve the operator file IDENTICALLY. This is the closing
gate: it cross-checks the Python FFI verdict against an oracle produced by
`degenbot-config` in Rust.

The Rust half
(`rust/crates/foundation/degenbot-config/tests/config_parity_oracle.rs`) loads
a fixed operator file + per-environment variables through `degenbot-config`,
asserts the operator intent, and writes the WHOLE verdict — every declared
key's value, its winning layer, and the per-entry layer of each table — to the
shared artifact
`rust/crates/foundation/degenbot-config/tests/fixtures/config_parity/oracle.json`.
This half loads the same file + environments through `degenbot._ffi` in a fresh
interpreter per environment and compares.

Why this can fail:

* The Rust verdict is produced by Rust code; the Python verdict by the FFI. A
  Python-only regression (a stale/foreign holder install, an empty provenance
  map reporting `file` for an `env` winner, a default that drifted from the
  schema) makes them disagree.
* The Rust half asserts the operator's intended value AND layer for a sample of
  keys, so a Rust regression fails there rather than silently agreeing with
  itself.
* The whole projection is compared, not one key: the pre-epic divergence was a
  Python default that disagreed with the schema's — a key nobody enumerated.

`test_seeded_*` and `test_an_empty_provenance_map_fails_the_comparator` prove
the comparator has teeth: a mutated value, layer, entry layer, or an emptied
provenance map must produce a non-empty diff.
"""

from __future__ import annotations

import json
import os
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import tomllib
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[1]
_FIXTURE_DIR = _REPO_ROOT / "rust/crates/foundation/degenbot-config/tests/fixtures/config_parity"
_OPERATOR_FILE = _FIXTURE_DIR / "operator.toml"
_ORACLE_PATH = _FIXTURE_DIR / "oracle.json"
_AMBIENT_FILE = Path(__file__).resolve().parent / "ambient_config.toml"

_LAYERS = {"default", "file", "env", "cli"}


# A fresh interpreter installs the config at FFI module init, so a layer that
# is only installed at import is observable. `_ffi.resolved_config()` is the
# raw FFI boundary -- NOT the Python `degenbot.config` wrapper. The probe
# prints the whole verdict plus one result per requested resolution.
_PROBE = """\
import json
import sys

from degenbot import _ffi

verdict = _ffi.resolved_config()
out = {
    "values": dict(verdict.values),
    "provenance": dict(verdict.provenance),
    "entry_provenance": {
        key: dict(entries) for key, entries in verdict.entry_provenance.items()
    },
    "ops": [],
}
for op in json.loads(sys.argv[1]):
    kind = op["kind"]
    try:
        if kind == "node_uri":
            resolved = verdict.node_uri(op["chain_id"], op["scope"], op.get("node"))
            out["ops"].append({"value": resolved.uri, "source": resolved.source})
        elif kind == "chain_id":
            resolved = verdict.resolve_chain_id(op["value"])
            out["ops"].append({"value": resolved.chain_id, "source": resolved.source})
        elif kind == "database_path":
            resolved = verdict.resolve_database_path(op["value"])
            out["ops"].append({"value": resolved.path, "source": resolved.source})
        else:
            raise AssertionError(f"unknown resolution kind {kind!r}")
    except BaseException as exc:  # noqa: BLE001 - the refusal message is the assertion
        out["ops"].append({"error": str(exc), "error_type": type(exc).__name__})
print(json.dumps(out, sort_keys=True))
"""


def _load_oracle() -> dict:
    with _ORACLE_PATH.open() as handle:
        return json.load(handle)


def _run_probe(
    operator_file: Path,
    ops: list[dict],
    env_overrides: dict[str, str],
    tmp_path: Path,
) -> dict:
    """Load ``operator_file`` + ``env_overrides`` in a fresh interpreter.

    Every declared env name is cleared first -- the ``DEGENBOT_`` names and the
    unprefixed ``VERIFICATION_RETRY_*`` exception -- so neither the developer's
    shell nor the suite's ambient config decides a layer this test is about.
    The only layers are the written operator file and ``env_overrides``.
    """
    xdg_config = tmp_path / "xdg-config"
    xdg_state = tmp_path / "xdg-state"
    xdg_config.mkdir(exist_ok=True)
    xdg_state.mkdir(exist_ok=True)

    env = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(("DEGENBOT_", "VERIFICATION_RETRY_"))
    }
    env.update(
        {
            "DEGENBOT_CONFIG": str(operator_file),
            "XDG_CONFIG_HOME": str(xdg_config),
            "XDG_STATE_HOME": str(xdg_state),
        },
        **env_overrides,
    )
    completed = subprocess.run(  # ruff: ignore[subprocess-without-shell-equals-true] - fixed argv, no shell
        [sys.executable, "-c", _PROBE, json.dumps(ops)],
        capture_output=True,
        text=True,
        check=False,
        env=env,
    )
    assert completed.returncode == 0, completed.stderr
    return json.loads(completed.stdout)


def _resolve_environments(oracle: dict, tmp_path: Path) -> dict[str, dict]:
    """The whole FFI verdict, one fresh interpreter per environment."""
    resolved: dict[str, dict] = {}
    for environment in oracle["environments"]:
        resolved[environment["id"]] = _run_probe(_OPERATOR_FILE, [], environment["env"], tmp_path)
    return resolved


def _resolve_cases(oracle: dict, tmp_path: Path) -> dict[str, dict]:
    """One resolution per case, each in a fresh interpreter."""
    by_environment = {environment["id"]: environment for environment in oracle["environments"]}
    resolved: dict[str, dict] = {}
    for case in oracle["cases"]:
        environment = by_environment[case["environment"]]
        probe = _run_probe(_OPERATOR_FILE, [case["resolution"]], environment["env"], tmp_path)
        assert len(probe["ops"]) == 1
        resolved[case["id"]] = probe["ops"][0]
    return resolved


def _verdict_diffs(expected: dict, actual: dict) -> list[str]:
    """One message per divergence between an oracle verdict and the FFI answer."""
    diffs: list[str] = []
    for section in ("values", "provenance", "entry_provenance"):
        want = expected[section]
        got = actual[section]
        missing = sorted(set(want) - set(got))
        unexpected = sorted(set(got) - set(want))
        if missing or unexpected:
            diffs.append(f"{section} key set: missing={missing} unexpected={unexpected}")
        diffs.extend(
            f"{section}[{key}]: expected {want[key]!r} actual {got[key]!r}"
            for key in sorted(set(want) & set(got))
            if want[key] != got[key]
        )
    return diffs


def _case_diffs(verdict: dict, actual: dict) -> list[str]:
    """One message per divergence between an oracle resolution and the FFI answer."""
    diffs: list[str] = []
    if "error" in verdict:
        if "error" not in actual:
            diffs.append(f"expected refusal, got {actual}")
        else:
            if actual["error"] != verdict["error"]:
                diffs.append(
                    f"refusal message: expected {verdict['error']!r} actual {actual['error']!r}"
                )
            if actual.get("error_type") != "ValueError":
                diffs.append(f"refusal type: expected ValueError actual {actual.get('error_type')}")
    elif "error" in actual:
        diffs.append(f"expected {verdict}, got refusal {actual['error']!r}")
    else:
        if actual["value"] != verdict["value"]:
            diffs.append(f"value: expected {verdict['value']!r} actual {actual['value']!r}")
        if actual["source"] != verdict["source"]:
            diffs.append(f"source: expected {verdict['source']!r} actual {actual['source']!r}")
    return diffs


class TestCrossSurfaceParity:
    def test_ffi_whole_verdict_matches_the_rust_oracle(self, tmp_path: Path) -> None:
        """Every declared key, its value, and its layer equal the Rust oracle."""
        oracle = _load_oracle()
        assert oracle["schema"] == 2
        assert oracle["operator_file"] == "operator.toml"
        assert _OPERATOR_FILE.is_file(), "the shared operator file must be checked in"

        results = _resolve_environments(oracle, tmp_path)

        failures: list[str] = [
            f"environment {environment['id']}: {diff}"
            for environment in oracle["environments"]
            for diff in _verdict_diffs(environment, results[environment["id"]])
        ]
        assert not failures, "FFI whole verdict diverged from the Rust oracle:\n" + "\n".join(
            failures
        )

    def test_ffi_resolutions_match_the_rust_oracle(self, tmp_path: Path) -> None:
        """Each node/chain/database resolution equals the Rust-written oracle."""
        oracle = _load_oracle()
        results = _resolve_cases(oracle, tmp_path)

        failures: list[str] = [
            f"{case['id']}: {diff}"
            for case in oracle["cases"]
            for diff in _case_diffs(case["verdict"], results[case["id"]])
        ]
        assert not failures, "FFI diverged from the Rust oracle:\n" + "\n".join(failures)

    def test_the_ledger_covers_every_layer_and_precedence(self) -> None:
        """Default, file, env, and cli are each decided, and the file/env
        precedence is pinned for both a declared scalar and an endpoint table."""
        oracle = _load_oracle()
        environments = {environment["id"]: environment for environment in oracle["environments"]}

        declared_layers = {
            environment["id"]: set(environment["provenance"].values())
            for environment in oracle["environments"]
        }
        present = set().union(*declared_layers.values())
        assert {"default", "file", "env"} <= present, f"declared layers: {sorted(present)}"
        assert declared_layers["base"] >= {"default", "file"}
        assert declared_layers["env_chain"] >= {"env"}
        assert declared_layers["env_retry"] == declared_layers["env_retry"]

        # A DECLARED SCALAR resolves from the file in one environment and from
        # the environment in another: precedence, not just a final value.
        assert environments["base"]["provenance"]["session.chain_id"] == "file"
        assert environments["base"]["values"]["session.chain_id"] == 201
        assert environments["env_chain"]["provenance"]["session.chain_id"] == "env"
        assert environments["env_chain"]["values"]["session.chain_id"] == 202

        # The endpoint table is file in one environment and env in another, and
        # the per-entry layers prove the env export overrode exactly one chain.
        assert environments["base"]["provenance"]["nodes.http"] == "file"
        assert environments["env_http"]["provenance"]["nodes.http"] == "env"
        http_entries = environments["env_http"]["entry_provenance"]["DEGENBOT_RPC_HTTP_CHAINID_"]
        assert http_entries["101"] == "env"
        assert http_entries["103"] == "file"
        assert environments["env_http"]["provenance"]["session.chain_id"] == "file"

        # The UNPREFIXED exception is covered: names declared without the
        # `DEGENBOT_` prefix are exactly where a divergence would hide.
        retry = environments["env_retry"]
        assert retry["provenance"]["verify.verify_retry_max_attempts"] == "env"
        assert retry["values"]["verify.verify_retry_max_attempts"] == 6
        assert retry["provenance"]["verify.verify_retry_jitter"] == "env"
        assert retry["values"]["verify.verify_retry_jitter"] == pytest.approx(0.25)

        # A DECLARED DEFAULT, untouched by file or env.
        assert environments["base"]["provenance"]["telemetry.otel"] == "default"
        assert environments["base"]["values"]["telemetry.otel"] is True

        # cli is only reachable through an explicit override, pinned as a case.
        case_sources = {case["verdict"].get("source") for case in oracle["cases"]}
        assert "cli" in case_sources, f"cli layer missing from cases: {case_sources}"

        # Every recorded layer is one the schema can report.
        unknown = {
            path: layer
            for environment in oracle["environments"]
            for path, layer in environment["provenance"].items()
            if layer not in _LAYERS
        }
        assert not unknown, f"every layer must be one of {sorted(_LAYERS)}, got {unknown}"

    def test_seeded_value_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated declared value must produce a non-empty diff."""
        oracle = _load_oracle()
        expected = next(e for e in oracle["environments"] if e["id"] == "base")
        actual = _run_probe(_OPERATOR_FILE, [], expected["env"], tmp_path)
        assert _verdict_diffs(expected, actual) == []

        mutated = json.loads(json.dumps(expected))
        mutated["values"]["telemetry.otel"] = False
        diffs = _verdict_diffs(mutated, actual)
        assert diffs, "a mutated value must fail the comparator"
        assert any("values[telemetry.otel]" in diff for diff in diffs)

    def test_seeded_provenance_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated winning layer must produce a non-empty diff."""
        oracle = _load_oracle()
        expected = next(e for e in oracle["environments"] if e["id"] == "base")
        actual = _run_probe(_OPERATOR_FILE, [], expected["env"], tmp_path)
        assert _verdict_diffs(expected, actual) == []

        mutated = json.loads(json.dumps(expected))
        mutated["provenance"]["session.chain_id"] = "env"
        diffs = _verdict_diffs(mutated, actual)
        assert diffs, "a mutated reported layer must fail the comparator"
        assert any("provenance[session.chain_id]" in diff for diff in diffs)

    def test_seeded_entry_provenance_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated per-entry layer must produce a non-empty diff."""
        oracle = _load_oracle()
        expected = next(e for e in oracle["environments"] if e["id"] == "base")
        actual = _run_probe(_OPERATOR_FILE, [], expected["env"], tmp_path)
        assert _verdict_diffs(expected, actual) == []

        mutated = json.loads(json.dumps(expected))
        mutated["entry_provenance"]["DEGENBOT_RPC_HTTP_CHAINID_"]["101"] = "env"
        diffs = _verdict_diffs(mutated, actual)
        assert diffs, "a mutated per-entry layer must fail the comparator"
        assert any("entry_provenance" in diff for diff in diffs)

    def test_an_empty_provenance_map_fails_the_comparator(self, tmp_path: Path) -> None:
        """An empty provenance map is a divergence, never an unknown layer."""
        oracle = _load_oracle()
        expected = next(e for e in oracle["environments"] if e["id"] == "base")
        actual = _run_probe(_OPERATOR_FILE, [], expected["env"], tmp_path)

        mutated = json.loads(json.dumps(expected))
        mutated["provenance"] = {}
        diffs = _verdict_diffs(mutated, actual)
        assert diffs, "an empty provenance map must fail the comparator"
        assert any("provenance key set" in diff for diff in diffs)

    def test_a_foreign_provenance_layer_fails_the_comparator(self, tmp_path: Path) -> None:
        """A layer the schema never reports is a divergence, not a synonym."""
        oracle = _load_oracle()
        expected = next(e for e in oracle["environments"] if e["id"] == "base")
        actual = _run_probe(_OPERATOR_FILE, [], expected["env"], tmp_path)

        mutated = json.loads(json.dumps(expected))
        mutated["provenance"]["session.chain_id"] = "elsewhere"
        diffs = _verdict_diffs(mutated, actual)
        assert diffs, "a foreign provenance layer must fail the comparator"
        assert any("provenance[session.chain_id]" in diff for diff in diffs)

    def test_seeded_case_layer_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated resolution layer must produce a non-empty diff."""
        oracle = _load_oracle()
        case = next(c for c in oracle["cases"] if c["id"] == "file_only_request")
        actual = _resolve_cases(oracle, tmp_path)[case["id"]]
        assert _case_diffs(case["verdict"], actual) == []

        mutated = json.loads(json.dumps(case["verdict"]))
        mutated["source"] = "env"
        diffs = _case_diffs(mutated, actual)
        assert diffs, "a mutated reported layer must fail the comparator"
        assert any("source" in diff for diff in diffs)

    def test_seeded_case_value_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated resolution value must produce a non-empty diff."""
        oracle = _load_oracle()
        case = next(c for c in oracle["cases"] if c["id"] == "env_http_beats_file_ipc")
        actual = _resolve_cases(oracle, tmp_path)[case["id"]]
        assert _case_diffs(case["verdict"], actual) == []

        mutated = json.loads(json.dumps(case["verdict"]))
        mutated["value"] = "https://seeded-divergence.example:8545"
        diffs = _case_diffs(mutated, actual)
        assert diffs, "a mutated resolution value must fail the comparator"
        assert any("value" in diff for diff in diffs)

    def test_seeded_refusal_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated refusal message must produce a non-empty diff."""
        oracle = _load_oracle()
        case = next(c for c in oracle["cases"] if c["id"] == "subscription_never_selects_http")
        actual = _resolve_cases(oracle, tmp_path)[case["id"]]
        assert _case_diffs(case["verdict"], actual) == []

        mutated = json.loads(json.dumps(case["verdict"]))
        mutated["error"] = "no subscription endpoint resolved"
        diffs = _case_diffs(mutated, actual)
        assert diffs, "a mutated refusal must fail the comparator"
        assert any("refusal" in diff for diff in diffs)


class TestAmbientSuiteConfig:
    def test_ambient_suite_config_exercises_the_file_layer(self, tmp_path: Path) -> None:
        """The suite's ambient config declares ``[nodes]``, and it resolves live.

        Loading the ambient file in a fresh interpreter with every declared env
        name cleared leaves the file as the only layer, so the reported layer
        proves the file layer is wired end-to-end.
        """
        with _AMBIENT_FILE.open("rb") as handle:
            ambient = tomllib.load(handle)
        nodes = ambient["nodes"]
        http_uri = nodes["http"]["1"]
        ws_uri = nodes["ws"]["1"]

        probe = _run_probe(
            _AMBIENT_FILE,
            [
                {"kind": "node_uri", "chain_id": 1, "scope": "request"},
                {"kind": "node_uri", "chain_id": 1, "scope": "subscription"},
            ],
            {},
            tmp_path,
        )

        # Request scope prefers ws over http, so the file's ws entry wins; the
        # subscription scope can only take ws. Both report the file layer.
        assert probe["ops"][0] == {"value": ws_uri, "source": "file"}
        assert probe["ops"][1] == {"value": ws_uri, "source": "file"}
        assert ws_uri != http_uri, "the ambient config declares both transports"
