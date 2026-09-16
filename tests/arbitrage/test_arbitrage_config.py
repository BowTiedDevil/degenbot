"""Tests for ArbitrageConfig — the unified arbitrage configuration value object.

`ArbitrageConfig` bundles the ~20 scattered arbitrage tunables (operator identity,
node endpoints, executor contract, dispatch knobs, path filters, dry-run)
that `main()` currently reads ad-hoc from three sources: a `mainnet.env`
dotenv dict, module-top constants, and CLI args. `from_env` is the factory
that delegates RPC resolution to the library `resolve_rpc_uris` cascade
(`examples/eth_backrun_helpers.py` → `degenbot.config.resolve_rpc_uris`).

Node resolution no longer defaults to `localhost`. A chain with no configured
endpoint in any layer raises `RpcNotConfiguredError` (a `ValueError` subclass)
pointing at `DEGENBOT_RPC_*_CHAINID_{cid}` / config.toml. The legacy
`NODE_HOST_*`/`NODE_PORT_*` variables are retired and ignored.
"""

from __future__ import annotations

import dataclasses
import warnings

import pytest

from degenbot import config as config_module
from degenbot.config import RpcNotConfiguredError
from degenbot.runner.config import ArbitrageConfig

_HTTP_ENV = "DEGENBOT_RPC_HTTP_CHAINID_1"
_WS_ENV = "DEGENBOT_RPC_WS_CHAINID_1"


def _set_rpc_env(monkeypatch: pytest.MonkeyPatch, *, http: str, ws: str) -> None:
    """Set the chain-1 RPC OS envvars (the new cascade mechanism)."""
    monkeypatch.setenv(_HTTP_ENV, http)
    monkeypatch.setenv(_WS_ENV, ws)


def _full_env() -> dict[str, str]:
    """Operator + executor fields. RPC is set via OS env (the new mechanism)."""
    return {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
        "INJECTED_EXECUTOR_ADDRESS": "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
        "EXECUTOR_OWNER_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
    }


