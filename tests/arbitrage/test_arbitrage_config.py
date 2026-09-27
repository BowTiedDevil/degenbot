"""Tests for ArbitrageConfig — the unified arbitrage configuration value object.

`ArbitrageConfig` bundles the ~20 scattered arbitrage tunables (operator identity,
node endpoints, executor contract, dispatch knobs, path filters, dry-run)
that `main()` once read ad-hoc from three sources: the example dotenv
dict, module-top constants, and CLI flags. `from_env` is the factory
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
from tests.helpers import verdict_probe as probe
from tests.helpers.identity_env import identity_env

# A chain id no operator file, environment, or harness sets, so a refusal is
# genuinely the absence of every layer rather than a leak.
_UNCONFIGURED_CHAIN = 988877

# The explicit override layer, used wherever a test only needs the cascade to
# answer: one endpoint, classified by the core, serves both capabilities.
_NODE = "wss://override.example"
_OVERRIDE = RpcCascadeOverrides(chain_id=1, node=_NODE)


def _cfg(env=None, *, live=False, permutation=None, rpc=None) -> ArbitrageConfig:
    """Build a config with the identity installed and the RPC override pinned.

    Every test here is about a non-RPC field, so the endpoint is supplied
    through the explicit layer. The operator/executor identity is installed in
    the process environment for the build, because that is where ``from_env``
    reads it.
    """
    with identity_env(env):
        return ArbitrageConfig.from_env(
            live=live, permutation=permutation, rpc=rpc if rpc is not None else _OVERRIDE
        )


def _full_env() -> dict[str, str]:
    """Operator + executor fields; the RPC override is supplied by :func:`_cfg`."""
    return {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
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

    def test_inject_code_true_overrides_executor_to_injected(self) -> None:
        """The stance swaps in the overlay address the sim injects bytecode at.

        """
        values = probe.config_values(
            ["inject_executor_code", "executor_address", "injected_address"],
            identity=_full_env(),
            env={"DEGENBOT_INJECT_EXECUTOR_CODE": "1"},
        )

        assert values["inject_executor_code"] is True
        assert values["executor_address"] == values["injected_address"]

    def test_live_with_injection_is_refused(self) -> None:
        """A live run cannot inject: the bytecode exists only in the overlay.

        """
        completed = probe.run(
            probe.build_config_code([], identity=_full_env(), live=True),
            env={"DEGENBOT_INJECT_EXECUTOR_CODE": "1"},
        )

        assert completed.returncode != 0, "live mode with injection active must refuse"
        assert "injection stance is active" in completed.stderr, completed.stderr


class TestInjectExecutorCodeUnifiedResolution:
    """One declared key, one name, and the bare spelling refused everywhere.

    The retired arrangement read the bare name from two layers with opposite
    answers (authoritative in the dotenv mapping, a hard error in the OS
    environment), so a dotenv-only ``INJECT_EXECUTOR_CODE=1`` produced a bot
    that booted live and never submitted. The stance is the declared
    ``simulation.inject_executor_code`` key now, and the bare name is refused
    in the process environment, the only layer left that can carry it.

    """

    _LEGACY = "INJECT_EXECUTOR_CODE"
    _TYPED = "DEGENBOT_INJECT_EXECUTOR_CODE"

    def test_the_bare_name_is_refused_from_the_os_environment(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.setenv(self._LEGACY, "1")

        with pytest.raises(ValueError, match=self._TYPED):
            _cfg({}, live=False, permutation=None)

    def test_the_refusal_names_the_replacement_and_the_divergence(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """The message must say what to set and what the old name cost."""
        monkeypatch.setenv(self._LEGACY, "0")

        with pytest.raises(ValueError, match=self._TYPED) as excinfo:
            _cfg({}, live=False, permutation=None)

        message = str(excinfo.value)

        assert self._TYPED in message
        assert "simulation.inject_executor_code" in message
        assert "never submitted" in message

    def test_the_declared_key_resolves_from_the_typed_env_name(self) -> None:
        """The honored spelling is the typed key env name, through the env layer.

        """
        values = probe.config_values(
            ["inject_executor_code"],
            identity=_full_env(),
            env={self._TYPED: "1"},
        )

        assert values["inject_executor_code"] is True

    def test_the_declared_key_reaches_the_operator_file_too(self) -> None:
        """File-layer reach is new: the typed name was OS-only before.

        A declared key an operator wrote in their file used to be ignored
        because only the OS environment carried the typed spelling, which is
        the same class of defect as the dotenv-only divergence above.

        """
        with probe.operator_file("[simulation]\ninject_executor_code = true\n") as written:
            values = probe.config_values(["inject_executor_code"], operator_file=written)

        assert values["inject_executor_code"] is True

    def test_unset_everywhere_defaults_to_no_injection(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        monkeypatch.delenv(self._LEGACY, raising=False)
        monkeypatch.delenv(self._TYPED, raising=False)

        cfg = _cfg(_full_env(), live=False, permutation=None)

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
    """The declared driver stances resolve through the core cascade.

    Each knob is a ``config_schema!`` key now, so a value reaches the config
    from the environment OR the operator file, with the declared default as the
    only fallback — and a value the schema cannot parse is refused at boot by
    the loader rather than by this layer.

    The retired ``examples/mainnet.env`` is not one of those layers: it held
    the ``OPERATOR_*``/``EXECUTOR_*`` identity keys only, and a ``DEGENBOT_*``
    name left in it changed nothing. Identity now comes from the process
    environment.

    """

    _KNOB_FIELDS = (
        "max_registered_paths",
        "min_profit_margin_bps",
        "erc6909_profit",
        "reg_progress_secs",
    )

    def _probe(self, **env: str) -> dict[str, object]:
        """Build the config in a child that declares exactly this env."""
        return probe.config_values(self._KNOB_FIELDS, env=env)

    def test_the_env_layer_reaches_every_knob(self) -> None:
        """One OS export each, all four honored."""
        values = self._probe(
            DEGENBOT_MAX_PATHS="50000",
            DEGENBOT_MIN_PROFIT_MARGIN_BPS="25",
            DEGENBOT_ERC6909_PROFIT="1",
            DEGENBOT_REG_PROGRESS_SECS="15",
        )

        assert values["max_registered_paths"] == 50000
        assert values["min_profit_margin_bps"] == 25
        assert values["erc6909_profit"] is True
        assert values["reg_progress_secs"] == pytest.approx(15.0)

    def test_the_file_layer_reaches_every_knob(self) -> None:
        """The same four keys, written as an operator file.

        The dotenv-only half of the old chain could not do this: the defaults
        lived beside the reader, and the file the operator edits was not a
        layer at all.

        """
        with probe.operator_file(
            "[dispatch]\nmin_profit_margin_bps = 30\nerc6909_profit = true\n"
            "[pathfinding]\nmax_registered_paths = 40000\nreg_progress_secs = 12.5\n"
        ) as written:
            values = probe.config_values(self._KNOB_FIELDS, operator_file=written)

        assert values["max_registered_paths"] == 40000
        assert values["min_profit_margin_bps"] == 30
        assert values["erc6909_profit"] is True
        assert values["reg_progress_secs"] == pytest.approx(12.5)

    def test_the_env_layer_beats_the_file_layer(self) -> None:
        """The cascade order, proven on a declared key."""
        with probe.operator_file("[pathfinding]\nmax_registered_paths = 40000\n") as written:
            values = probe.config_values(
                ["max_registered_paths"],
                env={"DEGENBOT_MAX_PATHS": "70000"},
                operator_file=written,
            )

        assert values["max_registered_paths"] == 70000

    def test_a_bad_value_is_refused_at_boot(self) -> None:
        """A typo'd knob is a loud refusal, not a silent default.

        The refusal is the typed error the hypothetical entry returns: the
        loader owns the parse, and the module-init boot path turns the same
        error into an exit(2) wrapper. Asked in-process, it names the key and
        the unparsable value without spawning a process.
        """
        from degenbot import _ffi

        with pytest.raises(ValueError, match="pathfinding.max_registered_paths") as excinfo:
            _ffi.resolve_hypothetical({"DEGENBOT_MAX_PATHS": "not-a-number"}, None)

        assert "not-a-number" in str(excinfo.value)


    def test_the_declared_defaults_apply_when_no_layer_supplies_a_knob(self) -> None:
        values = self._probe()

        assert values["max_registered_paths"] == 100000
        assert values["min_profit_margin_bps"] == 0
        assert values["erc6909_profit"] is False
        assert values["reg_progress_secs"] == pytest.approx(30.0)


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


class TestLiveModeRefusesPlaceholderKey:
    """A live run must never sign with a key the repo already publishes.

    The module ships two known placeholders: the dry-run throwaway key and the
    all-zero scalar. Either one reaching live mode is an operator who never
    replaced the placeholder, and the bot would submit signed transactions
    under a key anyone can read.
    """

    _DRY_RUN_KEY = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

    def test_live_mode_refuses_the_dry_run_throwaway_key(self) -> None:
        env = _full_env() | {"OPERATOR_PRIVATE_KEY": self._DRY_RUN_KEY}
        with pytest.raises(ValueError, match="placeholder"):
            _cfg(env, live=True, permutation=None)

    def test_live_mode_refuses_the_all_zero_key(self) -> None:
        env = _full_env() | {"OPERATOR_PRIVATE_KEY": "0x" + "0" * 64}
        with pytest.raises(ValueError, match="placeholder"):
            _cfg(env, live=True, permutation=None)
