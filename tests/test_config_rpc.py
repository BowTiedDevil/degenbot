"""Tests for ``degenbot.config.resolve_rpc_uris`` — the standard RPC-URI cascade.

The resolver is the one shared resolution path for an HTTP/WS endpoint by
chain id, used by the library, the ``degenbot`` click CLI, and the settlement-arbitrage
example. It implements the precedence:

    CLI arg  >  OS env ``DEGENBOT_RPC_{HTTP,WS}_CHAINID_{cid}``  >
    caller fallback  >  config.toml ``rpc[cid]``/``ws[cid]``  >  raise

Each URI (http, ws) resolves **independently** — one may come from CLI while
the other comes from config.toml. There is no ``localhost`` default: a chain
with no configured endpoint anywhere raises ``RpcNotConfiguredError`` so the
misconfiguration surfaces immediately instead of crashing later on a connect.
"""

from __future__ import annotations

import tomllib
from typing import TYPE_CHECKING

import pytest

from degenbot import config as config_module
from degenbot.config import (
    RpcNotConfiguredError,
    resolve_http_rpc_uri,
    resolve_rpc_uris,
    resolve_ws_rpc_uri,
)

if TYPE_CHECKING:
    from pathlib import Path


@pytest.fixture(autouse=True)
def isolated_config_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point ``CONFIG_FILE`` at a tmp path that does not exist by default.

    The devcontainer ships a real ``~/.config/degenbot/config.toml`` with chain
    1 configured. Without isolation, ``resolve_rpc_uris(1)`` would return those
    URIs and the "not configured" / cascade-ordering tests couldn't distinguish
    the config.toml layer from the others. Each test that wants config.toml to
    provide a value writes to this path.
    """
    cfg = tmp_path / "config.toml"
    monkeypatch.setattr(config_module, "CONFIG_FILE", cfg)
    return cfg


def _write_config(
    cfg: Path,
    *,
    rpc: dict[int, str] | None = None,
    ws: dict[int, str] | None = None,
) -> None:
    """Write a valid config.toml (``DegenbotConfig`` requires ``database`` + ``rpc``)."""
    lines: list[str] = [
        "[database]",
        'path = ":memory:"',
        "[rpc]",
    ]
    for cid, uri in (rpc or {}).items():
        lines.append(f'{cid} = "{uri}"')
    if ws:
        lines.append("[ws]")
        for cid, uri in ws.items():
            lines.append(f'{cid} = "{uri}"')
    cfg.write_text("\n".join(lines) + "\n", encoding="utf-8")
    # sanity: round-trips through tomllib
    assert tomllib.loads(cfg.read_text(encoding="utf-8"))


# ── envvar names by chain id ──────────────────────────────────────


def _http_env(chain_id: int) -> str:
    return f"DEGENBOT_RPC_HTTP_CHAINID_{chain_id}"


def _ws_env(chain_id: int) -> str:
    return f"DEGENBOT_RPC_WS_CHAINID_{chain_id}"


class TestCliOverride:
    """CLI arguments are the highest-priority source."""

    def test_cli_http_overrides_everything(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv(_http_env(1), "https://from-env.example")
        monkeypatch.setenv(_ws_env(1), "wss://ws-env.example")

        http, ws = resolve_rpc_uris(1, cli_http="https://from-cli.example")

        assert http == "https://from-cli.example"
        # ws had no CLI override — falls through to OS env
        assert ws == "wss://ws-env.example"

    def test_cli_ws_overrides_everything(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv(_http_env(1), "https://from-env.example")
        monkeypatch.setenv(_ws_env(1), "wss://ws-env.example")

        http, ws = resolve_rpc_uris(1, cli_ws="wss://from-cli.example")

        assert http == "https://from-env.example"
        assert ws == "wss://from-cli.example"


class TestOsEnv:
    """OS envvars (``export``-able in the devcontainer) are the second layer."""

    def test_env_referenced_by_chain_id(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv(_http_env(1), "https://chain-1.example")
        monkeypatch.setenv(_ws_env(1), "wss://chain-1.example")

        http, ws = resolve_rpc_uris(1)

        assert http == "https://chain-1.example"
        assert ws == "wss://chain-1.example"

    def test_env_for_other_chain_is_not_used(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv(_http_env(1), "https://chain-1.example")
        monkeypatch.setenv(_ws_env(1), "wss://chain-1.example")

        with pytest.raises(RpcNotConfiguredError):
            resolve_rpc_uris(42161)


class TestFallbackRank:
    """Caller fallbacks rank below OS env but above config.toml."""

    def test_fallback_used_when_env_unset(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(_http_env(1), raising=False)
        monkeypatch.delenv(_ws_env(1), raising=False)

        http, ws = resolve_rpc_uris(
            1,
            fallback_http="https://fallback.example",
            fallback_ws="wss://fallback.example",
        )

        assert http == "https://fallback.example"
        assert ws == "wss://fallback.example"

    def test_env_beats_fallback(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv(_http_env(1), "https://env.example")
        monkeypatch.setenv(_ws_env(1), "wss://ws-env.example")

        http, _ws = resolve_rpc_uris(1, fallback_http="https://fallback.example")

        assert http == "https://env.example"

    def test_fallback_beats_config(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_http_env(1), raising=False)
        monkeypatch.delenv(_ws_env(1), raising=False)
        _write_config(
            isolated_config_file,
            rpc={1: "http://from-config:8545"},
            ws={1: "ws://from-config:8546"},
        )

        http, ws = resolve_rpc_uris(
            1,
            fallback_http="https://fallback.example",
            fallback_ws="wss://fallback.example",
        )

        assert http == "https://fallback.example"
        assert ws == "wss://fallback.example"


class TestConfigTomlLayer:
    """config.toml ``rpc[cid]``/``ws[cid]`` is the fourth layer."""

    def test_config_provides_when_no_higher_source(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_http_env(1), raising=False)
        monkeypatch.delenv(_ws_env(1), raising=False)
        _write_config(
            isolated_config_file,
            rpc={1: "http://from-config:8545"},
            ws={1: "ws://from-config:8546"},
        )

        http, ws = resolve_rpc_uris(1)

        # pydantic HttpUrl/WebsocketUrl normalize the empty path to "/"
        assert http == "http://from-config:8545/"
        assert ws == "ws://from-config:8546/"

    def test_config_missing_for_chain_falls_through_to_raise(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_http_env(999), raising=False)
        # config exists but only has chain 1
        _write_config(isolated_config_file, rpc={1: "http://from-config:8545"})

        with pytest.raises(RpcNotConfiguredError):
            resolve_rpc_uris(999)


class TestNotConfigured:
    """No source — raise, with a helpful message naming every source."""

    def test_raises_when_no_source(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # use a chain id nothing configures (env cleared, config.toml absent)
        chain = 999
        monkeypatch.delenv(_http_env(chain), raising=False)
        monkeypatch.delenv(_ws_env(chain), raising=False)

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            resolve_rpc_uris(chain)

        msg = str(exc_info.value)
        # http is checked first (Q4=B independent resolution), so the first
        # missing URI reported is http — its envvar + the config layer are named.
        assert _http_env(chain) in msg
        assert "config" in msg.lower()

    def test_raises_when_config_file_absent(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # isolated_config_file does not exist by default; chain 1 env cleared
        monkeypatch.delenv(_http_env(1), raising=False)
        monkeypatch.delenv(_ws_env(1), raising=False)

        with pytest.raises(RpcNotConfiguredError):
            resolve_rpc_uris(1)

    def test_is_value_error_subclass(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # existing callers use pytest.raises(ValueError); keep them working
        monkeypatch.delenv(_http_env(999), raising=False)
        monkeypatch.delenv(_ws_env(999), raising=False)

        with pytest.raises(ValueError):
            resolve_rpc_uris(999)

    def test_http_only_still_raises_for_ws(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # Q4=B: each URI resolves independently
        monkeypatch.delenv(_ws_env(999), raising=False)

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            resolve_rpc_uris(999, cli_http="https://only-http.example")

        # the message must say which of http/ws was missing
        assert "ws" in str(exc_info.value).lower()

    def test_ws_only_still_raises_for_http(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(_http_env(999), raising=False)

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            resolve_rpc_uris(999, cli_ws="wss://only-ws.example")

        assert "http" in str(exc_info.value).lower()


class TestIndependentResolution:
    """Q4=B: http and ws resolve independently through different layers."""

    def test_http_from_cli_ws_from_config(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_http_env(1), raising=False)
        monkeypatch.delenv(_ws_env(1), raising=False)
        _write_config(isolated_config_file, ws={1: "ws://from-config:8546"})

        http, ws = resolve_rpc_uris(1, cli_http="https://from-cli.example")

        assert http == "https://from-cli.example"
        # pydantic WebsocketUrl normalizes the empty path to "/"
        assert ws == "ws://from-config:8546/"


# ── single-URI resolvers ─────────────────────────────────────────


class TestResolveHttpOnly:
    """``resolve_http_rpc_uri`` walks the same cascade for HTTP alone.

    The provider factory is HTTP-only (pool updater needs ``eth_getLogs``, not a
    subscription), so a missing/unconfigured WS must not block it. The single-URI
    resolver shares the precedence list but raises only when *its* URI is
    unresolved.
    """

    def test_cli_beats_env_and_config(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.setenv(_http_env(1), "https://env.example")
        _write_config(isolated_config_file, rpc={1: "http://from-config:8545"})

        assert (
            resolve_http_rpc_uri(1, cli_http="https://from-cli.example")
            == "https://from-cli.example"
        )

    def test_env_beats_config(
        self, monkeypatch: pytest.MonkeyPatch, isolated_config_file: Path
    ) -> None:
        _write_config(isolated_config_file, rpc={1: "http://localhost:8545"})
        monkeypatch.setenv(_http_env(1), "https://env.example")

        assert resolve_http_rpc_uri(1) == "https://env.example"

    def test_config_layer_when_no_higher_source(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_http_env(1), raising=False)
        _write_config(isolated_config_file, rpc={1: "http://from-config:8545"})

        assert resolve_http_rpc_uri(1) == "http://from-config:8545/"

    def test_fallback_between_env_and_config(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_http_env(1), raising=False)
        _write_config(isolated_config_file, rpc={1: "http://from-config:8545"})

        assert (
            resolve_http_rpc_uri(1, fallback_http="https://fallback.example")
            == "https://fallback.example"
        )

    def test_raises_with_envvar_named_when_no_source(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(_http_env(999), raising=False)

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            resolve_http_rpc_uri(999)

        assert _http_env(999) in str(exc_info.value)

    def test_accepts_preloaded_config(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        """A caller-supplied config short-circuits the disk read (factory path)."""
        monkeypatch.delenv(_http_env(1), raising=False)
        _write_config(isolated_config_file, rpc={1: "http://from-config:8545"})
        from degenbot.config import load_config_from_file

        cfg = load_config_from_file(isolated_config_file)

        assert resolve_http_rpc_uri(1, config=cfg) == "http://from-config:8545/"


class TestResolveWsOnly:
    """Symmetric WS-only resolution."""

    def test_ws_from_cli(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv(_ws_env(1), "wss://env.example")

        assert resolve_ws_rpc_uri(1, cli_ws="wss://from-cli.example") == "wss://from-cli.example"

    def test_ws_from_config(
        self,
        monkeypatch: pytest.MonkeyPatch,
        isolated_config_file: Path,
    ) -> None:
        monkeypatch.delenv(_ws_env(1), raising=False)
        _write_config(isolated_config_file, ws={1: "ws://from-config:8546"})

        assert resolve_ws_rpc_uri(1) == "ws://from-config:8546/"