class TestFromEnvFull:
    def test_full_env_populates_all_fields(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        cfg = ArbitrageConfig.from_env(_full_env(), live=True, permutation=None)

        assert cfg.dry_run is False
        assert cfg.operator_address == "0x9C56a29c7231974c269E24F9FB3c29203039089E"
        assert cfg.operator_private_key == "0x" + "a" * 64
        assert cfg.node_http == "https://eth.example.com"
        assert cfg.node_ws == "wss://ws.eth.example.com"
        assert cfg.executor_address == "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"
        assert cfg.inject_executor_code is False
        # main() behavior: inject=False keeps the env executor address
        # (EIP-55 canonical checksum casing)
        assert cfg.injected_address == "0x0D6d4C3CF3bD3b769De1821F2Be0D7d99913e4F1"
        assert cfg.executor_owner == "0x9C56a29c7231974c269E24F9FB3c29203039089E"

    def test_inject_code_true_overrides_executor_to_injected(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = _full_env() | {"INJECT_EXECUTOR_CODE": "1"}
        cfg = ArbitrageConfig.from_env(env, live=True, permutation=None)

        assert cfg.inject_executor_code is True
        # main() behavior: when INJECT_EXECUTOR_CODE, executor_address = injected_address
        assert cfg.executor_address == cfg.injected_address


class TestInjectExecutorCodeUnifiedResolution:
    """One flag, one surface: the injection stance resolves once in from_env.

    The retired arrangement read the bare name from two layers with opposite
    defaults (dotenv dict here, module constant off os.environ elsewhere), so
    a file-only override produced a bot that booted live but never submitted.
    """

    _LEGACY = "INJECT_EXECUTOR_CODE"
    _TYPED = "DEGENBOT_INJECT_EXECUTOR_CODE"

    def test_legacy_bare_os_env_name_raises_with_migration_message(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        monkeypatch.setenv(self._LEGACY, "1")
        monkeypatch.delenv(self._TYPED, raising=False)
        with pytest.raises(ValueError, match=self._TYPED):
            ArbitrageConfig.from_env({}, live=False, permutation=None)

    def test_typed_os_env_beats_dotenv_layer(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.setenv(self._TYPED, "1")
        env = _full_env() | {"INJECT_EXECUTOR_CODE": "0"}
        cfg = ArbitrageConfig.from_env(env, live=False, permutation=None)
        assert cfg.inject_executor_code is True

    def test_dotenv_only_and_env_only_agree(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.delenv(self._TYPED, raising=False)
        for value, expected in (("0", False), ("1", True)):
            via_dotenv = ArbitrageConfig.from_env(
                _full_env() | {"INJECT_EXECUTOR_CODE": value}, live=False, permutation=None
            )
            dotenv_free = {k: v for k, v in _full_env().items() if k != "INJECT_EXECUTOR_CODE"}
            monkeypatch.setenv(self._TYPED, value)
            via_env = ArbitrageConfig.from_env(dotenv_free, live=False, permutation=None)
            monkeypatch.delenv(self._TYPED, raising=False)
            assert via_dotenv.inject_executor_code == expected
            assert via_env.inject_executor_code == expected

    def test_unset_everywhere_defaults_to_no_injection(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.delenv(self._TYPED, raising=False)
        env = {k: v for k, v in _full_env().items() if k != "INJECT_EXECUTOR_CODE"}
        cfg = ArbitrageConfig.from_env(env, live=False, permutation=None)
        assert cfg.inject_executor_code is False


class TestDryRunDefaults:
    _DRY_RUN_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
    _DRY_RUN_ADDR = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

    def test_dry_run_missing_operator_defaults_to_valid_dummy_key(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        # Regression: the dry-run placeholder private key must be a VALID
        # secp256k1 scalar. `dispatch_profitable_results` constructs a
        # `TxSigner(key=cfg.operator_private_key)` unconditionally — the
        # former all-zero placeholder (0x00..00) is rejected by the curve (zero
        # is not a valid scalar), raising `ValueError: signature error` and
        # killing the result-consumer task on the very first result batch. The
        # Anvil account-0 key is a well-known valid throwaway that never signs
        # (the Rust submit leaf's `dry_run` guard skips `sign_eip1559`).
        from degenbot._ffi.submission import TxSigner

        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = _full_env() | {"OPERATOR_ADDRESS": "", "OPERATOR_PRIVATE_KEY": ""}
        cfg = ArbitrageConfig.from_env(env, live=False, permutation=None)

        assert cfg.dry_run is True
        assert cfg.operator_address == self._DRY_RUN_ADDR
        assert cfg.operator_private_key == self._DRY_RUN_KEY
        # The placeholder must actually load as a signer (the crash surface).
        # `TxSigner.address` renders lowercase (not EIP-55), so compare
        # case-insensitively against the config's checksummed address.
        signer = TxSigner(key=cfg.operator_private_key, chain_id=1)
        assert signer.address.lower() == cfg.operator_address.lower()


class TestLiveModeRequiresOperator:
    def test_live_mode_missing_operator_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # operator check happens before node resolution — ValueError raised, no DeprecationWarning
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = _full_env() | {"OPERATOR_ADDRESS": "", "OPERATOR_PRIVATE_KEY": ""}
        with pytest.raises(ValueError, match="OPERATOR"):
            ArbitrageConfig.from_env(env, live=True, permutation=None)


class TestLiveOwnerOperatorTriangle:
    """Live mode refuses an owner/operator mismatch.

    The simulator's execute() caller is executor_owner; a mismatched owner
    makes every simulation revert on the contract's owner gate, and the bot
    then live-idles indefinitely with zero submissions and no error.
    """

    def test_live_mismatched_owner_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = _full_env() | {"EXECUTOR_OWNER_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"}
        with pytest.raises(ValueError, match="EXECUTOR_OWNER_ADDRESS"):
            ArbitrageConfig.from_env(env, live=True, permutation=None)

    def test_live_empty_owner_defaults_to_operator(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = {k: v for k, v in _full_env().items() if k != "EXECUTOR_OWNER_ADDRESS"}
        cfg = ArbitrageConfig.from_env(env, live=True, permutation=None)
        assert cfg.executor_owner == cfg.operator_address

    def test_dry_run_mismatched_owner_allowed(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = _full_env() | {"EXECUTOR_OWNER_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"}
        cfg = ArbitrageConfig.from_env(env, live=False, permutation=None)
        assert cfg.executor_owner == "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"


class TestRpcCascade:
    """from_env delegates to resolve_rpc_uris: CLI > OS env > legacy > config.toml > raise."""

    def test_missing_everywhere_raises_with_envvar_pointers(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv(_HTTP_ENV, raising=False)
        monkeypatch.delenv(_WS_ENV, raising=False)

        # also isolate config.toml so the devcontainer's real config doesn't satisfy chain 1
        monkeypatch.setattr(
            config_module, "CONFIG_FILE", type("P", (), {"exists": lambda self: False})()
        )

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            ArbitrageConfig.from_env({}, live=False, permutation=None)

        msg = str(exc_info.value)
        assert _HTTP_ENV in msg
        assert "config" in msg.lower()

    def test_cli_override_beats_os_env(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://from-env.example", ws="wss://ws-env.example")
        cfg = ArbitrageConfig.from_env(
            _full_env(),
            live=False,
            permutation=None,
            cli_http="https://from-cli.example",
            cli_ws="wss://from-cli.example",
        )
        assert cfg.node_http == "https://from-cli.example"
        assert cfg.node_ws == "wss://from-cli.example"

    def test_cli_http_only_ws_from_env(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://from-env.example", ws="wss://ws-env.example")
        cfg = ArbitrageConfig.from_env(
            _full_env(),
            live=False,
            permutation=None,
            cli_http="https://from-cli.example",
        )
        assert cfg.node_http == "https://from-cli.example"
        assert cfg.node_ws == "wss://ws-env.example"


class TestLegacyNodeHostIgnored:
    """NODE_HOST_*/NODE_PORT_* are retired: the variables are ignored (no warning)."""

    def test_legacy_vars_are_fully_ignored(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(_HTTP_ENV, raising=False)
        monkeypatch.delenv(_WS_ENV, raising=False)

        monkeypatch.setattr(
            config_module, "CONFIG_FILE", type("P", (), {"exists": lambda self: False})()
        )
        env = _full_env() | {
            "NODE_HOST_HTTP": "https://legacy.example",
            "NODE_PORT_HTTP": "8545",
            "NODE_HOST_WEBSOCKET": "wss://legacy.example",
            "NODE_PORT_WEBSOCKET": "8546",
        }
        # No RPC source in any layer → RpcNotConfiguredError, and no
        # DeprecationWarning either (the variables are not consulted at all).
        with warnings.catch_warnings():
            warnings.simplefilter("error", DeprecationWarning)
            with pytest.raises(RpcNotConfiguredError):
                ArbitrageConfig.from_env(env, live=False, permutation=None)

    def test_legacy_vars_lose_to_os_env_without_warnings(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _set_rpc_env(monkeypatch, http="https://from-env.example", ws="wss://ws-env.example")
        env = _full_env() | {
            "NODE_HOST_HTTP": "https://legacy.example",
            "NODE_PORT_HTTP": "8545",
        }
        with warnings.catch_warnings():
            warnings.simplefilter("error", DeprecationWarning)
            cfg = ArbitrageConfig.from_env(env, live=False, permutation=None)

        assert cfg.node_http == "https://from-env.example"


class TestPermutationOverride:
    def test_permutation_string_becomes_singleton_frozenset(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        cfg = ArbitrageConfig.from_env(_full_env(), live=False, permutation="V3-V4-V3")
        assert cfg.permutation_filter == frozenset({"V3-V4-V3"})

    def test_no_permutation_is_none(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        cfg = ArbitrageConfig.from_env(_full_env(), live=False, permutation=None)
        assert cfg.permutation_filter is None


class TestExecutorZeroAddress:
    def test_zero_executor_address_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        env = _full_env() | {"EXECUTOR_CONTRACT_ADDRESS": "0x" + "0" * 40}
        with pytest.raises(ValueError, match=r"zero address|EXECUTOR_CONTRACT_ADDRESS"):
            ArbitrageConfig.from_env(env, live=False, permutation=None)


class TestImmutability:
    def test_config_is_frozen(self, monkeypatch: pytest.MonkeyPatch) -> None:
        _set_rpc_env(monkeypatch, http="https://eth.example.com", ws="wss://ws.eth.example.com")
        cfg = ArbitrageConfig.from_env(_full_env(), live=False, permutation=None)
        with pytest.raises(dataclasses.FrozenInstanceError):
            cfg.operator_address = "0x" + "1" * 40  # type: ignore[misc]
