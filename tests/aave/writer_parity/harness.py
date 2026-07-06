"""Shared harness for the Aave writer §4.2 parity tests (U5YIBG).

The offline mock-RPC live-vs-live harness (ADR-005 §4.2). Both the Rust driver
(``degenbot_rs.run_aave_update``) + the Python oracle
(``cli/aave/commands.py::update_aave_market``) are driven against the SAME mock
JSON-RPC server serving canned ``eth_getLogs`` + ``eth_blockNumber`` responses,
into two identically-seeded temp SQLite DBs. The resulting ``aave_*`` rows are
compared byte-for-byte.

Per §4.3, the parity tests here are TEMPORARY: once GREEN, CZM7TI retires the
Python oracle + these tests together. The Rust ``#[cfg(test)]`` corpus in
``write.rs`` stays permanent.
"""

from __future__ import annotations

import json
import threading
from contextlib import contextmanager
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import TYPE_CHECKING, Any

from sqlalchemy import create_engine, text
from sqlalchemy.orm import Session

from degenbot.degenbot_rs import db_upgrade_database

if TYPE_CHECKING:
    from collections.abc import Generator
    from pathlib import Path

# The known contract addresses the seeded market + fixture logs use. Both the
# Rust fetch spec + the Python fetchers filter on these.
POOL_ADDRESS = "0x" + "11" * 20
POOL_CONFIGURATOR_ADDRESS = "0x" + "22" * 20
POOL_ADDRESS_PROVIDER_ADDRESS = "0x" + "33" * 20
POOL_DATA_PROVIDER_ADDRESS = "0x" + "73" * 20
PRICE_ORACLE_ADDRESS = "0x" + "44" * 20
GHO_TOKEN_ADDRESS = "0x" + "55" * 20
GHO_VTOKEN_ADDRESS = "0x" + "95" * 20
ZERO_ADDRESS = "0x" + "00" * 20

# The seeded asset's erc20 parents (underlying/a_token/v_token) + the
# pre-seeded user (the Python oracle `assert user is not None` requires the
# user to pre-exist; the Rust `get_or_create_user` is a no-op on it).
UNDERLYING_ADDRESS = "0x" + "66" * 20
A_TOKEN_ADDRESS = "0x" + "77" * 20
V_TOKEN_ADDRESS = "0x" + "88" * 20
USER_ADDRESS = "0x" + "ab" * 20

# The canonical Aave V3 UserEModeSet topic0
# (keccak of "UserEModeSet(address,uint8)").
USER_E_MODE_SET_TOPIC = "0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84"

# The bootstrap stamp — the seeded market's `last_update_block`. Both paths
# start from `stamp + 1` (flag #4: bootstrap the stamp so both paths align).
BOOTSTRAP_BLOCK = 1000
# The fixture events live at this block (one chunk, one event).
FIXTURE_BLOCK = 1001


def _hex(n: int) -> str:
    """Encode a non-negative int as a JSON-RPC hex string (`0x...`)."""
    return hex(n)


def _u256(value: int) -> str:
    """Encode a non-negative int as a 32-byte hex word (the ABI uint256 shape)."""
    return "0x" + value.to_bytes(32, "big").hex()


def _pad_address(addr: str) -> str:
    """Pad a 20-byte address to a 32-byte topic word."""
    return "0x" + addr[2:].rjust(64, "0").lower()


def _make_log(
    *,
    address: str,
    topics: list[str],
    data: str,
    block: int,
    log_index: int,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned alloy/web3-shaped `eth_getLogs` entry.

    `tx_hash` defaults to a fixed value (the single-tx-group fixture shape);
    pass a distinct hash per tx group for multi-tx-within-chunk fixtures
    (the fetch spec groups logs by `transactionHash`).
    """
    tx_hash = tx_hash or "0x" + (b"tx" + b"\x00" * 30).hex()
    block_hash = "0x" + (b"bk" + b"\x00" * 30).hex()
    return {
        "address": address,
        "topics": topics,
        "data": data,
        "blockNumber": _hex(block),
        "logIndex": _hex(log_index),
        "transactionIndex": _hex(log_index),
        "transactionHash": tx_hash,
        "blockHash": block_hash,
        "removed": False,
    }


def make_user_e_mode_set_log(
    *,
    user_address: str,
    category_id: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a UserEModeSet event."""
    _ = tx_index
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[USER_E_MODE_SET_TOPIC, _pad_address(user_address)],
        data=_u256(category_id),
        block=block,
        log_index=log_index,
    )


# ReserveUsedAsCollateral{Enabled,Disabled}(address indexed reserve,
# address indexed user) — topic0 is the signature hash, topic1 is the reserve
# (the asset's underlying address), topic2 is the user. No data.
_RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC = (
    "0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2"
)
_RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC = (
    "0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd"
)


