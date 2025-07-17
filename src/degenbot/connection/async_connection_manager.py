import tenacity
import web3

from degenbot.exceptions import DegenbotValueError
from degenbot.types.aliases import ChainId


class AsyncConnectionManager:
    def __init__(self) -> None:
        self.connections: dict[ChainId, web3.AsyncWeb3] = {}
        self._default_chain_id: ChainId | None = None

    def get_web3(self, chain_id: ChainId) -> "web3.AsyncWeb3":
        try:
            return self.connections[chain_id]
        except KeyError:
            raise DegenbotValueError(
                message="Chain ID does not have a registered Web3 instance."
            ) from None

    async def register_web3(
        self,
        w3: web3.AsyncWeb3,
        *,
        optimize_middleware: bool = True,
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

        if optimize_middleware:
            w3.middleware_onion.clear()
        self.connections[await w3.eth.chain_id] = w3

    def set_default_chain(self, chain_id: ChainId) -> None:
        self._default_chain_id = chain_id

    @property
    def default_chain_id(self) -> ChainId:
        if self._default_chain_id is None:
            raise DegenbotValueError(message="A default chain ID has not been provided.")
        return self._default_chain_id
