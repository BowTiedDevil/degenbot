from json import JSONDecodeError
from typing import TYPE_CHECKING, cast

import tenacity
from ujson import loads as ujson_loads
from web3 import AsyncBaseProvider, AsyncWeb3, JSONBaseProvider
from web3.types import RPCResponse

from degenbot.exceptions import DegenbotValueError
from degenbot.provider import AsyncProviderAdapter
from degenbot.types.aliases import ChainId


def _fast_decode_rpc_response(raw_response: bytes) -> RPCResponse:
    """
    Decode the JSON-RPC response using ujson.
    """

    try:
        return cast("RPCResponse", ujson_loads(raw_response))
    except ValueError:
        # Re-raise as a dummy JSONDecodeError so web3py's exception handling works as intended.
        msg = "JSON failure"
        raise JSONDecodeError(msg, "[]", 0) from None


class AsyncConnectionManager:
    def __init__(self) -> None:
        self.connections: dict[ChainId, AsyncProviderAdapter] = {}
        self._default_chain_id: ChainId | None = None

    def _reset(self) -> None:
        self.connections.clear()
        self._default_chain_id = None

    def get_provider(self, chain_id: ChainId) -> AsyncProviderAdapter:
        """Get an AsyncProviderAdapter for the specified chain ID.

        Args:
            chain_id: The chain ID to get the provider for

        Returns:
            AsyncProviderAdapter for the chain

        Raises:
            DegenbotValueError: If no provider is registered for the chain
        """
        try:
            return self.connections[chain_id]
        except KeyError:
            raise DegenbotValueError(
                message="Chain ID does not have a registered provider."
            ) from None

    def get_web3(self, chain_id: ChainId) -> AsyncWeb3[AsyncBaseProvider]:
        """Get the underlying AsyncWeb3 instance for the specified chain ID.

        Args:
            chain_id: The chain ID to get the AsyncWeb3 instance for

        Returns:
            AsyncWeb3 instance for the chain

        Raises:
            DegenbotValueError: If no provider is registered for the chain
            DegenbotValueError: If the provider is not a Web3 provider
        """
        provider = self.get_provider(chain_id)
        if provider.provider_type != "web3":
            raise DegenbotValueError(
                message="Provider is not a Web3 provider."
            ) from None
        return provider.underlying

    async def register_provider(
        self,
        provider: AsyncProviderAdapter,
        *,
        optimize: bool = True,
    ) -> None:
        """Register an AsyncProviderAdapter.

        Args:
            provider: The AsyncProviderAdapter to register
            optimize: Whether to optimize the underlying provider (Web3 only)

        Raises:
            DegenbotValueError: If the provider is not connected
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

        # Get the underlying AsyncWeb3 instance for optimization if needed
        if optimize and provider.provider_type == "web3":
            w3 = provider.underlying
            # Remove all middleware and monkey-patch the JSON decoding for RPC responses
            w3.middleware_onion.clear()
            if TYPE_CHECKING:
                assert isinstance(w3.provider, JSONBaseProvider)
            w3.provider.decode_rpc_response = _fast_decode_rpc_response

        self.connections[await provider.get_chain_id()] = provider

    async def register_web3(
        self,
        w3: AsyncWeb3[AsyncBaseProvider],
        *,
        optimize: bool = True,
    ) -> None:
        """Register an AsyncWeb3 instance (legacy method, wraps in AsyncProviderAdapter).

        Args:
            w3: The AsyncWeb3 instance to register
            optimize: Whether to optimize the AsyncWeb3 instance
        """
        provider = AsyncProviderAdapter.from_web3(w3)
        await self.register_provider(provider, optimize=optimize)

    def set_default_chain(self, chain_id: ChainId) -> None:
        self._default_chain_id = chain_id

    @property
    def default_chain_id(self) -> ChainId:
        if self._default_chain_id is None:
            raise DegenbotValueError(message="A default chain ID has not been provided.")
        return self._default_chain_id
