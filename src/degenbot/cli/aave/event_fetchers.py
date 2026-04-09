"""Event fetching functions for Aave V3 blockchain interactions."""

from eth_typing import ChecksumAddress
from web3.types import LogReceipt

from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3OracleEvent,
    AaveV3PoolConfigEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    AaveV3StkAaveEvent,
    ERC20Event,
)
from degenbot.functions import fetch_logs_retrying
from degenbot.provider.interface import ProviderAdapter


def fetch_pool_events(
    provider: ProviderAdapter,
    pool_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool contract events for assertions and config updates.
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[pool_address],
        topic_signature=[
            [
                AaveV3PoolEvent.BORROW.value,
                AaveV3PoolEvent.DEFICIT_CREATED.value,
                AaveV3PoolEvent.LIQUIDATION_CALL.value,
                AaveV3PoolEvent.MINTED_TO_TREASURY.value,
                AaveV3PoolEvent.REPAY.value,
                AaveV3PoolEvent.RESERVE_DATA_UPDATED.value,
                AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_DISABLED.value,
                AaveV3PoolEvent.RESERVE_USED_AS_COLLATERAL_ENABLED.value,
                AaveV3PoolEvent.SUPPLY.value,
                AaveV3PoolEvent.USER_E_MODE_SET.value,
                AaveV3PoolEvent.WITHDRAW.value,
            ]
        ],
    )


def fetch_reserve_initialization_events(
    provider: ProviderAdapter,
    configurator_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool Configurator events for reserve initialization and configuration changes.
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[configurator_address],
        topic_signature=[
            [
                AaveV3PoolConfigEvent.ASSET_COLLATERAL_IN_EMODE_CHANGED.value,
                AaveV3PoolConfigEvent.COLLATERAL_CONFIGURATION_CHANGED.value,
                AaveV3PoolConfigEvent.EMODE_ASSET_CATEGORY_CHANGED.value,
                AaveV3PoolConfigEvent.EMODE_CATEGORY_ADDED.value,
                AaveV3PoolConfigEvent.RESERVE_INITIALIZED.value,
            ]
        ],
    )


def fetch_scaled_token_events(
    provider: ProviderAdapter,
    token_addresses: list[ChecksumAddress],
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch events from all scaled tokens (aTokens, vTokens).
    """

    if not token_addresses:
        return []

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=token_addresses,
        topic_signature=[
            [
                AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value,
                AaveV3PoolConfigEvent.UPGRADED.value,
                AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value,
                AaveV3ScaledTokenEvent.BURN.value,
                AaveV3ScaledTokenEvent.MINT.value,
                # Include ERC20 Transfer events for proper paired transfer matching
                ERC20Event.TRANSFER.value,
            ]
        ],
    )


def fetch_stk_aave_events(
    provider: ProviderAdapter,
    discount_token: ChecksumAddress | None,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch stkAAVE events including STAKED and REDEEM for classification.
    """

    if not discount_token:
        return []
    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[discount_token],
        topic_signature=[
            [
                AaveV3StkAaveEvent.REDEEM.value,
                AaveV3StkAaveEvent.STAKED.value,
                ERC20Event.TRANSFER.value,
            ]
        ],
    )


def fetch_address_provider_events(
    provider: ProviderAdapter,
    provider_address: ChecksumAddress,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch Pool Address Provider events for contract updates.
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[provider_address],
        topic_signature=[
            [
                AaveV3PoolConfigEvent.ADDRESS_SET.value,
                AaveV3PoolConfigEvent.POOL_CONFIGURATOR_UPDATED.value,
                AaveV3PoolConfigEvent.POOL_DATA_PROVIDER_UPDATED.value,
                AaveV3PoolConfigEvent.POOL_UPDATED.value,
                AaveV3PoolConfigEvent.PRICE_ORACLE_UPDATED.value,
                AaveV3PoolConfigEvent.PROXY_CREATED.value,
            ]
        ],
    )


def fetch_discount_config_events(
    provider: ProviderAdapter,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch discount-related events from any contract (not address-specific).
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        topic_signature=[
            [
                AaveV3GhoDebtTokenEvent.DISCOUNT_RATE_STRATEGY_UPDATED.value,
                AaveV3GhoDebtTokenEvent.DISCOUNT_TOKEN_UPDATED.value,
            ]
        ],
    )


def fetch_oracle_events(
    provider: ProviderAdapter,
    oracle_address: ChecksumAddress | None,
    start_block: int,
    end_block: int,
) -> list[LogReceipt]:
    """
    Fetch AaveOracle events for oracle configuration changes.

    If oracle_address is None, fetches events from all contracts (discovery mode).
    """

    return fetch_logs_retrying(
        w3=provider,
        start_block=start_block,
        end_block=end_block,
        address=[oracle_address] if oracle_address is not None else None,
        topic_signature=[
            [
                AaveV3OracleEvent.ASSET_SOURCE_UPDATED.value,
            ]
        ],
    )
