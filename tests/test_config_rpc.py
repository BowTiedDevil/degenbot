"""Python's view of the four-layer config cascade (ADR-062 D7/D10).

``degenbot-config`` owns the resolution: an explicit override, the environment,
the operator file, then a declared default, with the winning layer reported.
Python delegates to it, so these tests pin the delegation rather than a second
implementation -- including the asymmetry that used to exist, where a Python
cascade read an ``[rpc]`` spelling the typed loader refuses and therefore
ignored ``DEGENBOT_CONFIG`` entirely.

The config is installed once at FFI module init, so the file and environment
layers are exercised in a subprocess whose environment is fixed BEFORE the
import; the ``cli`` override and the refusals are per-call and are exercised
in-process. A subprocess is the only honest way to observe an
install-at-import cascade, and it is what makes ``DEGENBOT_CONFIG`` a real
test rather than a re-derivation of the path.
"""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys
from typing import TYPE_CHECKING, Any

import pytest

from degenbot import config as config_module
from degenbot.bot import Bot
from degenbot.config import (
    RpcNotConfiguredError,
    resolve_chain_id,
    resolve_http_rpc_uri,
    resolve_node,
    resolve_rpc_uris,
    resolve_ws_rpc_uri,
)

if TYPE_CHECKING:
    from pathlib import Path

# A chain id no operator file, environment, or harness sets, so an in-process
# refusal is genuinely the absence of every layer rather than a leak.
_UNCONFIGURED_CHAIN = 988877

# The probe drives the PUBLIC surface from a fresh interpreter, so a layer that
# is only installed at import is observable. It reports one JSON object per op
# and turns a refusal into {"error": "Type: message"} so a test can assert on
# the message the operator actually sees.
_PROBE = """\
import json
import sys

from degenbot import _ffi
from degenbot.config import (
    config_file_path,
    resolve_chain_id,
    resolve_database_path,
    resolve_http_rpc_uri,
    resolve_node,
    resolve_rpc_uris,
    resolve_ws_rpc_uri,
)

results = []
for op in json.loads(sys.argv[1]):
    kind = op[0]
    try:
        if kind == "file":
            results.append({"file": config_file_path()})
        elif kind == "node":
            resolved = resolve_node(op[1], op[2], node=op[3])
            results.append({"uri": resolved.uri, "source": resolved.source})
        elif kind == "http":
            results.append({"uri": resolve_http_rpc_uri(op[1], node=op[2])})
        elif kind == "ws":
            results.append({"uri": resolve_ws_rpc_uri(op[1], node=op[2])})
        elif kind == "pair":
            results.append({"pair": list(resolve_rpc_uris(op[1], node=op[2]))})
        elif kind == "chain":
            results.append({"chain_id": resolve_chain_id(op[1])})
        elif kind == "chain_source":
            resolved = _ffi.resolved_config().resolve_chain_id(op[1])
            results.append({"chain_id": resolved.chain_id, "source": resolved.source})
        elif kind == "database":
            results.append({"path": resolve_database_path(op[1])})
        elif kind == "database_source":
            resolved = _ffi.resolved_config().resolve_database_path(op[1])
            results.append({"path": resolved.path, "source": resolved.source})
        else:
            raise AssertionError("unknown probe op: " + kind)
    except BaseException as exc:  # noqa: BLE001 - the message is the assertion
        results.append({"error": type(exc).__name__ + ": " + str(exc)})
print(json.dumps(results))
"""


