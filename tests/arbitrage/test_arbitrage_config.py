"""Tests for ArbitrageConfig — the unified arbitrage configuration value object.

`ArbitrageConfig` bundles the ~20 scattered arbitrage tunables (operator identity,
node endpoints, executor contract, dispatch knobs, path filters, dry-run)
that `main()` currently reads ad-hoc from three sources: a `mainnet.env`
dotenv dict, module-top constants, and CLI args. `from_env` is the factory
that delegates RPC resolution to the library `resolve_rpc_uris` cascade
(`examples/eth_backrun_helpers.py` → `degenbot.config.resolve_rpc_uris`).

Node resolution no longer defaults to `localhost`. A chain with no configured
endpoint in any layer raises `RpcNotConfiguredError` (a `ValueError` subclass)
pointing at the `DEGENBOT_RPC_*_CHAINID_{cid}` env families and the
`[nodes.*]` file tables. The legacy `NODE_HOST_*`/`NODE_PORT_*`\nvariables are retired and ignored.
"""

from __future__ import annotations

import dataclasses
import warnings

import pytest

from degenbot.config import RpcNotConfiguredError
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides

# A chain id no operator file, environment, or harness sets, so a refusal is
# genuinely the absence of every layer rather than a leak.
_UNCONFIGURED_CHAIN = 988877

# The explicit override layer, used wherever a test only needs the cascade to
# answer: one endpoint, classified by the core, serves both capabilities.
_NODE = "wss://override.example"
_OVERRIDE = RpcCascadeOverrides(chain_id=1, node=_NODE)


def _cfg(env, *, live=False, permutation=None, rpc=None) -> ArbitrageConfig:
    """Build a config with the RPC override pinned.

    Every test here is about a non-RPC field, so the endpoint is supplied
    through the explicit layer rather than by mutating an environment the
    installed config has already read.
    """
    return ArbitrageConfig.from_env(
        env, live=live, permutation=permutation, rpc=rpc if rpc is not None else _OVERRIDE
    )


