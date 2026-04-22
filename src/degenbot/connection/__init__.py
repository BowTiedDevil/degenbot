import tenacity
from web3 import AsyncBaseProvider, AsyncWeb3, Web3

from degenbot.exceptions import DegenbotValueError
from degenbot.provider import AsyncProviderAdapter, ProviderAdapter

from .async_connection_manager import AsyncConnectionManager
from .connection_manager import ConnectionManager


def get_async_provider() -> AsyncProviderAdapter:
    """Get the default async provider.

    Returns:
        AsyncProviderAdapter for the default chain

    Raises:
        DegenbotValueError: If no default provider has been registered
    """
    if async_connection_manager.default_chain_id is None:
        raise DegenbotValueError(
            message="A default provider has not been registered."
        ) from None
    return async_connection_manager.get_provider(chain_id=async_connection_manager.default_chain_id)


def get_async_web3() -> AsyncWeb3[AsyncBaseProvider]:
    """Get the default AsyncWeb3 instance (legacy method).

    Returns:
        AsyncWeb3 for the default chain

    Raises:
        DegenbotValueError: If no default provider has been registered
        DegenbotValueError: If the provider is not a Web3 provider
    """
    if async_connection_manager.default_chain_id is None:
        raise DegenbotValueError(
            message="A default provider has not been registered."
        ) from None
    return async_connection_manager.get_web3(chain_id=async_connection_manager.default_chain_id)


async def set_async_provider(
    provider: AsyncProviderAdapter,
    *,
    optimize: bool = True,
) -> None:
    """Set the default async provider.

    Args:
        provider: The AsyncProviderAdapter to set as default
        optimize: Whether to optimize the provider
    """
    async_w3_connected_check_with_retry = tenacity.AsyncRetrying(
        stop=tenacity.stop_after_delay(10),
        wait=tenacity.wait_exponential_jitter(),
        retry=tenacity.retry_if_result(lambda result: result is False),
    )
    try:
        await async_w3_connected_check_with_retry(provider.is_connected)
    except tenacity.RetryError as exc:
        raise DegenbotValueError(message="Provider is not connected.") from exc

    await async_connection_manager.register_provider(provider, optimize=optimize)
    async_connection_manager.set_default_chain(await provider.get_chain_id())


async def set_async_web3(
    w3: AsyncWeb3[AsyncBaseProvider],
    *,
    optimize: bool = True,
) -> None:
    """Set the default AsyncWeb3 instance (legacy method, wraps in AsyncProviderAdapter).

    Args:
        w3: The AsyncWeb3 instance to set as default
        optimize: Whether to optimize the Web3 instance
    """
    provider = AsyncProviderAdapter.from_web3(w3)
    await set_async_provider(provider, optimize=optimize)


def get_provider() -> ProviderAdapter:
    """Get the default provider.

    Returns:
        ProviderAdapter for the default chain

    Raises:
        DegenbotValueError: If no default provider has been registered
    """
    return connection_manager.get_provider(chain_id=connection_manager.default_chain_id)


def get_web3() -> Web3:
    """Get the default Web3 instance (legacy method).

    Returns:
        Web3 for the default chain

    Raises:
        DegenbotValueError: If no default provider has been registered
        DegenbotValueError: If the provider is not a Web3 provider
    """
    return connection_manager.get_web3(chain_id=connection_manager.default_chain_id)


def set_provider(
    provider: ProviderAdapter,
    *,
    optimize: bool = True,
) -> None:
    """Set the default provider.

    Args:
        provider: The ProviderAdapter to set as default
        optimize: Whether to optimize the provider
    """
    w3_connected_check_with_retry = tenacity.Retrying(
        stop=tenacity.stop_after_delay(10),
        wait=tenacity.wait_exponential_jitter(),
        retry=tenacity.retry_if_result(lambda result: result is False),
    )
    try:
        w3_connected_check_with_retry(fn=provider.is_connected)
    except tenacity.RetryError as exc:
        raise DegenbotValueError(message="Provider is not connected.") from exc

    connection_manager.register_provider(provider, optimize=optimize)
    connection_manager.set_default_chain(provider.chain_id)


def set_web3(
    w3: Web3,
    *,
    optimize: bool = True,
) -> None:
    """Set the default Web3 instance (legacy method, wraps in ProviderAdapter).

    Args:
        w3: The Web3 instance to set as default
        optimize: Whether to optimize the Web3 instance
    """
    provider = ProviderAdapter.from_web3(w3)
    set_provider(provider, optimize=optimize)


connection_manager = ConnectionManager()
async_connection_manager = AsyncConnectionManager()


__all__ = (
    "async_connection_manager",
    "connection_manager",
    "get_async_provider",
    "get_async_web3",
    "get_provider",
    "get_web3",
    "set_async_provider",
    "set_async_web3",
    "set_provider",
    "set_web3",
)
