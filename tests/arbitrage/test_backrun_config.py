"""Tests for BackrunConfig — the unified backrun configuration value object.

`BackrunConfig` bundles the ~20 scattered backrun tunables (operator identity,
node endpoints, executor contract, dispatch knobs, path filters, dry-run)
that `main()` currently reads ad-hoc from three sources: a `mainnet.env`
dotenv dict, module-top constants, and CLI args. `from_env` is the factory
that reproduces `main()`'s current env-parsing + defaulting exactly, so the
bridge onto `main()` (slice 5b) is behavior-preserving.

Pure-data: no behavior, no engine/WS/RPC. The fatal `return` paths in
`main()` (missing operator in live mode, zero-address executor) are promoted
to `ValueError` here, since a factory can't "return early".
"""

from __future__ import annotations

import dataclasses

import pytest

from examples.eth_backrun_helpers import BackrunConfig


def _full_env() -> dict[str, str]:
    """A complete env dict exercising every field."""
    return {
        "OPERATOR_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
        "OPERATOR_PRIVATE_KEY": "0x" + "a" * 64,
        "NODE_HOST_HTTP": "https://eth.example.com",
        "NODE_PORT_HTTP": "8545",
        "NODE_HOST_WEBSOCKET": "wss://ws.eth.example.com",
        "NODE_PORT_WEBSOCKET": "8546",
        "EXECUTOR_CONTRACT_ADDRESS": "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5",
        "INJECT_EXECUTOR_CODE": "0",
        "INJECTED_EXECUTOR_ADDRESS": "0x0D6d4c3cF3BD3b769De1821f2BE0d7d99913E4F1",
        "EXECUTOR_OWNER_ADDRESS": "0x9C56a29c7231974c269E24F9FB3c29203039089E",
    }


class TestFromEnvFull:
    def test_full_env_populates_all_fields(self) -> None:
        cfg = BackrunConfig.from_env(_full_env(), live=True, permutation=None)

        assert cfg.dry_run is False
        assert cfg.operator_address == "0x9C56a29c7231974c269E24F9FB3c29203039089E"
        assert cfg.operator_private_key == "0x" + "a" * 64
        assert cfg.node_http == "https://eth.example.com:8545"
        assert cfg.node_ws == "wss://ws.eth.example.com:8546"
        assert cfg.executor_address == "0x543C7eF4F2368a9411c94A055e7236E6Dc6f99D5"
        assert cfg.inject_executor_code is False
        # main() behavior: inject=False keeps the env executor address
        # (EIP-55 canonical checksum casing)
        assert cfg.injected_address == "0x0D6d4C3CF3bD3b769De1821F2Be0D7d99913e4F1"
        assert cfg.executor_owner == "0x9C56a29c7231974c269E24F9FB3c29203039089E"

    def test_inject_code_true_overrides_executor_to_injected(self) -> None:
        env = _full_env() | {"INJECT_EXECUTOR_CODE": "1"}
        cfg = BackrunConfig.from_env(env, live=True, permutation=None)

        assert cfg.inject_executor_code is True
        # main() behavior: when INJECT_EXECUTOR_CODE, executor_address = injected_address
        assert cfg.executor_address == cfg.injected_address


class TestDryRunDefaults:
    def test_dry_run_missing_operator_defaults_to_zero_and_dummy_key(self) -> None:
        env = _full_env() | {"OPERATOR_ADDRESS": "", "OPERATOR_PRIVATE_KEY": ""}
        cfg = BackrunConfig.from_env(env, live=False, permutation=None)

        # dry-run: operator resolves to ZERO_ADDRESS, private_key to 0x00..00
        assert cfg.dry_run is True
        assert cfg.operator_address == "0x" + "0" * 40
        assert cfg.operator_private_key == "0x" + "0" * 64


class TestLiveModeRequiresOperator:
    def test_live_mode_missing_operator_raises(self) -> None:
        env = _full_env() | {"OPERATOR_ADDRESS": "", "OPERATOR_PRIVATE_KEY": ""}
        with pytest.raises(ValueError, match="OPERATOR"):
            BackrunConfig.from_env(env, live=True, permutation=None)


class TestNodeDefaults:
    def test_missing_node_hosts_default_to_localhost(self) -> None:
        env = _full_env() | {
            "NODE_HOST_HTTP": "",
            "NODE_PORT_HTTP": "",
            "NODE_HOST_WEBSOCKET": "",
            "NODE_PORT_WEBSOCKET": "",
        }
        cfg = BackrunConfig.from_env(env, live=False, permutation=None)

        assert cfg.node_http == "http://localhost:8545"
        assert cfg.node_ws == "ws://localhost:8546"


class TestPermutationOverride:
    def test_permutation_string_becomes_singleton_frozenset(self) -> None:
        cfg = BackrunConfig.from_env(_full_env(), live=False, permutation="V3-V4-V3")
        assert cfg.permutation_filter == frozenset({"V3-V4-V3"})

    def test_no_permutation_is_none(self) -> None:
        cfg = BackrunConfig.from_env(_full_env(), live=False, permutation=None)
        assert cfg.permutation_filter is None


class TestExecutorZeroAddress:
    def test_zero_executor_address_raises(self) -> None:
        env = _full_env() | {"EXECUTOR_CONTRACT_ADDRESS": "0x" + "0" * 40}
        with pytest.raises(ValueError, match=r"zero address|EXECUTOR_CONTRACT_ADDRESS"):
            BackrunConfig.from_env(env, live=False, permutation=None)


class TestImmutability:
    def test_config_is_frozen(self) -> None:
        cfg = BackrunConfig.from_env(_full_env(), live=False, permutation=None)
        with pytest.raises(dataclasses.FrozenInstanceError):
            cfg.operator_address = "0x" + "1" * 40  # type: ignore[misc]
