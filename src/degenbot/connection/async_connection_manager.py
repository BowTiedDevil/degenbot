from json import JSONDecodeError
from typing import TYPE_CHECKING, cast

import tenacity
from ujson import loads as ujson_loads
from web3 import AsyncBaseProvider, AsyncWeb3, JSONBaseProvider
from web3.types import RPCResponse

from degenbot.exceptions import DegenbotValueError
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
        self.connections: dict[ChainId, AsyncWeb3[AsyncBaseProvider]] = {}
        self._default_chain_id: ChainId | None = None

    def get_web3(self, chain_id: ChainId) -> AsyncWeb3[AsyncBaseProvider]:
        try:
            return self.connections[chain_id]
        except KeyError:
            raise DegenbotValueError(
                message="Chain ID does not have a registered Web3 instance."
            ) from None

    async def register_web3(
        self,
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

        if optimize:
            # Remove all middleware and monkey-patch the JSON decoding for RPC responses
            w3.middleware_onion.clear()
            if TYPE_CHECKING:
                assert isinstance(w3.provider, JSONBaseProvider)
            w3.provider.decode_rpc_response = _fast_decode_rpc_response

        self.connections[await w3.eth.chain_id] = w3

    def set_default_chain(self, chain_id: ChainId) -> None:
        self._default_chain_id = chain_id

    @property
    def default_chain_id(self) -> ChainId:
        if self._default_chain_id is None:
            raise DegenbotValueError(message="A default chain ID has not been provided.")
        return self._default_chain_id