def make_reserve_used_as_collateral_log(
    *,
    user_address: str,
    reserve_address: str,
    enabled: bool,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a ReserveUsedAsCollateral event.

    `enabled=True` → the `...Enabled` topic0; `False` → the `...Disabled` topic0.
    """
    topic0 = (
        _RESERVE_USED_AS_COLLATERAL_ENABLED_TOPIC
        if enabled
        else _RESERVE_USED_AS_COLLATERAL_DISABLED_TOPIC
    )
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[topic0, _pad_address(reserve_address), _pad_address(user_address)],
        data="0x",
        block=block,
        log_index=log_index,
    )


# ReserveDataUpdated(address indexed reserve, uint256 liquidityRate,
# uint256 stableBorrowRate, uint256 variableBorrowRate, uint256 liquidityIndex,
# uint256 variableBorrowIndex) — topic0 is the signature hash, topic1 is the
# reserve. The data is 5 uint256 words (stableBorrowRate is decoded then
# discarded — deprecated on Aave V3).
_RESERVE_DATA_UPDATED_TOPIC = "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a"


def make_reserve_data_updated_log(
    *,
    reserve_address: str,
    liquidity_rate: int,
    stable_borrow_rate: int,
    variable_borrow_rate: int,
    liquidity_index: int,
    variable_borrow_index: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a ReserveDataUpdated event.

    The data is the 5 non-indexed uint256 words concatenated (96 bytes).
    `stable_borrow_rate` is included (the Python decodes all 5 then discards it;
    the Rust skips the 2nd word).
    """
    words = [
        liquidity_rate,
        stable_borrow_rate,
        variable_borrow_rate,
        liquidity_index,
        variable_borrow_index,
    ]
    data = "0x" + "".join(v.to_bytes(32, "big").hex() for v in words)
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[_RESERVE_DATA_UPDATED_TOPIC, _pad_address(reserve_address)],
        data=data,
        block=block,
        log_index=log_index,
    )


# CollateralConfigurationChanged(address indexed asset, uint256 ltv,
# uint256 liquidationThreshold, uint256 liquidationBonus) — emitted by the
# Pool Configurator (so the fixture log's address is POOL_CONFIGURATOR).
# Both paths IGNORE the event data (ltv/lt/bonus) + RPC-fetch the full bitmap
# via `getConfiguration(address)` (the comment in dispatch says a pool upgrade
# can emit stale event values).
_COLLATERAL_CONFIGURATION_CHANGED_TOPIC = (
    "0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995"
)
# The `getConfiguration(address)` 4-byte selector — both paths issue this
# `eth_call` against the Pool contract (keyed by the calldata selector in the
# mock's `eth_call_responses`). Hardcoded (keccak of the signature, first 4
# bytes) to avoid computing at import time.
GET_CONFIGURATION_SELECTOR = "0xc44b11f7"
# The no-arg `*_REVISION()` eth_call selectors — both paths RPC these against
# the PoolUpdated/PoolConfiguratorUpdated `newAddress` to read the contract
# revision (keyed by selector in the mock's `eth_call_responses`).
POOL_REVISION_SELECTOR = "0x0148170e"
CONFIGURATOR_REVISION_SELECTOR = "0x7af635a6"
ATOKEN_REVISION_SELECTOR = "0x0bd7ad3b"
DEBT_TOKEN_REVISION_SELECTOR = "0xb9a7b622"  # noqa: S105
# `getSourceOfAsset(address)` — the PriceOracle RPC the ReserveInitialized
# handler calls to set the asset's initial price_source (keyed by selector).
GET_SOURCE_OF_ASSET_SELECTOR = "0x92bf2be0"


