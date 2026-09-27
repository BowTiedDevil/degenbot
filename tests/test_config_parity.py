"""Cross-surface config acceptance ledger (task BZ6W4Y).

The epic's claim is that a Rust console, a pure-Rust consumer, and a
Python-launched bot resolve the operator file IDENTICALLY. This is the
closing gate: it cross-checks the Python FFI resolver against an oracle
produced by `degenbot-config` in Rust.

The Rust half
(`rust/crates/foundation/degenbot-config/tests/config_parity_oracle.rs`)
resolves a fixed operator file + per-case environment through
`degenbot-config`, asserts the operator intent, and writes the verdicts
(uri + reported layer, or the refusal) to the shared artifact
`rust/crates/foundation/degenbot-config/tests/fixtures/config_parity/oracle.json`.
This half resolves the same cases through `degenbot._ffi` in a fresh
interpreter whose only layers are that file and the recorded environment,
and compares.

Why this can fail:

* The Rust verdict is produced by Rust code; the Python verdict by the FFI.
  A Python-only regression (a stale/foreign holder install, an empty
  provenance map reporting `file` for an `env` winner) makes them disagree.
* The Rust half asserts the operator's intended uri AND layer, so a Rust
  regression fails there rather than silently agreeing with itself.
* The reported LAYER is compared, not only the uri, so an unknown-layer
  provenance map cannot pass by reporting `file`.

`test_seeded_*` proves the comparator has teeth: a mutated oracle layer or
uri must produce a non-empty diff.
"""

from __future__ import annotations

import json
import os
import subprocess  # ruff: ignore[suspicious-subprocess-import]
import sys
import tomllib
from pathlib import Path

_REPO_ROOT = Path(__file__).resolve().parents[1]
_FIXTURE_DIR = _REPO_ROOT / "rust/crates/foundation/degenbot-config/tests/fixtures/config_parity"
_OPERATOR_FILE = _FIXTURE_DIR / "operator.toml"
_ORACLE_PATH = _FIXTURE_DIR / "oracle.json"
_AMBIENT_FILE = Path(__file__).resolve().parent / "ambient_config.toml"


# A fresh interpreter installs the config at FFI module init, so a layer that
# is only installed at import is observable. `ResolvedConfig.node_uri` is the raw
# FFI boundary -- NOT the Python `resolve_node` wrapper, which is a one-line
# delegation to this exact method.
_PROBE = """\
import json
import sys

from degenbot import _ffi

results = []
for chain_id, scope in json.loads(sys.argv[1]):
    try:
        resolved = _ffi.resolved_config().node_uri(chain_id, scope)
        results.append({"uri": resolved.uri, "source": resolved.source})
    except BaseException as exc:  # noqa: BLE001 - the refusal message is the assertion
        results.append({"error": str(exc), "error_type": type(exc).__name__})
print(json.dumps(results))
"""


def _load_oracle() -> dict:
    with _ORACLE_PATH.open() as handle:
        return json.load(handle)


def _run_probe(
    operator_file: Path,
    ops: list[list[object]],
    env_overrides: dict[str, str],
    tmp_path: Path,
) -> list[dict]:
    """Resolve ``ops`` in a fresh interpreter pinned to ``operator_file`` + env.

    Every ``DEGENBOT_*`` name is cleared first, so neither the developer's
    shell nor the suite's ambient config decides a layer this test is about;
    the only layers are the written operator file and ``env_overrides``.
    """
    xdg_config = tmp_path / "xdg-config"
    xdg_state = tmp_path / "xdg-state"
    xdg_config.mkdir(exist_ok=True)
    xdg_state.mkdir(exist_ok=True)

    env = {name: value for name, value in os.environ.items() if not name.startswith("DEGENBOT_")}
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
    parsed: list[dict] = json.loads(completed.stdout)
    assert len(parsed) == len(ops)
    return parsed


def _resolve_cases(operator_file: Path, cases: list[dict], tmp_path: Path) -> dict[str, dict]:
    """Resolve every case, one interpreter per distinct environment.

    The cascade is installed at import, so the environment belongs to the
    interpreter; cases that share an environment share a probe.
    """
    groups: dict[tuple[tuple[str, str], ...], list[dict]] = {}
    for case in cases:
        key = tuple(sorted(case["env"].items()))
        groups.setdefault(key, []).append(case)

    resolved: dict[str, dict] = {}
    for key, group in groups.items():
        ops = [[case["chain_id"], case["scope"]] for case in group]
        raw = _run_probe(operator_file, ops, dict(key), tmp_path)
        for case, actual in zip(group, raw, strict=True):
            resolved[case["id"]] = actual
    return resolved


def _diffs(verdict: dict, actual: dict) -> list[str]:
    """One message per divergence between an oracle verdict and the FFI answer."""
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
        if actual["uri"] != verdict["uri"]:
            diffs.append(f"uri: expected {verdict['uri']!r} actual {actual['uri']!r}")
        if actual["source"] != verdict["source"]:
            diffs.append(f"source: expected {verdict['source']!r} actual {actual['source']!r}")
    return diffs


