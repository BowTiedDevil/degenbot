import tenacity
from web3 import AsyncBaseProvider, AsyncWeb3, Web3

from degenbot.exceptions import DegenbotValueError

from .async_connection_manager import AsyncConnectionManager
from .connection_manager import ConnectionManager


def get_async_web3() -> AsyncWeb3[AsyncBaseProvider]:
    if async_connection_manager.default_chain_id is None:
        raise DegenbotValueError(
            message="A default Web3 instance has not been registered."
        ) from None
    return async_connection_manager.get_web3(chain_id=async_connection_manager.default_chain_id)


async def set_async_web3(
    w3: AsyncWeb3[AsyncBaseProvider],
    *,
    optimize: bool = True,
) -> None:
    async_w3_connected_check_with_retry = tenacity.AsyncRetrying(
        stop=tenacity.stop_after_delay(10),
        wait=tenacity.wait_exponential_jitter(),
        retry=tenacity.retry_if_result(lambda result: result is False),
    )
    try:
        await async_w3_connected_check_with_retry(w3.is_connected)
    except tenacity.RetryError as exc:
        raise DegenbotValueError(message="Web3 instance is not connected.") from exc

    await async_connection_manager.register_web3(w3, optimize=optimize)
    async_connection_manager.set_default_chain(await w3.eth.chain_id)


def get_web3() -> Web3:
    return connection_manager.get_web3(chain_id=connection_manager.default_chain_id)


def set_web3(
    w3: Web3,
    *,
    optimize: bool = True,
) -> None:
    w3_connected_check_with_retry = tenacity.Retrying(
        stop=tenacity.stop_after_delay(10),
        wait=tenacity.wait_exponential_jitter(),
        retry=tenacity.retry_if_result(lambda result: result is False),
    )
    try:
        w3_connected_check_with_retry(fn=w3.is_connected)
    except tenacity.RetryError as exc:
        raise DegenbotValueError(message="Web3 instance is not connected.") from exc

    connection_manager.register_web3(w3, optimize=optimize)
    connection_manager.set_default_chain(w3.eth.chain_id)


connection_manager = ConnectionManager()
async_connection_manager = AsyncConnectionManager()


__all__ = (
    "async_connection_manager",
    "connection_manager",
    "get_web3",
    "get_web3_async",
    "set_web3",
    "set_web3_async",
)
