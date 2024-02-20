from typing import TYPE_CHECKING, Any, Dict, Iterable, List

import aiohttp
import eth_account.datastructures
import eth_account.messages
import ujson
import web3
from hexbytes import HexBytes

from ..exceptions import ExternalServiceError
from ..logging import logger

_SUPPORTED_ENDPOINTS = [
    "eth_callBundle",
    "eth_sendBundle",
]


class BlockBuilder:
    """
    A block builder providing an HTTP endpoint and offering one or more bundle methods as defined by the Flashbots RPC endpoint specification.
    """

    def __init__(
        self,
        url: str,
        endpoints: Iterable[str],
        authentication_header_label: str | None = None,
    ):
        if not url.startswith(("http://", "https://")):
            raise ValueError("Invalid URL")
        self.url = url

        for endpoint in endpoints:
            if endpoint not in _SUPPORTED_ENDPOINTS:
                logger.warning(f"Endpoint {endpoint} is not supported and has been ignored.")
        self.endpoints = tuple(
            [endpoint for endpoint in endpoints if endpoint in _SUPPORTED_ENDPOINTS]
        )

        self.authentication_header_label = authentication_header_label

    @staticmethod
    async def send(
        url: str,
        session: aiohttp.ClientSession,
        headers: Dict[str, Any],
        data: Dict[str, Any],
        close_session_after: bool,
    ) -> Any:
        try:
            async with session.post(
                url=url,
                data=data,
                headers=headers,
            ) as resp:
                relay_response = await resp.json(
                    # Some builders omit or return an invalid MIME type, so use None to bypass the
                    # check in the `json` method
                    content_type=None,
                )
        except aiohttp.ClientError as exc:
            raise ExternalServiceError(f"HTTP Error: {exc}") from None
        else:
            if "error" in relay_response:
                return relay_response["error"]
            return relay_response["result"]
        finally:
            if close_session_after is False:
                await session.close()

    async def send_eth_bundle(
        self,
        bundle: Iterable[HexBytes],
        block_number: int,
        min_timestamp: int | None = None,
        max_timestamp: int | None = None,
        reverting_hashes: List[str] | None = None,
        replacement_uuid: str | None = None,
        signer_address: str | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the eth_sendBundle endpoint
        """

        if "eth_sendBundle" not in self.endpoints:
            raise ValueError("eth_sendBundle was not included in the list of supported endpoints.")

        http_session_provided = True if http_session is not None else False
        if http_session is None:
            http_session_provided = False
            http_session = aiohttp.ClientSession(raise_for_status=True)

        formatted_bundle: List[str] = [tx.hex() for tx in bundle]

        if self.authentication_header_label is not None and any(
            [signer_address is None, signer_key is None]
        ):
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        bundle_params: Dict[str, Any] = {
            "txs": (
                # Array[String], A list of signed transactions to execute in an atomic bundle
                formatted_bundle
            ),
            "blockNumber": (
                # String, a hex encoded block number for which this bundle is valid on
                hex(block_number)
            ),
        }

        if min_timestamp is not None:
            bundle_params[
                # (Optional) Number, the minimum timestamp for which this bundle is valid, in seconds since the unix epoch
                "minTimestamp"
            ] = min_timestamp

        if max_timestamp is not None:
            bundle_params[
                # (Optional) Number, the maximum timestamp for which this bundle is valid, in seconds since the unix epoch
                "maxTimestamp"
            ] = max_timestamp

        if reverting_hashes is not None:
            bundle_params[
                # (Optional) Array[String], A list of tx hashes that are allowed to revert
                "revertingTxHashes"
            ] = reverting_hashes

        if replacement_uuid is not None:
            bundle_params[
                # (Optional) String, UUID that can be used to cancel/replace this bundle
                "replacementUuid"
            ] = replacement_uuid

        bundle_payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_sendBundle",
                "params": [bundle_params],
            }
        )

        bundle_headers = {"Content-Type": "application/json"}

        if self.authentication_header_label:
            if TYPE_CHECKING:
                assert signer_address is not None
                assert signer_key is not None
            send_bundle_message: eth_account.datastructures.SignedMessage = (
                eth_account.Account.sign_message(
                    signable_message=eth_account.messages.encode_defunct(
                        text=web3.Web3.keccak(text=bundle_payload).hex()
                    ),
                    private_key=signer_key,
                )
            )
            bundle_signature = signer_address.lower() + ":" + send_bundle_message.signature.hex()
            bundle_headers[self.authentication_header_label] = bundle_signature

        result = await self.send(
            url=self.url,
            session=http_session,
            headers=bundle_headers,
            data=bundle_payload,
            close_session_after=False if http_session_provided is True else True,
        )
        return result

    async def call_eth_bundle(
        self,
        bundle: Iterable[HexBytes],
        block_number: int,
        state_block: int | str,
        block_timestamp: int | None = None,
        signer_address: str | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the eth_callBundle endpoint for simulation against some block state
        """

        if "eth_callBundle" not in self.endpoints:
            raise ValueError("eth_callBundle was not included in the list of supported endpoints.")

        http_session_provided = True if http_session is not None else False
        if http_session is None:
            http_session_provided = False
            http_session = aiohttp.ClientSession(raise_for_status=True)

        formatted_bundle: List[str] = [tx.hex() for tx in bundle]

        bundle_params: Dict[str, Any] = {
            "txs": (
                # Array[String], A list of signed transactions to execute in an atomic bundle
                formatted_bundle
            ),
            "blockNumber": (
                # String, a hex encoded block number for which this bundle is valid on
                hex(block_number)
            ),
        }

        if isinstance(state_block, str):
            if state_block != "latest":
                raise ValueError("state_block tag may only be an integer, or the string 'latest'")
            bundle_params["stateBlockNumber"] = state_block
        elif isinstance(state_block, int):
            bundle_params[
                # String, either a hex encoded number or a block tag for which state to base this simulation on. Can use "latest"
                "stateBlockNumber"
            ] = hex(state_block)

        if block_timestamp is not None:
            bundle_params["timestamp"] = block_timestamp

        bundle_payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_callBundle",
                "params": [bundle_params],
            }
        )

        bundle_headers = {"Content-Type": "application/json"}

        if self.authentication_header_label:
            if TYPE_CHECKING:
                assert signer_address is not None
                assert signer_key is not None
            bundle_message: eth_account.datastructures.SignedMessage = (
                eth_account.Account.sign_message(
                    signable_message=eth_account.messages.encode_defunct(
                        text=web3.Web3.keccak(text=bundle_payload).hex()
                    ),
                    private_key=signer_key,
                )
            )
            bundle_signature = signer_address.lower() + ":" + bundle_message.signature.hex()
            bundle_headers[self.authentication_header_label] = bundle_signature

        result = await self.send(
            url=self.url,
            session=http_session,
            headers=bundle_headers,
            data=bundle_payload,
            close_session_after=False if http_session_provided is True else True,
        )
        return result