def _probe(tmp_path: Path, file_body: str, *ops: list[Any], **env_overrides: str) -> list[dict]:
    """Run ``ops`` in a fresh interpreter with a pinned file and environment.

    Every ``DEGENBOT_RPC_*`` / ``DEGENBOT_DEFAULT_CHAIN_ID`` name is cleared
    first and the XDG homes are pointed into ``tmp_path``, so neither the
    developer's shell nor their ``~/.config`` file can decide a layer this test
    is about. ``DEGENBOT_CONFIG`` always names the written file, so the file
    layer is reachable only through the documented selection.

    Args:
        tmp_path: The per-test scratch directory.
        file_body: The operator file's TOML.
        ops: Probe operations, in order.
        env_overrides: Extra environment names to set for the child.

    Returns:
        One result dict per op.

    """
    config_file = tmp_path / "operator.toml"
    config_file.write_text(file_body, encoding="utf-8")
    xdg_config = tmp_path / "xdg-config"
    xdg_state = tmp_path / "xdg-state"
    xdg_config.mkdir(exist_ok=True)
    xdg_state.mkdir(exist_ok=True)

    env = dict(os.environ)
    for name in list(env):
        if name.startswith("DEGENBOT_RPC_") or name == "DEGENBOT_DEFAULT_CHAIN_ID":
            del env[name]
    env.update(
        {
            "DEGENBOT_CONFIG": str(config_file),
            "XDG_CONFIG_HOME": str(xdg_config),
            "XDG_STATE_HOME": str(xdg_state),
        },
        **env_overrides,
    )
    completed = subprocess.run(  # noqa: S603 - fixed argv, no shell
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


def _nodes_file(
    *,
    http: dict[int, str] | None = None,
    ws: dict[int, str] | None = None,
    ipc: dict[int, str] | None = None,
) -> str:
    """Render an operator file carrying the declared ``[nodes.*]`` tables."""
    lines: list[str] = []
    for table, entries in (("http", http), ("ws", ws), ("ipc", ipc)):
        if not entries:
            continue
        lines.append(f"[nodes.{table}]")
        lines.extend(f"{chain_id} = {json.dumps(uri)}" for chain_id, uri in entries.items())
    return "\n".join(lines) + "\n"


class _BoundaryProvider:
    """A stand-in for the external RPC boundary (no network, observable chain).

    ``Bot`` reaches the network only through the injected provider, so the
    chain identity the session adopts is observable from this object alone.
    """

    def __init__(self, chain_id: int) -> None:
        self.chain_id = chain_id

    def close(self) -> None:
        """No-op: the real provider's close releases an Arc with no flag."""


class TestFileLayer:
    """The operator file is the base layer, and ``DEGENBOT_CONFIG`` selects it."""

    def test_file_only_resolution_with_no_environment(self, tmp_path: Path) -> None:
        """Every declared table is read, with nothing exported."""
        # One chain per table, so each op exercises the table it names rather
        # than the transport preference order within a single chain.
        body = _nodes_file(
            http={1: "http://file-http:8545"},
            ws={2: "ws://file-ws:8546"},
            ipc={3: "/tmp/file.ipc"},
        )

        from_http, from_ws, from_ipc, selected = _probe(
            tmp_path,
            body,
            ["http", 1, None],
            ["ws", 2, None],
            ["http", 3, None],
            ["file"],
        )

        assert from_http == {"uri": "http://file-http:8545"}
        assert from_ws == {"uri": "ws://file-ws:8546"}
        assert from_ipc == {"uri": "/tmp/file.ipc"}
        assert selected == {"file": str(tmp_path / "operator.toml")}

    def test_degenbot_config_is_the_selected_file(self, tmp_path: Path) -> None:
        """``DEGENBOT_CONFIG`` names the file; the XDG home is not consulted.

        The pre-0.6 Python cascade re-derived ``$XDG_CONFIG_HOME`` itself and
        so silently ignored this name, which is why a Python-launched bot
        could not take its endpoints from the operator file the console boots
        on.
        """
        xdg_home = tmp_path / "xdg-config"
        xdg_home.mkdir()
        (xdg_home / "degenbot").mkdir()
        (xdg_home / "degenbot" / "config.toml").write_text(
            _nodes_file(http={1: "http://xdg-http:8545"}),
            encoding="utf-8",
        )

        selected_file, request = _probe(
            tmp_path,
            _nodes_file(http={1: "http://override-http:8545"}),
            ["file"],
            ["http", 1, None],
        )

        assert selected_file == {"file": str(tmp_path / "operator.toml")}
        assert request == {"uri": "http://override-http:8545"}

    def test_each_layer_reports_itself_as_the_winner(self, tmp_path: Path) -> None:
        """Every resolution names the layer that supplied the value.

        One probe per environment: the cascade is installed at import, so the
        environment belongs to the interpreter, not to a call.
        """
        body = _nodes_file(http={1: "http://file-http:8545"})

        from_file = _probe(tmp_path, body, ["node", 1, "request", None])[0]
        from_env = _probe(
            tmp_path,
            body,
            ["node", 1, "request", None],
            DEGENBOT_RPC_HTTP_CHAINID_1="https://env-http:8545",
        )[0]
        from_cli = _probe(tmp_path, body, ["node", 1, "request", "https://cli-http:8545"])[0]

        assert from_file == {"uri": "http://file-http:8545", "source": "file"}
        assert from_env == {"uri": "https://env-http:8545", "source": "env"}
        assert from_cli == {"uri": "https://cli-http:8545", "source": "cli"}

    def test_capabilities_resolve_independently(self, tmp_path: Path) -> None:
        """One capability from the environment, the other from the file.

        The two scopes are not one answer with two names: a session can take
        its read endpoint from an export and its feed endpoint from the file,
        in the same process, from the same installed config.
        """
        body = _nodes_file(
            http={1: "http://file-http:8545"},
            ws={2: "ws://file-ws:8546"},
        )

        env_request, file_subscription, refused, file_request = _probe(
            tmp_path,
            body,
            ["node", 1, "request", None],
            ["node", 2, "subscription", None],
            ["node", 1, "subscription", None],
            ["node", 2, "request", None],
            DEGENBOT_RPC_HTTP_CHAINID_1="https://env-http:8545",
        )

        assert env_request == {"uri": "https://env-http:8545", "source": "env"}
        assert file_subscription == {"uri": "ws://file-ws:8546", "source": "file"}
        assert "subscription" in refused["error"], "the file HTTP entry is not a feed"
        assert file_request == {"uri": "ws://file-ws:8546", "source": "file"}


class TestScopes:
    """A capability is a transport filter, not a preference order alone."""

    def test_subscription_scope_never_selects_an_http_entry(self, tmp_path: Path) -> None:
        """An HTTP-only file satisfies a read, never a feed."""

        request, subscription = _probe(
            tmp_path,
            _nodes_file(http={1: "http://file-http:8545"}),
            ["http", 1, None],
            ["ws", 1, None],
        )

        assert request == {"uri": "http://file-http:8545"}
        assert "subscription" in subscription["error"]

    def test_an_http_override_does_not_fill_the_subscription_slot(self, tmp_path: Path) -> None:
        """The override is classified too: ``http://`` cannot become a feed."""
        subscription = _probe(
            tmp_path,
            _nodes_file(),
            ["ws", 1, "http://override-http:8545"],
        )[0]

        assert "subscription" in subscription["error"]

    def test_an_ipc_entry_serves_both_scopes(self, tmp_path: Path) -> None:
        """The IPC table is a first-class transport in either capability."""

        request, subscription = _probe(
            tmp_path,
            _nodes_file(ipc={1: "/tmp/file.ipc"}),
            ["http", 1, None],
            ["ws", 1, None],
        )

        assert request == {"uri": "/tmp/file.ipc"}
        assert subscription == {"uri": "/tmp/file.ipc"}


class TestNotConfigured:
    """No layer supplied an endpoint: refuse, naming every layer consulted."""

    def test_the_refusal_names_all_four_layers(self) -> None:
        with pytest.raises(RpcNotConfiguredError) as exc_info:
            resolve_http_rpc_uri(_UNCONFIGURED_CHAIN)

        msg = str(exc_info.value)
        assert f"DEGENBOT_RPC_HTTP_CHAINID_{_UNCONFIGURED_CHAIN}" in msg
        assert f"DEGENBOT_RPC_WS_CHAINID_{_UNCONFIGURED_CHAIN}" in msg
        assert f"DEGENBOT_RPC_IPC_CHAINID_{_UNCONFIGURED_CHAIN}" in msg
        assert "nodes.http" in msg
        assert "nodes.ws" in msg
        assert "nodes.ipc" in msg
        assert "explicit node argument" in msg
        assert "there is no localhost default" in msg

    def test_it_stays_a_value_error(self) -> None:
        """Callers that already catch a misconfigured endpoint keep working."""
        with pytest.raises(ValueError):  # noqa: PT011 - the class is the assertion
            resolve_rpc_uris(_UNCONFIGURED_CHAIN)

    def test_the_pair_reports_which_capability_is_missing(self) -> None:
        with pytest.raises(RpcNotConfiguredError) as exc_info:
            resolve_rpc_uris(_UNCONFIGURED_CHAIN, node="https://only-http:8545")

        assert "subscription" in str(exc_info.value)


class TestRetiredOverrideKeywords:
    """The per-transport override keywords are gone, not translated."""

    @pytest.mark.parametrize(
        "fn",
        [resolve_http_rpc_uri, resolve_ws_rpc_uri, resolve_rpc_uris],
    )
    def test_fallback_keywords_are_refused(self, fn: Any) -> None:
        with pytest.raises(TypeError, match="fallback_http") as exc_info:
            fn(1, fallback_http="https://fallback:8545")

        assert "node=" in str(exc_info.value)

    def test_cli_keywords_are_refused(self) -> None:
        with pytest.raises(TypeError, match="cli_http") as exc_info:
            resolve_rpc_uris(1, cli_http="https://from-cli:8545")

        assert "node=" in str(exc_info.value)

    def test_an_unrecognized_keyword_is_a_plain_type_error(self) -> None:
        with pytest.raises(TypeError, match="unexpected keyword argument"):
            resolve_rpc_uris(1, endpoint="https://from-cli:8545")


class TestBotKeywordOverrides:
    """``Bot``'s keywords are the explicit layer, ahead of the file and the env.

    The suite's installed config names chain 1 (via ``tests/ambient_config.toml``
    and the developer's environment), so an override that lands on another chain
    is by construction outranking every configured layer.
    """

    def test_chain_and_database_overrides_beat_every_configured_layer(self, tmp_path: Path) -> None:
        database = tmp_path / "session.db"
        provider = _BoundaryProvider(4242)

        with Bot(chain_id=4242, database=str(database), provider=provider) as bot:
            assert bot.chain_id == 4242
            assert bot.database_path == database

    def test_the_node_override_reaches_the_provider_factory(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The endpoint the session asks for is the one the factory resolves."""
        recorded: dict[str, object] = {}

        def fake_factory(*, chain_id: int | str | None = None, node: str | None = None) -> Any:
            recorded["chain_id"] = chain_id
            recorded["node"] = node
            return _BoundaryProvider(1)

        monkeypatch.setattr("degenbot.bot._bot.get_provider_from_config", fake_factory)

        with Bot(node="https://override-http:8545", database=":memory:") as bot:
            assert bot.chain_id == resolve_chain_id(None)

        assert recorded["node"] == "https://override-http:8545"

    def test_a_provider_from_another_chain_is_refused(self) -> None:
        from degenbot.exceptions.base import DegenbotValueError

        with (
            pytest.raises(DegenbotValueError, match="wrong chain"),
            Bot(
                chain_id=4242,
                database=":memory:",
                provider=_BoundaryProvider(1),
            ),
        ):
            pass


class TestDatabaseAndChainCascade:
    """The chain id and the database path cascade through the same four layers."""

    def test_the_database_path_cascades(self, tmp_path: Path) -> None:
        body = "[database]\npath = " + chr(34) + "/operator/operator.db" + chr(34) + "\n"

        declared = _probe(tmp_path, body, ["database_source", None])[0]
        from_env = _probe(
            tmp_path,
            body,
            ["database_source", None],
            DEGENBOT_DB_PATH=str(tmp_path / "env.db"),
        )[0]
        override = _probe(tmp_path, body, ["database_source", ":memory:"])[0]

        assert declared == {"path": "/operator/operator.db", "source": "file"}
        assert from_env == {"path": str(tmp_path / "env.db"), "source": "env"}
        assert override == {"path": ":memory:", "source": "cli"}

    def test_a_tilde_database_path_is_expanded_by_the_resolver(self, tmp_path: Path) -> None:
        """The file is hand-edited, so a ``~`` is what an operator writes.

        The resolver expands it; left literal it would resolve against the
        process cwd and SQLite could not open the file.
        """
        body = "[database]\npath = " + chr(34) + "~/operator.db" + chr(34) + "\n"

        resolved = _probe(tmp_path, body, ["database", None])[0]

        assert "~" not in resolved["path"]
        assert resolved["path"].startswith(str(pathlib.Path.home()))

    def test_the_session_chain_id_cascades(self, tmp_path: Path) -> None:
        body = "[session]\nchain_id = 8453\n"

        from_file = _probe(tmp_path, body, ["chain_source", None])[0]
        from_env = _probe(
            tmp_path,
            body,
            ["chain_source", None],
            DEGENBOT_DEFAULT_CHAIN_ID="42161",
        )[0]
        override = _probe(tmp_path, body, ["chain_source", "137"])[0]

        assert from_file == {"chain_id": 8453, "source": "file"}
        assert from_env == {"chain_id": 42161, "source": "env"}
        assert override == {"chain_id": 137, "source": "cli"}

    def test_a_non_integer_chain_id_is_refused_by_the_layer_that_owns_the_spelling(
        self,
    ) -> None:
        with pytest.raises(ValueError, match="explicit layer"):
            resolve_chain_id("mainnet")

    def test_the_module_no_longer_owns_the_file_location(self) -> None:
        """The duplicated XDG/path logic is gone; the core reports the file."""
        assert config_module.config_file_path() == config_module._ffi.resolved_config().config_file_path
        for retired in ("CONFIG_DIR", "CONFIG_FILE", "DB_PATH", "DegenbotConfig"):
            assert not hasattr(config_module, retired)
