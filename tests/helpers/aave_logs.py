"""Canned Aave V3 `eth_getLogs` fixture entries + the constants they key on.

The log builders are the fixture vocabulary of the Rust-writer parity suite:
each one encodes one Aave V3 event at the topic/data shape `eth_getLogs`
returns, keyed on the seeded market the harness serves. They live here rather
than beside the mock-RPC harness so a test that needs only fixture events does
not pull in the HTTP server and the SQLite seeding.

The addresses are the seeded market the mock-RPC harness serves, so the
builders and the server must agree on them; the harness imports them from here.
"""

from __future__ import annotations

from typing import Any

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
DEBT_TOKEN_REVISION_SELECTOR = "0xb9a7b622"  # ruff: ignore[hardcoded-password-string]
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
# tail; `degenbot.abi.encode` builds it cleanly).
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
    from degenbot.abi import encode as abi_encode

    data = (
        "0x"
        + abi_encode(
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
_DISCOUNT_TOKEN_UPDATED_TOPIC = "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529"  # ruff: ignore[hardcoded-password-string]


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
# writes `aave_v3_users.gho_discount = newDiscountPercent` (rule #5
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


# ── GHO ops-parser fixtures (the multi-tx-within-chunk flag #1 proof) ─
# The Pool `Borrow` / `Repay` events + the variableDebtToken `Mint` / `Burn`
# (scaled-token) events + the plain ERC20 `Transfer` companion the ops parser
# asserts (the Borrow requires a Transfer from ZERO_ADDRESS to the borrower on
# the borrowed token, matching the debt-mint amount). Each builder takes a
# `tx_hash` so a fixture can place several tx groups in ONE chunk.
#
# Topic0 hashes (keccak256 of the canonical Aave V3 / ERC20 event signatures).
SUPPLY_TOPIC = "0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61"
BORROW_TOPIC = "0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0"
REPAY_TOPIC = "0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051"
WITHDRAW_TOPIC = "0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7"
MINT_TOPIC = "0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196"
BURN_TOPIC = "0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90"
ERC20_TRANSFER_TOPIC = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
BALANCE_TRANSFER_TOPIC = "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666"


def make_withdraw_log(
    *,
    reserve: str,
    user: str,
    to: str,
    amount: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a Pool `Withdraw` event.

    Real Aave V3 Withdraw signature (verified on mainnet, block 16496928):
        Withdraw(address indexed reserve, address indexed user,
                 address indexed to, uint256 amount)
    3 indexed params → 4 topics: [sig, reserve(1), user(2), to(3)]. Non-indexed
    data: abi.encode(amount) = 1 word. The CollateralBurn debits `user`'s
    position; `to` receives the underlying.
    """
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[
            WITHDRAW_TOPIC,
            _pad_address(reserve),
            _pad_address(user),  # topic[2] = user (indexed)
            _pad_address(to),  # topic[3] = to (indexed)
        ],
        data=_u256(amount),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_balance_transfer_log(
    *,
    token_address: str,
    from_address: str,
    to_address: str,
    value: int,
    index: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for an aToken `BalanceTransfer` event.

    Aave V3 aToken `BalanceTransfer(address indexed from, address indexed to,
    uint256 value, uint256 index)` — emitted alongside the plain ERC20
    `Transfer` when aTokens move between users (the scaled-balance bookkeeping).
    2 indexed params → 3 topics: [sig, from(1), to(2)]. Non-indexed data:
    abi.encode(value, index) = 2 words. The standalone BalanceTransfer op
    credits `to` + debits `from` (collateral moves between users — NOT a
    Pool Supply/Withdraw).
    """
    words = [_u256(value)[2:], _u256(index)[2:]]
    return _make_log(
        address=token_address.lower(),
        topics=[
            BALANCE_TRANSFER_TOPIC,
            _pad_address(from_address),
            _pad_address(to_address),
        ],
        data="0x" + "".join(words),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_supply_log(
    *,
    reserve: str,
    on_behalf_of: str,
    amount: int,
    user: str | None = None,
    referral_code: int = 0,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a Pool `Supply` event.

    Real Aave V3 Supply signature (verified on mainnet, block 16496817):
        Supply(address indexed reserve, address user,
               address indexed onBehalfOf, uint256 amount,
               uint16 indexed referralCode)
    3 indexed params → 4 topics: [sig, reserve(topic1), onBehalfOf(topic2),
    referralCode(topic3)]. Non-indexed data: abi.encode(user, amount) = 2 words.

    `user` defaults to `on_behalf_of` (the common self-supply case) but should
    be set DISTINCT from `on_behalf_of` + `referral_code ≠ 0` in tests that
    exercise the topic-indexing decode path.
    """
    user = user or on_behalf_of
    words = [_pad_address(user)[2:], _u256(amount)[2:]]
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[
            SUPPLY_TOPIC,
            _pad_address(reserve),
            _pad_address(on_behalf_of),  # topic[2] = onBehalfOf (indexed)
            _u256(referral_code),  # topic[3] = referralCode (indexed uint16)
        ],
        data="0x" + "".join(words),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_borrow_log(
    *,
    reserve: str,
    on_behalf_of: str,
    amount: int,
    user: str | None = None,
    interest_rate_mode: int = 2,
    borrow_rate: int = 0,
    referral_code: int = 0,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a Pool `Borrow` event.

    Real Aave V3 Borrow signature (verified on mainnet: block 16497464
    referralCode=64, block 16498172 referralCode=6671):
        Borrow(address indexed reserve, address user,
               address indexed onBehalfOf, uint256 amount,
               DataTypes.InterestRateMode interestRateMode, uint256 borrowRate,
               uint16 indexed referralCode)
    3 indexed params → 4 topics: [sig, reserve(1), onBehalfOf(2), referralCode(3)].
    Non-indexed data: abi.encode(user, amount, mode, borrowRate) = 4 words.

    `user` defaults to `on_behalf_of` (the common self-borrow case) but should
    be set DISTINCT from `on_behalf_of` in tests that exercise the
    topic-indexing decode path (the bug that masked byte-IDENTITY when
    `user == on_behalf_of` made topic[2]==topic[3]) .
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
            _pad_address(on_behalf_of),  # topic[2] = onBehalfOf (indexed)
            _u256(referral_code),  # topic[3] = referralCode (indexed uint16)
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


# Pool `LiquidationCall(address indexed collateralAsset, address indexed debtAsset,
# address indexed user, uint256 debtToCover, uint256 liquidatedCollateralAmount,
# address liquidator, bool receiveAToken)` — topic0 hash matches the Rust
# decoder's LIQUIDATION_CALL_TOPIC byte-exact. The 3 indexed params map to
# topics[1..3] (collateralAsset, debtAsset, user); non-indexed data is
# abi.encode(debtToCover, liquidatedCollateralAmount, liquidator, receiveAToken)
# = 4 words = 128 bytes.
LIQUIDATION_CALL_TOPIC = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"


def make_liquidation_call_log(
    *,
    collateral_asset: str,
    debt_asset: str,
    user: str,
    debt_to_cover: int,
    liquidated_collateral_amount: int,
    liquidator: str,
    receive_a_token: bool = True,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a Pool `LiquidationCall` event.

    Real Aave V3 LiquidationCall signature (verified on mainnet):
        event LiquidationCall(
            address indexed collateralAsset, address indexed debtAsset,
            address indexed user, uint256 debtToCover,
            uint256 liquidatedCollateralAmount, address liquidator,
            bool receiveAToken)
    3 indexed params → 4 topics. Non-indexed data: abi.encode(debtToCover,
    liquidatedCollateralAmount, liquidator, receiveAToken) = 4 words.
    The ops parser reads `debtToCover` (word 0) + `liquidatedCollateralAmount`
    (word 1) — `extract_pool_amount_word0` / `word1` in the dispatch path.
    """
    words = [
        _u256(debt_to_cover)[2:],
        _u256(liquidated_collateral_amount)[2:],
        _pad_address(liquidator)[2:],
        _u256(1 if receive_a_token else 0)[2:],
    ]
    return _make_log(
        address=POOL_ADDRESS.lower(),
        topics=[
            LIQUIDATION_CALL_TOPIC,
            _pad_address(collateral_asset),
            _pad_address(debt_asset),
            _pad_address(user),
        ],
        data="0x" + "".join(words),
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


# stkAAVE `Staked(address indexed from, address indexed onBehalfOf, uint256 amount)`
# + `Redeem(address indexed from, address indexed to, uint256 amount)` — emitted by
# the stkAAVE token (the GHO asset's `v_gho_discount_token`). The Rust config
# path processes the semantic Staked/Redeem events (the coinciding ERC20
# Transfer is fetched but decodes to None → skipped); the Python
# `transaction_processor` processes the coinciding ERC20 Transfer on the discount
# token. For a stake, both increment `onBehalfOf`; for a redeem, both decrement
# `from` (the redeemer). Topic hashes match the Rust decoder's STAKED_TOPIC /
# REDEEM_TOPIC byte-exact.
STK_AAVE_ADDRESS = "0x" + "ee" * 20
_STAKED_TOPIC = "0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc"
_REDEEM_TOPIC = "0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc"


def make_staked_log(
    *,
    staker: str,
    on_behalf_of: str,
    amount: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a stkAAVE `Staked` event."""
    return _make_log(
        address=STK_AAVE_ADDRESS.lower(),
        topics=[
            _STAKED_TOPIC,
            _pad_address(staker),
            _pad_address(on_behalf_of),
        ],
        data=_u256(amount),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )


def make_redeem_log(
    *,
    redeemer: str,
    to: str,
    amount: int,
    block: int = FIXTURE_BLOCK,
    log_index: int = 0,
    tx_hash: str | None = None,
) -> dict[str, Any]:
    """Build a canned `eth_getLogs` entry for a stkAAVE `Redeem` event."""
    return _make_log(
        address=STK_AAVE_ADDRESS.lower(),
        topics=[
            _REDEEM_TOPIC,
            _pad_address(redeemer),
            _pad_address(to),
        ],
        data=_u256(amount),
        block=block,
        log_index=log_index,
        tx_hash=tx_hash,
    )