def _full_env() -> dict[str, str]:
    """Operator + executor fields; the RPC override is supplied by :func:`_cfg`."""
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
        cfg = _cfg(_full_env(), live=True, permutation=None)

        assert cfg.dry_run is False
        assert cfg.operator_address == "0x9C56a29c7231974c269E24F9FB3c29203039089E"
        assert cfg.operator_private_key == "0x" + "a" * 64
        assert cfg.chain_id == 1
        assert cfg.node_http == _NODE
        assert cfg.node_ws == _NODE
        assert cfg.executor_address == "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"
        assert cfg.inject_executor_code is False
        # main() behavior: inject=False keeps the env executor address
        # (EIP-55 canonical checksum casing)
        assert cfg.injected_address == "0x0D6d4C3CF3bD3b769De1821F2Be0D7d99913e4F1"
        assert cfg.executor_owner == "0x9C56a29c7231974c269E24F9FB3c29203039089E"

    def test_inject_code_true_overrides_executor_to_injected(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        env = _full_env() | {"INJECT_EXECUTOR_CODE": "1"}
        cfg = _cfg(env, live=False, permutation=None)

        assert cfg.inject_executor_code is True
        # injection overrides the executor with the injected address
        assert cfg.executor_address == cfg.injected_address

    def test_live_with_injection_is_refused(self, monkeypatch: pytest.MonkeyPatch) -> None:
        env = _full_env() | {"INJECT_EXECUTOR_CODE": "1"}
        with pytest.raises(ValueError, match="inject"):
            _cfg(env, live=True, permutation=None)


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
        monkeypatch.setenv(self._LEGACY, "1")
        monkeypatch.delenv(self._TYPED, raising=False)
        with pytest.raises(ValueError, match=self._TYPED):
            _cfg({}, live=False, permutation=None)

    def test_typed_os_env_beats_dotenv_layer(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.setenv(self._TYPED, "1")
        env = _full_env() | {"INJECT_EXECUTOR_CODE": "0"}
        cfg = _cfg(env, live=False, permutation=None)
        assert cfg.inject_executor_code is True

    def test_dotenv_only_and_env_only_agree(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.delenv(self._TYPED, raising=False)
        for value, expected in (("0", False), ("1", True)):
            via_dotenv = _cfg(
                _full_env() | {"INJECT_EXECUTOR_CODE": value}, live=False, permutation=None
            )
            dotenv_free = {k: v for k, v in _full_env().items() if k != "INJECT_EXECUTOR_CODE"}
            monkeypatch.setenv(self._TYPED, value)
            via_env = _cfg(dotenv_free, live=False, permutation=None)
            monkeypatch.delenv(self._TYPED, raising=False)
            assert via_dotenv.inject_executor_code == expected
            assert via_env.inject_executor_code == expected

    def test_unset_everywhere_defaults_to_no_injection(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.delenv(self._TYPED, raising=False)
        env = {k: v for k, v in _full_env().items() if k != "INJECT_EXECUTOR_CODE"}
        cfg = _cfg(env, live=False, permutation=None)
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

        env = _full_env() | {"OPERATOR_ADDRESS": "", "OPERATOR_PRIVATE_KEY": ""}
        cfg = _cfg(env, live=False, permutation=None)

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
        env = _full_env() | {"OPERATOR_ADDRESS": "", "OPERATOR_PRIVATE_KEY": ""}
        with pytest.raises(ValueError, match="OPERATOR"):
            _cfg(env, live=True, permutation=None)


class TestLiveOwnerOperatorTriangle:
    """Live mode refuses an owner/operator mismatch.

    The simulator's execute() caller is executor_owner; a mismatched owner
    makes every simulation revert on the contract's owner gate, and the bot
    then live-idles indefinitely with zero submissions and no error.
    """

    def test_live_mismatched_owner_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        env = _full_env() | {"EXECUTOR_OWNER_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"}
        with pytest.raises(ValueError, match="EXECUTOR_OWNER_ADDRESS"):
            _cfg(env, live=True, permutation=None)

    def test_live_empty_owner_defaults_to_operator(self, monkeypatch: pytest.MonkeyPatch) -> None:
        env = {k: v for k, v in _full_env().items() if k != "EXECUTOR_OWNER_ADDRESS"}
        cfg = _cfg(env, live=True, permutation=None)
        assert cfg.executor_owner == cfg.operator_address

    def test_dry_run_mismatched_owner_allowed(self, monkeypatch: pytest.MonkeyPatch) -> None:
        env = _full_env() | {"EXECUTOR_OWNER_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"}
        cfg = _cfg(env, live=False, permutation=None)
        assert cfg.executor_owner == "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"


class TestRunnerKnobResolution:
    """Runner knobs resolve once: OS env > example dotenv > code default,
    with fail-loud numeric parsing (a typo'd knob is a loud error)."""

    _KNOB_ENVS = (
        "DEGENBOT_MAX_PATHS",
        "DEGENBOT_MIN_PROFIT_MARGIN_BPS",
        "DEGENBOT_ERC6909_PROFIT",
        "DEGENBOT_REG_PROGRESS_SECS",
    )

    def _cfg(self, monkeypatch: pytest.MonkeyPatch, extra: dict[str, str]) -> ArbitrageConfig:
        # The layer-under-test is the dotenv dict: ambient OS env (the
        # devcontainer exports DEGENBOT_MAX_PATHS) must not leak in.
        for name in self._KNOB_ENVS:
            monkeypatch.delenv(name, raising=False)
        return _cfg(_full_env() | extra, live=False, permutation=None)

    def test_dotenv_layer_values(self, monkeypatch: pytest.MonkeyPatch) -> None:
        cfg = self._cfg(
            monkeypatch,
            {
                "DEGENBOT_MAX_PATHS": "50000",
                "DEGENBOT_MIN_PROFIT_MARGIN_BPS": "25",
                "DEGENBOT_ERC6909_PROFIT": "1",
                "DEGENBOT_REG_PROGRESS_SECS": "15",
            },
        )
        assert cfg.max_registered_paths == 50000
        assert cfg.min_profit_margin_bps == 25
        assert cfg.erc6909_profit is True
        assert cfg.reg_progress_secs == 15.0

    def test_os_env_beats_dotenv(self, monkeypatch: pytest.MonkeyPatch) -> None:
        # Bypass _cfg: the helper strips knob OS env to isolate layers, but
        # THIS test is the OS-env-beats-dotenv layer.
        monkeypatch.setenv("DEGENBOT_MAX_PATHS", "70000")
        cfg = _cfg(_full_env() | {"DEGENBOT_MAX_PATHS": "50000"}, live=False, permutation=None)
        assert cfg.max_registered_paths == 70000

    def test_invalid_numeric_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        with pytest.raises(ValueError, match="DEGENBOT_MAX_PATHS"):
            self._cfg(monkeypatch, {"DEGENBOT_MAX_PATHS": "not-a-number"})

    def test_defaults_when_unset(self, monkeypatch: pytest.MonkeyPatch) -> None:
        cfg = self._cfg(monkeypatch, {})
        assert cfg.max_registered_paths == 100000
        assert cfg.min_profit_margin_bps == 0
        assert cfg.erc6909_profit is False
        assert cfg.reg_progress_secs == 30.0


class TestRpcCascade:
    """from_env delegates to resolve_rpc_uris, so the four-layer cascade applies.

    The config is installed once at FFI module init, so what a per-call
    argument can reach is the explicit ``node`` override and the refusal when
    nothing in the installed config supplies the chain.
    """

    def test_the_explicit_node_override_fills_both_capabilities(self) -> None:
        """One endpoint, classified by the core, serves whichever slot it fits."""
        cfg = _cfg(_full_env(), live=False, permutation=None)

        assert cfg.chain_id == 1
        assert cfg.node_http == _NODE
        assert cfg.node_ws == _NODE

    def test_an_unconfigured_chain_refuses_with_envvar_pointers(self) -> None:
        with pytest.raises(RpcNotConfiguredError) as exc_info:
            _cfg(
                {},
                live=False,
                permutation=None,
                rpc=RpcCascadeOverrides(chain_id=_UNCONFIGURED_CHAIN),
            )

        msg = str(exc_info.value)
        assert f"DEGENBOT_RPC_HTTP_CHAINID_{_UNCONFIGURED_CHAIN}" in msg
        assert "nodes.http" in msg

    def test_a_chain_named_only_by_the_file_refuses_on_the_subscription_scope(
        self,
    ) -> None:
        """A chain the file serves by HTTP alone cannot host a feed."""
        with pytest.raises(RpcNotConfiguredError) as exc_info:
            _cfg(
                {},
                live=False,
                permutation=None,
                rpc=RpcCascadeOverrides(
                    chain_id=_UNCONFIGURED_CHAIN, node="http://only-a-read.example"
                ),
            )

        assert "subscription" in str(exc_info.value)


class TestLegacyNodeHostIgnored:
    """NODE_HOST_*/NODE_PORT_* are retired: the variables are ignored (no warning)."""

    def test_legacy_vars_are_fully_ignored(self) -> None:
        env = _full_env() | {
            "NODE_HOST_HTTP": "https://legacy.example",
            "NODE_PORT_HTTP": "8545",
            "NODE_HOST_WEBSOCKET": "wss://legacy.example",
            "NODE_PORT_WEBSOCKET": "8546",
        }
        # No layer carries them and nothing warns: the variables are not
        # consulted at all, so the override is the only endpoint in play.
        with warnings.catch_warnings():
            warnings.simplefilter("error", DeprecationWarning)
            cfg = _cfg(env, live=False, permutation=None)

        assert cfg.node_http == _NODE
        assert cfg.node_ws == _NODE

    def test_legacy_vars_do_not_reach_the_cascade_without_warnings(self) -> None:
        env = _full_env() | {
            "NODE_HOST_HTTP": "https://legacy.example",
            "NODE_PORT_HTTP": "8545",
        }
        with warnings.catch_warnings():
            warnings.simplefilter("error", DeprecationWarning)
            cfg = _cfg(env, live=False, permutation=None)

        assert cfg.node_http == _NODE


class TestPermutationOverride:
    def test_permutation_string_becomes_singleton_frozenset(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        cfg = _cfg(_full_env(), live=False, permutation="V3-V4-V3")
        assert cfg.permutation_filter == frozenset({"V3-V4-V3"})

    def test_no_permutation_is_none(self, monkeypatch: pytest.MonkeyPatch) -> None:
        cfg = _cfg(_full_env(), live=False, permutation=None)
        assert cfg.permutation_filter is None


class TestExecutorZeroAddress:
    def test_zero_executor_address_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        env = _full_env() | {"EXECUTOR_CONTRACT_ADDRESS": "0x" + "0" * 40}
        with pytest.raises(ValueError, match=r"zero address|EXECUTOR_CONTRACT_ADDRESS"):
            _cfg(env, live=False, permutation=None)


class TestImmutability:
    def test_config_is_frozen(self, monkeypatch: pytest.MonkeyPatch) -> None:
        cfg = _cfg(_full_env(), live=False, permutation=None)
        with pytest.raises(dataclasses.FrozenInstanceError):
            cfg.operator_address = "0x" + "1" * 40  # type: ignore[misc]