class TestCrossSurfaceParity:
    def test_ffi_matches_the_rust_oracle(self, tmp_path: Path) -> None:
        """The FFI verdict equals the Rust-written oracle for every case."""
        oracle = _load_oracle()
        assert oracle["operator_file"] == "operator.toml"
        assert _OPERATOR_FILE.is_file(), "the shared operator file must be checked in"

        results = _resolve_cases(_OPERATOR_FILE, oracle["cases"], tmp_path)

        failures: list[str] = [
            f"{case['id']}: {diff}"
            for case in oracle["cases"]
            for diff in _diffs(case["verdict"], results[case["id"]])
        ]
        assert not failures, "FFI diverged from the Rust oracle:\n" + "\n".join(failures)

    def test_ledger_pins_the_required_cases_and_reported_layers(self) -> None:
        """The ledger covers file-only, env-beats-file, scope, and the layer."""
        cases = {case["id"]: case for case in _load_oracle()["cases"]}
        assert {
            "file_only_request",
            "env_http_beats_file_ipc",
            "file_ipc_preferred_request",
            "file_ws_subscription",
            "subscription_never_selects_http",
        } <= set(cases)

        # FILE LAYER ONLY: no env, and the layer reported is the file.
        assert cases["file_only_request"]["env"] == {}
        assert cases["file_only_request"]["verdict"] == {
            "uri": "http://file-101.example:8545",
            "source": "file",
        }

        # ENV BEATS FILE: an env http beats a file ipc, and the layer is env.
        assert cases["env_http_beats_file_ipc"]["verdict"] == {
            "uri": "https://env-102.example:8545",
            "source": "env",
        }

        # SCOPE: a file http alone cannot satisfy a subscription; the refusal
        # names the http entry it declined to select.
        refusal = cases["subscription_never_selects_http"]["verdict"]
        assert "error" in refusal
        assert "nodes.http" in refusal["error"]
        assert "subscription" in refusal["error"]

        # THE REPORTED LAYER, not only the value: the file/ws subscription
        # reports the file layer.
        assert cases["file_ws_subscription"]["verdict"]["source"] == "file"

    def test_seeded_layer_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated layer must produce a non-empty diff."""
        oracle = _load_oracle()
        case = next(c for c in oracle["cases"] if c["id"] == "file_only_request")
        actual = _resolve_cases(_OPERATOR_FILE, [case], tmp_path)[case["id"]]
        assert _diffs(case["verdict"], actual) == []

        mutated = json.loads(json.dumps(case["verdict"]))
        mutated["source"] = "env"
        diffs = _diffs(mutated, actual)
        assert diffs, "a mutated reported layer must fail the comparator"
        assert any("source" in diff for diff in diffs)

    def test_seeded_uri_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated endpoint must produce a non-empty diff."""
        oracle = _load_oracle()
        case = next(c for c in oracle["cases"] if c["id"] == "env_http_beats_file_ipc")
        actual = _resolve_cases(_OPERATOR_FILE, [case], tmp_path)[case["id"]]
        assert _diffs(case["verdict"], actual) == []

        mutated = json.loads(json.dumps(case["verdict"]))
        mutated["uri"] = "https://seeded-divergence.example:8545"
        diffs = _diffs(mutated, actual)
        assert diffs, "a mutated endpoint must fail the comparator"
        assert any("uri" in diff for diff in diffs)

    def test_seeded_refusal_mutation_fails_the_comparator(self, tmp_path: Path) -> None:
        """Teeth proof: a mutated refusal message must produce a non-empty diff."""
        oracle = _load_oracle()
        case = next(c for c in oracle["cases"] if c["id"] == "subscription_never_selects_http")
        actual = _resolve_cases(_OPERATOR_FILE, [case], tmp_path)[case["id"]]
        assert _diffs(case["verdict"], actual) == []

        mutated = json.loads(json.dumps(case["verdict"]))
        mutated["error"] = "no subscription endpoint resolved"
        diffs = _diffs(mutated, actual)
        assert diffs, "a mutated refusal must fail the comparator"
        assert any("refusal" in diff for diff in diffs)


class TestAmbientSuiteConfig:
    def test_ambient_suite_config_exercises_the_file_layer(self, tmp_path: Path) -> None:
        """The suite's ambient config declares ``[nodes]``, and it resolves live.

        Loading the ambient file in a fresh interpreter with every
        ``DEGENBOT_*`` name cleared leaves the file as the only layer, so the
        reported layer proves the file layer is wired end-to-end.
        """
        with _AMBIENT_FILE.open("rb") as handle:
            ambient = tomllib.load(handle)
        nodes = ambient["nodes"]
        http_uri = nodes["http"]["1"]
        ws_uri = nodes["ws"]["1"]

        request, subscription = _run_probe(
            _AMBIENT_FILE,
            [[1, "request"], [1, "subscription"]],
            {},
            tmp_path,
        )

        # Request scope prefers ws over http, so the file's ws entry wins; the
        # subscription scope can only take ws. Both report the file layer.
        assert request == {"uri": ws_uri, "source": "file"}
        assert subscription == {"uri": ws_uri, "source": "file"}
        assert ws_uri != http_uri, "the ambient config declares both transports"