def make_collateral_configuration_changed_log(
    *,
    asset_address: str,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a CollateralConfigurationChanged.

    The event data (ltv/lt/bonus) is included for shape realism but ignored by
    both paths (they RPC-fetch the full bitmap).
    """
    return _make_log(
        address=POOL_CONFIGURATOR_ADDRESS.lower(),
        topics=[_COLLATERAL_CONFIGURATION_CHANGED_TOPIC, _pad_address(asset_address)],
        # 3 dummy words (ltv/lt/bonus) — ignored by both paths.
        data="0x" + (0).to_bytes(96, "big").hex(),
        block=block,
        log_index=log_index,
    )


# ── the 7 pure-decode config-event fixtures (no eth_call mock needed) ──────
# EModeCategoryAdded(uint8 indexed categoryId, uint256 ltv, uint256
# liquidationThreshold, uint256 liquidationBonus, address oracle, string label)
# — emitted by the Pool Configurator. The `oracle` data word + the dynamic
# `string label` are abi-encoded (the string needs the offset+length+data
# tail; `eth_abi.encode` builds it cleanly).
_EMODE_CATEGORY_ADDED_TOPIC = "0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb"


def make_e_mode_category_added_log(
    *,
    category_id: int,
    ltv: int,
    liquidation_threshold: int,
    liquidation_bonus: int,
    oracle_address: str | None,
    label: str,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an EModeCategoryAdded event."""
    import eth_abi

    data = (
        "0x"
        + eth_abi.abi.encode(
            ["uint256", "uint256", "uint256", "address", "string"],
            [ltv, liquidation_threshold, liquidation_bonus, oracle_address or ZERO_ADDRESS, label],
        ).hex()
    )
    return _make_log(
        address=POOL_CONFIGURATOR_ADDRESS.lower(),
        topics=[_EMODE_CATEGORY_ADDED_TOPIC, _u256(category_id)],
        data=data,
        block=block,
        log_index=log_index,
    )


# EModeAssetCategoryChanged(address indexed asset, uint8 oldCategoryId,
# uint8 newCategoryId) — emitted by the Pool Configurator (older Aave).
_EMODE_ASSET_CATEGORY_CHANGED_TOPIC = (
    "0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264"
)


def make_e_mode_asset_category_changed_log(
    *,
    asset_address: str,
    new_category_id: int,
    old_category_id: int = 0,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an EModeAssetCategoryChanged."""
    return _make_log(
        address=POOL_CONFIGURATOR_ADDRESS.lower(),
        topics=[_EMODE_ASSET_CATEGORY_CHANGED_TOPIC, _pad_address(asset_address)],
        data="0x"
        + old_category_id.to_bytes(32, "big").hex()
        + new_category_id.to_bytes(32, "big").hex(),
        block=block,
        log_index=log_index,
    )


# AssetCollateralInEModeChanged(address indexed asset, uint8 categoryId,
# bool collateral) — emitted by the Pool Configurator (newer Aave 3.4+).
_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC = (
    "0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098"
)


def make_asset_collateral_in_emode_changed_log(
    *,
    asset_address: str,
    category_id: int,
    is_collateral: bool,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an AssetCollateralInEModeChanged."""
    return _make_log(
        address=POOL_CONFIGURATOR_ADDRESS.lower(),
        topics=[_ASSET_COLLATERAL_IN_EMODE_CHANGED_TOPIC, _pad_address(asset_address)],
        data="0x"
        + category_id.to_bytes(32, "big").hex()
        + (1 if is_collateral else 0).to_bytes(32, "big").hex(),
        block=block,
        log_index=log_index,
    )


# PriceOracleUpdated(address indexed oldAddress, address indexed newAddress) —
# emitted by the PoolAddressesProvider.
_PRICE_ORACLE_UPDATED_TOPIC = "0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd"


def make_price_oracle_updated_log(
    *,
    new_oracle_address: str,
    old_oracle_address: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a PriceOracleUpdated event."""
    return _make_log(
        address=POOL_ADDRESS_PROVIDER_ADDRESS.lower(),
        topics=[
            _PRICE_ORACLE_UPDATED_TOPIC,
            _pad_address(old_oracle_address),
            _pad_address(new_oracle_address),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# AddressSet(bytes32 indexed id, address indexed oldAddress, address indexed
# newAddress) — emitted by the PoolAddressesProvider. `contract_id` is the
# right-padded-ASCII bytes32 identifier (e.g. b"POOL_DATA_PROVIDER").
_ADDRESS_SET_TOPIC = "0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7"


def make_address_set_log(
    *,
    contract_id: str,
    new_address: str,
    old_address: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an AddressSet event.

    `contract_id` is the ASCII identifier (right-padded to 32 bytes — both
    paths strip the trailing NULs to recover the name).
    """
    return _make_log(
        address=POOL_ADDRESS_PROVIDER_ADDRESS.lower(),
        topics=[
            _ADDRESS_SET_TOPIC,
            "0x" + contract_id.encode("ascii").ljust(32, b"\x00").hex(),
            _pad_address(old_address),
            _pad_address(new_address),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# PoolDataProviderUpdated(address indexed oldAddress, address indexed
# newAddress) — emitted by the PoolAddressesProvider.
_POOL_DATA_PROVIDER_UPDATED_TOPIC = (
    "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d"
)


def make_pool_data_provider_updated_log(
    *,
    new_address: str,
    old_address: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a PoolDataProviderUpdated event."""
    return _make_log(
        address=POOL_ADDRESS_PROVIDER_ADDRESS.lower(),
        topics=[
            _POOL_DATA_PROVIDER_UPDATED_TOPIC,
            _pad_address(old_address),
            _pad_address(new_address),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# PoolUpdated(address indexed oldAddress, address indexed newAddress) —
# emitted by the PoolAddressesProvider. Both paths RPC `POOL_REVISION()` on
# the new address + UPDATE the `POOL` contract row's `revision` (NOT address —
# the proxy address is stable).
_POOL_UPDATED_TOPIC = "0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627"


def make_pool_updated_log(
    *,
    new_address: str,
    old_address: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a PoolUpdated event."""
    return _make_log(
        address=POOL_ADDRESS_PROVIDER_ADDRESS.lower(),
        topics=[
            _POOL_UPDATED_TOPIC,
            _pad_address(old_address),
            _pad_address(new_address),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# PoolConfiguratorUpdated(address indexed oldAddress, address indexed newAddress)
# — emitted by the PoolAddressesProvider. RPC `CONFIGURATOR_REVISION()` on the
# new address + UPDATE the `POOL_CONFIGURATOR` contract row's `revision`.
_POOL_CONFIGURATOR_UPDATED_TOPIC = (
    "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465"
)


def make_pool_configurator_updated_log(
    *,
    new_address: str,
    old_address: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a PoolConfiguratorUpdated event."""
    return _make_log(
        address=POOL_ADDRESS_PROVIDER_ADDRESS.lower(),
        topics=[
            _POOL_CONFIGURATOR_UPDATED_TOPIC,
            _pad_address(old_address),
            _pad_address(new_address),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# ReserveInitialized(address indexed asset, address indexed aToken,
#   address stableDebtToken, address variableDebtToken,
#   address interestRateStrategyAddress) — emitted by the PoolConfigurator.
# Both paths create 3 erc20 rows (`asset`, `aToken`, `vToken`) + a new
# `aave_v3_assets` row (a/v token revisions via EIP-1967 → ATOKEN_REVISION/
# DEBT_TOKEN_REVISION eth_calls + price_source via getSourceOfAsset eth_call).
# `stableDebtToken` + `interestRateStrategyAddress` are ignored by both paths.
_RESERVE_INITIALIZED_TOPIC = "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f"


def make_reserve_initialized_log(
    *,
    asset: str,
    a_token: str,
    v_token: str,
    stable_debt_token: str = ZERO_ADDRESS,
    interest_rate_strategy: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a ReserveInitialized event."""
    data = "0x" + (
        _pad_address(stable_debt_token)[2:]
        + _pad_address(v_token)[2:]
        + _pad_address(interest_rate_strategy)[2:]
    )
    return _make_log(
        address=POOL_CONFIGURATOR_ADDRESS.lower(),
        topics=[
            _RESERVE_INITIALIZED_TOPIC,
            _pad_address(asset),
            _pad_address(a_token),
        ],
        data=data,
        block=block,
        log_index=log_index,
    )


# Upgraded(address indexed implementation) — emitted by an aToken/vToken proxy.
# Both paths look up the asset by the proxy address (a_token first, then v_token)
# + RPC `ATOKEN_REVISION()`/`DEBT_TOKEN_REVISION()` on the new implementation.
# This is the ScaledTokenUpgrade config-row identity test (the v_token_revision
# bump converges across paths; the ops-parser drift is flag #1, a separate
# concern). For the deprecation side effect the upgraded vToken must BE the GHO
# vToken — the seeded asset's v_token is NOT, so `deprecated_gho_token_id` is
# None + only the revision column updates.
_UPGRADED_TOPIC = "0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b"


def make_upgraded_log(
    *,
    proxy_address: str,
    new_implementation: str,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an Upgraded event.

    `proxy_address` is the aToken/vToken proxy (the emitter); `new_implementation`
    is the indexed implementation (the RPC target for `*_REVISION()`).
    """
    return _make_log(
        address=proxy_address.lower(),
        topics=[_UPGRADED_TOPIC, _pad_address(new_implementation)],
        data="0x",
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


# newDiscountToken)` — emitted by the GHO variable-debt token. The Python
# validates the emitter == the GHO vToken address; emit from GHO_VTOKEN_ADDRESS.
_DISCOUNT_TOKEN_UPDATED_TOPIC = "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529"  # noqa: S105


def make_discount_token_updated_log(
    *,
    new_discount_token: str,
    old_discount_token: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    return _make_log(
        address=GHO_VTOKEN_ADDRESS.lower(),
        topics=[
            _DISCOUNT_TOKEN_UPDATED_TOPIC,
            _pad_address(old_discount_token),
            _pad_address(new_discount_token),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# GHO `DiscountRateStrategyUpdated(address indexed oldDiscountRateStrategy,
# address indexed newDiscountRateStrategy)` — emitted by the GHO vToken.
_DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC = (
    "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef"
)


def make_discount_rate_strategy_updated_log(
    *,
    new_strategy: str,
    old_strategy: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    return _make_log(
        address=GHO_VTOKEN_ADDRESS.lower(),
        topics=[
            _DISCOUNT_RATE_STRATEGY_UPDATED_TOPIC,
            _pad_address(old_strategy),
            _pad_address(new_strategy),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# GHO `DiscountPercentUpdated(address indexed user, uint256 oldDiscountPercent,
# uint256 indexed newDiscountPercent)` — emitted by the GHO vToken. The
# `newDiscountPercent` is topic[2] (indexed); `oldDiscountPercent` is the
# non-indexed data word. The Rust config path's `dispatch_discount_percent_updated`
# builds a `GhoDiscountPercentUpdated` chunk event → `apply_gho_discount_percent_updated`
# writes `aave_v3_users.gho_discount = newDiscountPercent` (the U5YIBG #5
# Rust-written column the `verify_gho_discount_amounts` invariant reads).
_DISCOUNT_PERCENT_UPDATED_TOPIC = (
    "0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956"
)


def make_discount_percent_updated_log(
    *,
    user_address: str,
    new_discount_percent: int,
    old_discount_percent: int = 0,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a GHO DiscountPercentUpdated event."""
    return _make_log(
        address=GHO_VTOKEN_ADDRESS.lower(),
        topics=[
            _DISCOUNT_PERCENT_UPDATED_TOPIC,
            _pad_address(user_address),
            "0x" + format(new_discount_percent, "064x"),
        ],
        data="0x" + format(old_discount_percent, "064x"),
        block=block,
        log_index=log_index,
    )


# AssetSourceUpdated(address indexed asset, address indexed source) — emitted
# by the AaveOracle contract (the PRICE_ORACLE row's address).
_ASSET_SOURCE_UPDATED_TOPIC = "0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1"


def make_asset_source_updated_log(
    *,
    asset_address: str,
    source_address: str,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an AssetSourceUpdated event."""
    return _make_log(
        address=PRICE_ORACLE_ADDRESS.lower(),
        topics=[
            _ASSET_SOURCE_UPDATED_TOPIC,
            _pad_address(asset_address),
            _pad_address(source_address),
        ],
        data="0x",
        block=block,
        log_index=log_index,
    )


# ── GHO ops-parser fixtures (YTRUEW — the multi-tx-within-chunk flag #1 proof) ─
# The Pool `Borrow` / `Repay` events + the variableDebtToken `Mint` / `Burn`
# (scaled-token) events + the plain ERC20 `Transfer` companion the ops parser
# asserts (the Borrow requires a Transfer from ZERO_ADDRESS to the borrower on
# the borrowed token, matching the debt-mint amount). Each builder takes a
# `tx_hash` so a fixture can place several tx groups in ONE chunk.
#
# Topic0 hashes (keccak256 of the canonical Aave V3 / ERC20 event signatures).
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
REPAY_TOPIC = "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051"
MINT_TOPIC = "0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"
BURN_TOPIC = "0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90"
ERC20_TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"


def make_borrow_log(
    *,
    reserve: str,
    on_behalf_of: str,
    amount: int,
    user: str | None = None,
    interest_rate_mode: int = 2,
    borrow_rate: int = 0,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a Pool `Borrow` event.

    `Borrow(address indexed reserve, address user, address indexed onBehalfOf,
    uint256 amount, uint8 interestRateMode, uint256 borrowRate,
    uint16 indexed referralCode)` — emitted by the Pool. topics: [sig, reserve,
    user, onBehalfOf]; data: abi.encode(user, amount, mode, rate) (4 words).
    Both paths read reserve=topic1, onBehalfOf=topic3, amount=data word 1.
    """
    user = user or on_behalf_of
    words = [
        _pad_address(user)[2:],
        _u256(amount)[2:],
        interest_rate_mode.to_bytes(32, "big").hex(),  # uint8 in 32 bytes
        borrow_rate.to_bytes(32, "big").hex(),
    ]
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[
            BORROW_TOPIC,
            _pad_address(reserve),
            _pad_address(user),
            _pad_address(on_behalf_of),
        ],
        data="0x" + "".join(words),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_repay_log(
    *,
    reserve: str,
    user: str,
    amount: int,
    repayer: str | None = None,
    use_a_tokens: bool = False,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a Pool `Repay` event.

    `Repay(address indexed reserve, address indexed user, address indexed
    repayer, uint256 amount, bool useATokens)` — emitted by the Pool. topics:
    [sig, reserve, user, repayer]; data: abi.encode(amount, useATokens) (2 words).
    Both paths read reserve=topic1, user=topic2, amount=data word 0.
    """
    repayer = repayer or user
    data = _u256(amount)[2:] + (1 if use_a_tokens else 0).to_bytes(32, "big").hex()
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[REPAY_TOPIC, _pad_address(reserve), _pad_address(user), _pad_address(repayer)],
        data="0x" + data,
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_scaled_token_mint_log(
    *,
    token_address: str,
    on_behalf_of: str,
    value: int,
    balance_increase: int = 0,
    index: int,
    caller: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a variableDebtToken `Mint` event.

    `Mint(address indexed caller, address indexed onBehalfOf, uint256 value,
    uint256 balanceIncrease, uint256 index)` — emitted by the vToken. topics:
    [sig, caller, onBehalfOf]; data: abi.encode(value, balanceIncrease, index).
    """
    words = [_u256(value)[2:], _u256(balance_increase)[2:], _u256(index)[2:]]
    return _make_log(
        address=token_address.lower(),
        topics=[MINT_TOPIC, _pad_address(caller), _pad_address(on_behalf_of)],
        data="0x" + "".join(words),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_scaled_token_burn_log(
    *,
    token_address: str,
    from_address: str,
    value: int,
    balance_increase: int = 0,
    index: int,
    target: str = ZERO_ADDRESS,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a variableDebtToken `Burn` event.

    `Burn(address indexed from, address indexed target, uint256 value,
    uint256 balanceIncrease, uint256 index)` — emitted by the vToken. topics:
    [sig, from, target]; data: abi.encode(value, balanceIncrease, index).
    """
    words = [_u256(value)[2:], _u256(balance_increase)[2:], _u256(index)[2:]]
    return _make_log(
        address=token_address.lower(),
        topics=[BURN_TOPIC, _pad_address(from_address), _pad_address(target)],
        data="0x" + "".join(words),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_erc20_transfer_log(
    *,
    token_address: str,
    from_address: str,
    to_address: str,
    value: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a plain ERC20 `Transfer` event.

    `Transfer(address indexed from, address indexed to, uint256 value)` —
    emitted by any ERC20. topics: [sig, from, to]; data: abi.encode(value).
    The ops parser asserts a Borrow has a matching Transfer from ZERO_ADDRESS
    to the borrower (the minted-underlying companion).
    """
    return _make_log(
        address=token_address.lower(),
        topics=[ERC20_TRANSFER_TOPIC, _pad_address(from_address), _pad_address(to_address)],
        data=_u256(value),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def seed_gho_asset(
    session: Session,
    *,
    market_id: int = 1,
    chain_id: int = 1,
    v_token_revision: int = 3,
    gho_discount_percent: int = 1000,
) -> None:
    """Seed the GHO debt asset + a pre-seeded user with a GHO discount.

    seed the GHO underlying asset row
    (aave_v3_assets) with `underlying_asset_id` = the GHO token erc20 (id 1),
    `v_token_id` = the GHO vToken erc20 (id 5), + `v_token_revision` as given
    (the pre-upgrade rev the discount pre-pass reads at chunk-start). The
    companion ERC20 `Transfer` (the Borrow's minted-debt companion the ops
    parser asserts) is emitted by the GHO vToken itself — both paths classify it
    as a debt transfer (the Rust matches by from/target/amount; the Python
    asserts `event_type ∈ {DEBT_*, ERC20_DEBT_*, GHO_DEBT_*}`).

    `gho_discount_percent` seeds the user's `aave_v3_users.gho_discount` (the
    path #1 DB-cache the discount pre-pass reads). Non-zero so the
    discount-active (pre-upgrade) vs deprecated (post-upgrade) divergence the
    fixture exercises is observable.
    """
    from degenbot.checksum_cache import get_checksum_address

    # A phantom GHO aToken erc20 (id 6) — the GHO asset's a_token_id. Kept
    # distinct from the GHO token (id 1, the underlying) so the GHO token is
    # NOT fetched as an aToken. The companion Transfer is emitted by the GHO
    # vToken (a debt token) so both paths classify it as a debt transfer.
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (6, :c, :a)"),
        {"c": chain_id, "a": get_checksum_address("0x" + "56" * 20)},
    )
    seed_gho_vtoken(session, chain_id=chain_id)
    # The GHO underlying asset (id 2). a_token_id = phantom GHO aToken (id 6);
    # v_token_id = the GHO vToken erc20 (id 5) + the pre-upgrade rev.
    session.execute(
        text(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, "
            "a_token_id, a_token_revision, v_token_id, v_token_revision, "
            "e_mode_category_id, price_source, last_update_block, "
            "liquidity_index, liquidity_rate, borrow_index, borrow_rate) "
            "VALUES (2, :market, 1, 6, 1, 5, :rev, NULL, NULL, NULL, 0, 0, 0, 0)"
        ),
        {"market": market_id, "rev": v_token_revision},
    )
    # The pre-seeded user with a GHO discount (path #1 DB-cache).
    session.execute(
        text(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, "
            "gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, "
            "isolation_mode_debt) "
            "VALUES (1, :market, :addr, 0, :disc, NULL, NULL, '0')"
        ),
        {
            "market": market_id,
            "addr": get_checksum_address(USER_ADDRESS),
            "disc": gho_discount_percent,
        },
    )
    session.flush()


def dump_debt_position_rows(session: Session) -> list[dict[str, Any]]:
    """Dump `aave_v3_debt_positions` rows as comparable dicts (the GHO debt
    balance + last_index the ops parser writes — the byte-IDENTITY target for
    the flag #1 fixture)."""
    rows = session.execute(
        text(
            "SELECT id, user_id, asset_id, balance, last_index "
            "FROM aave_v3_debt_positions ORDER BY id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


@dataclass
class MockRpcRegistry:
    """Holds the canned RPC responses for the parity mock server.

    `logs` is the full list of canned `eth_getLogs` entries; the server filters
    by the requested `address` + `topics[0]` group. `block_number` is the tip
    served for `eth_blockNumber`. `eth_call_responses` maps a calldata selector
    (the first 4 bytes, e.g. `"0xc44b11f7"` for `getConfiguration(address)`)
    to the canned 32-byte return value — the server dispatches `eth_call`s by
    selector.
    """

    block_number: int
    logs: list[dict[str, Any]] = field(default_factory=list)
    eth_call_responses: dict[str, str] = field(default_factory=dict)


class _MockRpcHandler(BaseHTTPRequestHandler):
    """Serves canned `eth_getLogs` + `eth_blockNumber` (+ a generic fallback)."""

    registry: MockRpcRegistry  # set as a class attr by `mock_rpc_server`

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        req = json.loads(body) if body else {}
        # alloy may batch (a list of requests) — handle both shapes.
        if isinstance(req, list):
            responses = [self._handle_one(r) for r in req]
            payload = json.dumps(responses).encode()
        else:
            payload = json.dumps(self._handle_one(req)).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _handle_one(self, req: dict[str, Any]) -> dict[str, Any]:
        method = req.get("method")
        req_id = req.get("id")
        if not isinstance(method, str):
            method = ""
        result = self._canned_result(method, req)
        return {"jsonrpc": "2.0", "id": req_id, "result": result}

    def _canned_result(self, method: str, req: dict[str, Any]) -> Any:
        if method == "eth_blockNumber":
            return _hex(self.registry.block_number)
        if method == "eth_getLogs":
            return self._filtered_logs(req)
        if method == "eth_call":
            return self._eth_call_result(req)
        if method == "eth_chainId":
            return _hex(1)
        # Generic OK for any auxiliary method (eth_getBlockByNumber, etc.).
        return "0x0"

    def _eth_call_result(self, req: dict[str, Any]) -> str:
        # alloy/web3 send `eth_call` params as `[{to, data/input, ...}]`.
        # alloy's TransactionRequest serializes the calldata under `input`
        # (the newer field name); web3/legacy uses `data`. Handle both.
        params = req.get("params", [])
        filt = params[0] if params and isinstance(params[0], dict) else {}
        calldata = filt.get("data") or filt.get("input") or "0x"
        selector = calldata[:10].lower() if isinstance(calldata, str) else ""
        return self.registry.eth_call_responses.get(selector, "0x")

    def _filtered_logs(self, req: dict[str, Any]) -> list[dict[str, Any]]:
        params = req.get("params", [])
        filt = params[0] if params and isinstance(params[0], dict) else {}
        # alloy sends `address` as a list (or a single string) + `topics` as
        # `[[topic0_a, topic0_b, ...]]` (the topic0 OR-group).
        raw_addr = filt.get("address")
        addresses: set[str] = set()
        if isinstance(raw_addr, str):
            addresses.add(raw_addr.lower())
        elif isinstance(raw_addr, list):
            addresses.update(a.lower() for a in raw_addr if isinstance(a, str))
        topic0_group: set[str] = set()
        topics = filt.get("topics") or []
        if topics:
            first = topics[0]
            if isinstance(first, list):
                # An OR-group: topic0 in {t0_a, t0_b, ...}.
                topic0_group = {t.lower() for t in first if isinstance(t, str)}
            elif isinstance(first, str):
                # A single topic0 value (the fetcher flattens a 1-element
                # OR-group to a bare hash). Treat it as a 1-element group.
                topic0_group = {first.lower()}
        # Match canned logs by address (+ topic0 if a group was requested).
        out: list[dict[str, Any]] = []
        for log in self.registry.logs:
            if addresses and log["address"].lower() not in addresses:
                continue
            if topic0_group and log["topics"][0].lower() not in topic0_group:
                continue
            out.append(log)
        return out

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002
        return


@contextmanager
def mock_rpc_server(
    *,
    logs: list[dict[str, Any]],
    block_number: int,
    eth_call_responses: dict[str, str] | None = None,
) -> Generator[str, None, None]:
    """Start a local mock JSON-RPC server; yield its URL.

    `eth_call_responses` maps a calldata selector (e.g.
    `GET_CONFIGURATION_SELECTOR`) to the canned 32-byte return value — the
    server dispatches `eth_call`s by selector.
    """
    _MockRpcHandler.registry = MockRpcRegistry(
        block_number=block_number,
        logs=logs,
        eth_call_responses=eth_call_responses or {},
    )
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockRpcHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@contextmanager
def seeded_db(
    tmp_path: Path,
    *,
    name: str,
    market_id: int = 1,
    chain_id: int = 1,
    last_update_block: int = BOOTSTRAP_BLOCK,
    with_price_oracle: bool = True,
) -> Generator[tuple[Path, Session], None, None]:
    """Create a temp SQLite DB with the Rust schema + a seeded Aave market.

    Yields the DB path + an open SQLAlchemy ``Session`` (the ORM operates on
    the Rust-created schema; the two are interoperable per ADR-010). The market
    + the POOL/POOL_CONFIGURATOR/POOL_ADDRESS_PROVIDER/PRICE_ORACLE contracts +
    a minimal GHO asset (token + aave_gho_tokens row) are seeded — the minimum
    both ``run_aave_update`` (the fetch-spec lookup) + ``update_aave_market``
    (the ``assert gho_asset is not None`` requirement) need.

    Disposes the engine (closing all SQLite connections) on exit — the caller
    must finish using the session before the ``with`` block ends.
    """
    db_path = tmp_path / f"{name}.db"
    # Create the full Rust-owned schema (the source of truth) on an empty file.
    db_upgrade_database(str(db_path))
    engine = create_engine(f"sqlite:///{db_path}")
    session = Session(engine)
    try:
        session.execute(
            text(
                "INSERT INTO aave_v3_markets (id, chain_id, name, active, "
                "last_update_block) VALUES (:id, :chain, :name, 1, :block)"
            ),
            {
                "id": market_id,
                "chain": chain_id,
                "name": "aave",
                "block": last_update_block,
            },
        )
        # The contract revisions (in production set via the ProxyCreated
        # bootstrap's POOL_REVISION()/CONFIGURATOR_REVISION() RPC; in the
        # offline harness the substrate lookup requires them non-NULL — flag #2:
        # ProxyCreated EXCLUDED, contracts seeded with revisions).
        _seed_contract(session, market_id, "POOL", POOL_ADDRESS, revision=1)
        _seed_contract(
            session, market_id, "POOL_CONFIGURATOR", POOL_CONFIGURATOR_ADDRESS, revision=1
        )
        _seed_contract(session, market_id, "POOL_ADDRESS_PROVIDER", POOL_ADDRESS_PROVIDER_ADDRESS)
        if with_price_oracle:
            _seed_contract(session, market_id, "PRICE_ORACLE", PRICE_ORACLE_ADDRESS)
        # The GHO token erc20 row + the aave_gho_tokens row (the Python oracle
        # asserts gho_asset is not None; the Rust treats it as optional).
        session.execute(
            text("INSERT INTO erc20_tokens (id, chain, address) VALUES (1, :chain, :addr)"),
            {"chain": chain_id, "addr": GHO_TOKEN_ADDRESS},
        )
        session.execute(
            text(
                "INSERT INTO aave_gho_tokens (id, token_id, v_token_id, "
                "v_gho_discount_rate_strategy, v_gho_discount_token) "
                "VALUES (1, 1, NULL, NULL, NULL)"
            ),
        )
        session.commit()
        yield db_path, session
    finally:
        session.close()
        engine.dispose()


def _seed_contract(
    session: Session, market_id: int, name: str, address: str, *, revision: int | None = None
) -> None:
    """Insert an aave_v3_contracts row (read by the Rust fetch spec + get_contract)."""
    session.execute(
        text(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
            "VALUES (:market, :name, :addr, :rev)"
        ),
        {"market": market_id, "name": name, "addr": address, "rev": revision},
    )


def dump_user_rows(session: Session) -> list[dict[str, Any]]:
    """Dump the ``aave_v3_users`` rows as comparable dicts (column-by-column)."""
    rows = session.execute(
        text(
            "SELECT id, market_id, address, e_mode, gho_discount, stk_aave_balance, "
            "isolation_mode_collateral_asset_id, isolation_mode_debt "
            "FROM aave_v3_users ORDER BY address"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def seed_asset_and_user(session: Session, *, market_id: int = 1, chain_id: int = 1) -> None:
    """Seed an AaveV3Asset (with erc20 parents) + a pre-seeded user.

    The asset's underlying_token address is ``UNDERLYING_ADDRESS`` (the
    `ReserveUsedAsCollateral`/`ReserveDataUpdated` events key on it). The user's
    address is ``USER_ADDRESS`` (matches the fixture events' `user`). Both rows
    are seeded with the SAME defaults the Rust `get_or_create_*` would write — so
    the Rust path's create-or-find is a no-op + the Python path's
    `assert ... is not None` passes.
    """
    from degenbot.checksum_cache import get_checksum_address

    uw = get_checksum_address(UNDERLYING_ADDRESS)
    aw = get_checksum_address(A_TOKEN_ADDRESS)
    vw = get_checksum_address(V_TOKEN_ADDRESS)
    # erc20 parents: id 2 = underlying, 3 = a_token, 4 = v_token
    # (id 1 is the GHO token seeded by `create_seeded_db`).
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (2, :c, :u)"),
        {"c": chain_id, "u": uw},
    )
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (3, :c, :a)"),
        {"c": chain_id, "a": aw},
    )
    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (4, :c, :v)"),
        {"c": chain_id, "v": vw},
    )
    session.execute(
        text(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, "
            "a_token_id, a_token_revision, v_token_id, v_token_revision, "
            "e_mode_category_id, price_source, last_update_block, "
            "liquidity_index, liquidity_rate, borrow_index, borrow_rate) "
            "VALUES (1, :market, 2, 3, 1, 4, 1, NULL, NULL, NULL, 0, 0, 0, 0)"
        ),
        {"market": market_id},
    )
    # The pre-seeded user (checksummed — both paths store/lookup the EIP-55 form).
    session.execute(
        text(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, "
            "gho_discount, stk_aave_balance, isolation_mode_collateral_asset_id, "
            "isolation_mode_debt) "
            "VALUES (1, :market, :addr, 0, 0, NULL, NULL, '0')"
        ),
        {"market": market_id, "addr": get_checksum_address(USER_ADDRESS)},
    )
    session.flush()


def dump_collateral_config_rows(session: Session) -> list[dict[str, Any]]:
    """Dump ``aave_v3_user_collateral_configs`` rows as comparable dicts."""
    rows = session.execute(
        text(
            "SELECT id, user_id, asset_id, enabled "
            "FROM aave_v3_user_collateral_configs ORDER BY user_id, asset_id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def dump_asset_rows(session: Session) -> list[dict[str, Any]]:
    """Dump the rate/index/block columns of ``aave_v3_assets`` as comparable dicts."""
    rows = session.execute(
        text(
            "SELECT id, market_id, price_source, liquidity_index, liquidity_rate, "
            "borrow_index, borrow_rate, last_update_block "
            "FROM aave_v3_assets ORDER BY id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def dump_emode_category_rows(session: Session) -> list[dict[str, Any]]:
    """Dump ``aave_v3_emode_categories`` rows as comparable dicts."""
    rows = session.execute(
        text(
            "SELECT id, market_id, category_id, label, ltv, "
            "liquidation_threshold, liquidation_bonus, price_source "
            "FROM aave_v3_emode_categories ORDER BY category_id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def dump_contract_rows(session: Session) -> list[dict[str, Any]]:
    """Dump ``aave_v3_contracts`` rows as comparable dicts (name-ordered)."""
    rows = session.execute(
        text(
            "SELECT id, market_id, name, address, revision FROM aave_v3_contracts ORDER BY name, id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def dump_gho_token_rows(session: Session) -> list[dict[str, Any]]:
    """Dump ``aave_gho_tokens`` rows as comparable dicts."""
    rows = session.execute(
        text(
            "SELECT id, token_id, v_token_id, v_gho_discount_rate_strategy, "
            "v_gho_discount_token FROM aave_gho_tokens ORDER BY id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


def seed_gho_vtoken(session: Session, *, chain_id: int = 1) -> None:
    """Seed the chain-unique GHO variable-debt token.

    Adds an ``erc20_tokens`` row for the GHO vToken (id 5) + sets
    ``aave_gho_tokens.v_token_id = 5`` so ``gho_asset.v_token`` resolves
    (the Python ``_process_discount_*_updated_event`` emitter check + the Rust
    ``AaveGhoAsset.v_token_address`` both need this non-NULL).
    """
    from degenbot.checksum_cache import get_checksum_address

    session.execute(
        text("INSERT INTO erc20_tokens (id, chain, address) VALUES (5, :c, :a)"),
        {"c": chain_id, "a": get_checksum_address(GHO_VTOKEN_ADDRESS)},
    )
    session.execute(
        text("UPDATE aave_gho_tokens SET v_token_id = 5 WHERE id = 1"),
    )
    session.flush()


def dump_asset_config_rows(session: Session) -> list[dict[str, Any]]:
    """Dump ``aave_v3_asset_configs`` rows as comparable dicts."""
    rows = session.execute(
        text(
            "SELECT id, asset_id, ltv, liquidation_threshold, liquidation_bonus, "
            "e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, "
            "flash_loan_enabled, isolation_mode, borrowable_in_isolation, "
            "debt_ceiling "
            "FROM aave_v3_asset_configs ORDER BY asset_id"
        )
    ).all()
    return [dict(row._mapping) for row in rows]


# Corpus for the ReserveDataUpdated parity. Each entry is a tuple of
# (label, liquidity_rate, stable_borrow_rate, variable_borrow_rate,
# liquidity_index, variable_borrow_index). The stable_borrow_rate is
# decoded-then-discarded (deprecated on Aave V3). The Ray-scale set exercises
# the U256 path (the values exceed 2**64 -> the Rust stores `U256.to_string()`
# + the Python stores the big int -> both must agree as decimal strings).
_RAY = 10**27


def create_resered_data_values() -> list[tuple[str, int, int, int, int, int]]:
    """Return the parametrized value sets for the ReserveDataUpdated parity."""
    return [
        (
            "small",
            111,
            222,
            333,
            444,
            555,
        ),
        (
            "zero",
            0,
            0,
            0,
            0,
            0,
        ),
        (
            "ray-scale",
            100_000_000_000_000_000_000_000_000,  # 1e26
            0,
            200_000_000_000_000_000_000_000_000,  # 2e26
            _RAY,  # 1e27 (exceeds 2**64)
            _RAY + 100_000_000_000_000_000_000_000_000,  # 1.1e27
        ),
    ]
