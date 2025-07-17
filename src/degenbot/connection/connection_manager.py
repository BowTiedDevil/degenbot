import tenacity
import web3

from degenbot.exceptions import DegenbotValueError
from degenbot.types.aliases import ChainId


class ConnectionManager:
    def __init__(self) -> None:
        self.connections: dict[ChainId, web3.Web3] = {}
        self._default_chain_id: ChainId | None = None

    def get_web3(self, chain_id: ChainId) -> web3.Web3:
        try:
            return self.connections[chain_id]
        except KeyError:
            raise DegenbotValueError(
                message="Chain ID does not have a registered Web3 instance."
            ) from None

    def register_web3(
        self,
        w3: web3.Web3,
        *,
        optimize_middleware: bool = True,
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

        if optimize_middleware:
            w3.middleware_onion.clear()
        self.connections[w3.eth.chain_id] = w3

    def set_default_chain(self, chain_id: ChainId) -> None:
        self._default_chain_id = chain_id

    @property
    def default_chain_id(self) -> ChainId:
        if self._default_chain_id is None:
            raise DegenbotValueError(message="A default Web3 instance has not been registered.")
        return self._default_chain_id
