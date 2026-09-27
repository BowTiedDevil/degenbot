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


def test_every_key_the_generated_reference_names_is_in_the_verdict() -> None:
    """The verdict carries every key the schema declares, with no accessor per key.

    The generated reference is the schema's own output, so this fails the
    moment a key is declared and the verdict does not grow with it.
    """
    declared = _declared_toml_paths()
    assert declared, "the generated key reference must name the declared keys"
    carried = set(_ffi.resolved_config().values)
    assert carried == declared, (
        "the verdict must carry exactly the declared keys: "
        f"missing={sorted(declared - carried)} unexpected={sorted(carried - declared)}"
    )


def test_a_key_the_reference_does_not_name_is_not_in_the_verdict() -> None:
    """The projection is closed: an undeclared path is absent, not defaulted."""
    assert "database.not_a_key" not in _ffi.resolved_config().values


def test_a_declared_value_keeps_the_kind_its_key_declared() -> None:
    """A value is a typed Python value, not a stringly-typed rendering.

    A path key must not arrive as a number, and a wei key must not arrive as
    a float: the parity oracle that compares the verdict against a Rust load
    compares these values, so a lossy projection would make it agree on a
    number nobody configured.
    """
    values = _ffi.resolved_config().values
    assert isinstance(values["pump.pump_debounce_ms"], int)
    assert isinstance(values["solve.min_profit_wei"], int)
    assert isinstance(values["allocator.mimalloc_purge_delay_mult"], float)
    assert isinstance(values["telemetry.metrics_addr"], str)
    assert isinstance(values["database.path"], str)
    assert values["telemetry.otel"] is True
    # An unset-able key projects as its kind or as None, never as a string.
    assert values["logging.trace_jsonl"] is None or isinstance(values["logging.trace_jsonl"], str)


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
    assert verdict.declared_database_path == verdict.values["database.path"]


def test_discovery_batch_size_is_clamped_to_a_positive_batch() -> None:
    """The batch size the discovery pipeline forwards is always >= 1.

    A zero would collapse the batched iterator into a per-path busy loop.
    """
    assert _ffi.resolved_config().discovery_batch_size >= 1



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
        before_values = dict(installed.values)
        before_provenance = dict(installed.provenance)

        hypothetical = _ffi.resolve_hypothetical({"DEGENBOT_DEFAULT_CHAIN_ID": "8453"}, None)

        assert hypothetical.values["session.chain_id"] == 8453
        assert hypothetical.provenance["session.chain_id"] == "env"
        # The install-once contract held: the verdict and its file did not move.
        assert dict(_ffi.resolved_config().values) == before_values
        assert dict(_ffi.resolved_config().provenance) == before_provenance
        assert _ffi.resolved_config().config_file_path == installed.config_file_path

    def test_a_var_set_after_install_does_not_move_the_verdict(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """A freeze is an in-process NEGATIVE assertion, not a fresh process."""
        before = dict(_ffi.resolved_config().values)
        monkeypatch.setenv("DEGENBOT_DEFAULT_CHAIN_ID", "424242")
        assert dict(_ffi.resolved_config().values) == before

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
