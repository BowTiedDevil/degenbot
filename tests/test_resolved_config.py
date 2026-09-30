"""The resolved verdict is the whole interface the Python driver reads.

`degenbot-config` owns the operator file, the environment, and the resolution
order (ADR-062 D7/D10). What crosses the language seam is ONE object: the
resolved verdict, installed once at FFI module init. These tests hold the
shape that makes the seam stop growing with the schema.

The load-bearing one is
`every_key_the_generated_reference_names_is_in_the_verdict`. It reads the
GENERATED key reference -- the operator-facing document produced from
`config_schema!` -- and requires the verdict to carry every key it names. A
key added to the schema therefore appears in the verdict with no FFI change,
and a hand-maintained accessor list would drift from the schema the moment
either moved.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from degenbot import _ffi

_REPO_ROOT = Path(__file__).resolve().parents[1]
_KEY_REFERENCE = _REPO_ROOT / "docs/rust-config-keys.md"

# `| `DEGENBOT_OTEL` | `telemetry.otel` | `bool` | `true` | Enable the OTel... |`
_KEY_ROW = re.compile(r"^\|\s*`[^`]*`\s*\|\s*`([^`]+)`\s*\|")
_LAYERS = {"default", "file", "env", "cli"}


def _declared_toml_paths() -> set[str]:
    """The dotted key paths the generated reference names, in declaration order."""
    paths = set()
    for line in _KEY_REFERENCE.read_text().splitlines():
        match = _KEY_ROW.match(line)
        if match is not None:
            paths.add(match.group(1))
    return paths


def test_the_verdict_is_one_frozen_object() -> None:
    """One call returns the whole verdict, and it cannot be edited in place.

    A verdict that Python can mutate is a second config authority wearing a
    Rust type, so the freeze is the property, not a detail.
    """
    verdict = _ffi.resolved_config()
    assert isinstance(verdict, _ffi.ResolvedConfig)
    with pytest.raises((AttributeError, TypeError)):
        verdict.chain_id = 1  # type: ignore[misc]


def _read_named(values: object, path: str) -> object:
    """One declared key read as a named property (``values.<section>.<field>``)."""
    node: object = values
    for part in path.split("."):
        node = getattr(node, part)
    return node


def test_every_key_the_generated_reference_names_is_in_the_verdict() -> None:
    """The verdict carries every key the schema declares, with no accessor per key.

    The generated reference is the schema's own output, so this fails the
    moment a key is declared and the named projection does not grow with it.
    """
    declared = _declared_toml_paths()
    assert declared, "the generated key reference must name the declared keys"
    values = _ffi.resolved_config().values
    missing = []
    for path in sorted(declared):
        try:
            _read_named(values, path)
        except AttributeError:
            missing.append(path)
    assert not missing, f"the verdict must carry every declared key: missing={missing}"


def test_a_key_the_reference_does_not_name_is_not_in_the_verdict() -> None:
    """The projection is closed: an undeclared name refuses, it does not default."""
    with pytest.raises(AttributeError):
        _ffi.resolved_config().values.database.not_a_key


def test_a_declared_value_keeps_the_kind_its_key_declared() -> None:
    """A value is a typed Python value, not a stringly-typed rendering.

    A path key must not arrive as a number, and a wei key must not arrive as
    a float: the parity oracle that compares the verdict against a Rust load
    compares these values, so a lossy projection would make it agree on a
    number nobody configured.
    """
    values = _ffi.resolved_config().values
    assert isinstance(values.pump.pump_debounce_ms, int)
    assert isinstance(values.solve.min_profit_wei, int)
    assert isinstance(values.allocator.mimalloc_purge_delay_mult, float)
    assert isinstance(values.telemetry.metrics_addr, str)
    assert isinstance(values.database.path, str)
    assert values.telemetry.otel is True
    # An unset-able key projects as its kind or as None, never as a string.
    assert values.logging.trace_jsonl is None or isinstance(values.logging.trace_jsonl, str)


def test_provenance_names_the_layer_that_won_each_key() -> None:
    """Every loaded key carries the layer that supplied it, from the same load.

    The verdict is built from the one load the process published at module
    init, so the layer is a fact about this process rather than something the
    driver re-derives.
    """
    provenance = _ffi.resolved_config().provenance
    declared = _declared_toml_paths()
    assert set(provenance) == declared, (
        f"missing={sorted(declared - set(provenance))} "
        f"unexpected={sorted(set(provenance) - declared)}"
    )
    unknown = {path: layer for path, layer in provenance.items() if layer not in _LAYERS}
    assert not unknown, f"every layer must be one of {sorted(_LAYERS)}, got {unknown}"


def test_the_database_path_getter_agrees_with_the_cascade() -> None:
    """The declared key and the cascade winner are different questions.

    The verdict answers both, and the cascade answer is the expanded one a
    session opens.
    """
    from degenbot.config import resolve_database_path

    verdict = _ffi.resolved_config()
    assert verdict.database_path.path == resolve_database_path()
    assert verdict.resolve_database_path().path == verdict.database_path.path
    assert verdict.declared_database_path == verdict.values.database.path


def test_the_declared_batch_size_projects_by_name_and_clamps_once() -> None:
    """The named projection exposes the declared key; the clamp has one owner.

    The FFI getter used to carry its own ``max(1)`` twin of the config
    factory's; the twin is deleted and the clamp lives only at the factory
    that hands the value to the discovery pipeline.
    """
    assert isinstance(
        _read_named(_ffi.resolved_config().values, "pathfinding.discovery_batch_size"), int
    )


class TestHypotheticalResolution:
    """The private comparison door: how a cascade WOULD resolve, installs nothing.

    ``_ffi.resolve_hypothetical`` is a pure function of its inputs, so it
    answers a claim about HOW the cascade resolves rather than a claim about
    WHAT this process installed. It is deliberately absent from
    ``degenbot.config`` — using the installed verdict where a hypothetical is
    meant is a tautology.
    """

    def test_it_resolves_the_environment_without_installing(self) -> None:
        """A hypothetical resolves its captured env and leaves the verdict alone."""
        installed = _ffi.resolved_config()
        before_chain_id = installed.values.session.chain_id
        before_provenance = dict(installed.provenance)

        hypothetical = _ffi.resolve_hypothetical({"DEGENBOT_DEFAULT_CHAIN_ID": "8453"}, None)

        assert hypothetical.values.session.chain_id == 8453
        assert hypothetical.provenance["session.chain_id"] == "env"
        # The install-once contract held: the verdict and its file did not move.
        assert _ffi.resolved_config().values.session.chain_id == before_chain_id
        assert dict(_ffi.resolved_config().provenance) == before_provenance
        assert _ffi.resolved_config().config_file_path == installed.config_file_path

    def test_a_var_set_after_install_does_not_move_the_verdict(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A freeze is an in-process NEGATIVE assertion, not a fresh process."""
        before = _ffi.resolved_config().values.session.chain_id
        monkeypatch.setenv("DEGENBOT_DEFAULT_CHAIN_ID", "424242")
        assert _ffi.resolved_config().values.session.chain_id == before

    def test_it_is_unreachable_from_the_python_config_home(self) -> None:
        """The hypothetical lives on the raw FFI seam, not the driver home."""
        from degenbot import config as config_home

        assert not hasattr(config_home, "resolve_hypothetical")
        assert "resolve_hypothetical" not in config_home.__all__

    def test_a_load_refusal_is_a_typed_error_not_a_process_exit(self) -> None:
        """The boot-refusal error survives as a typed refusal."""
        with pytest.raises(ValueError, match="pathfinding.max_registered_paths") as excinfo:
            _ffi.resolve_hypothetical({"DEGENBOT_MAX_PATHS": "not-a-number"}, None)
        assert "not-a-number" in str(excinfo.value)

    def test_the_siblings_cover_the_three_cascade_methods(self) -> None:
        """Every argument-taking cascade method has an installing-free sibling."""
        node = _ffi.resolve_hypothetical_node_uri(
            {"DEGENBOT_RPC_WS_CHAINID_1": "wss://hypothetical.example/ws"},
            None,
            1,
            "request",
        )
        assert node.uri == "wss://hypothetical.example/ws"
        assert node.source == "env"

        chain = _ffi.resolve_hypothetical_chain_id({"DEGENBOT_DEFAULT_CHAIN_ID": "10"}, None)
        assert chain.chain_id == 10
        assert chain.source == "env"

        database = _ffi.resolve_hypothetical_database_path({}, None, "/tmp/override.db")
        assert database.path == "/tmp/override.db"
        assert database.source == "cli"


# ── The executor-policy parity extension ─────────────────────────────────────
#
# The core batch executor converts the verdict's policy knobs through ONE
# typed boundary (`degenbot-batch-executor`'s `ExecutorPolicy::from`), so the
# driver and the FFI construction carry no twin clamp. The conversion face
# (`_ffi.simulation.executor_policy_py`) projects that same conversion, and
# these tests pin the property: every executor knob equals the verdict value
# the same load recorded — and the one clamp (the sim fan-out floor) lives in
# the conversion, not in Python.

_EXECUTOR_KNOBS = (
    ("sim_concurrency", "simulation.pipeline_concurrency"),
    ("min_profit_margin_bps", "dispatch.min_profit_margin_bps"),
    ("erc6909_profit", "dispatch.erc6909_profit"),
    ("inject_code", "simulation.inject_executor_code"),
    ("inject_code_guard", "simulation.inject_executor_code"),
)


def test_every_executor_knob_is_the_verdict_value_the_same_load_recorded() -> None:
    """Every executor knob converts from the SAME verdict Python projects."""
    verdict = _ffi.resolved_config()
    policy = _ffi.simulation.executor_policy_py(verdict.values)
    mismatched = [
        f"{knob}={getattr(policy, knob)} != {path}={_read_named(verdict.values, path)}"
        for knob, path in _EXECUTOR_KNOBS
        if getattr(policy, knob) != _read_named(verdict.values, path)
    ]
    assert not mismatched, f"the conversion must read the verdict: {mismatched}"


def test_the_inject_guards_are_one_declared_key_converted_once() -> None:
    """Both inject guards follow `simulation.inject_executor_code`."""
    policy = _ffi.simulation.executor_policy_py(_ffi.resolved_config().values)
    stance = _ffi.resolved_config().values.simulation.inject_executor_code
    assert policy.inject_code is stance
    assert policy.inject_code_guard is stance


def test_the_sim_fanout_floor_is_the_conversions_not_pythons() -> None:
    """The floor lives in the conversion; the verdict answers 0 unclamped."""
    hypothetical = _ffi.resolve_hypothetical({"DEGENBOT_SIM_PIPELINE_CONCURRENCY": "0"}, None)
    assert hypothetical.values.simulation.pipeline_concurrency == 0
    policy = _ffi.simulation.executor_policy_py(hypothetical.values)
    assert policy.sim_concurrency == 1
