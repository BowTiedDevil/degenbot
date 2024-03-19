from typing import TYPE_CHECKING, Any, Dict, Iterable, List

import aiohttp
import eth_account.datastructures
import eth_account.messages
import eth_account.signers.local
import ujson
from eth_account.signers.local import LocalAccount
from hexbytes import HexBytes

from .config import get_web3
from .exceptions import ExternalServiceError
from .functions import eip_191_hash


class BuilderEndpoint:
    """
    An external HTTP endpoint with one or more bundle-related methods defined by the Flashbots RPC specification at https://docs.flashbots.net/flashbots-auction/advanced/rpc-endpoint
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

        self.endpoints = tuple(endpoints)
        self.authentication_header_label = authentication_header_label

    @staticmethod
    def _build_authentication_header(
        payload: str,
        header_label: str | None,
        signer_key: str | None,
    ) -> Dict[str, Any]:
        """
        Build a MIME type header with an optional EIP-191 signature of the provided payload.
        """

        if signer_key:
            signer_account: LocalAccount = eth_account.Account.from_key(signer_key)
            signer_address = signer_account.address

        bundle_headers = {"Content-Type": "application/json"}

        if all([header_label is not None, signer_key is not None]):
            if TYPE_CHECKING:
                assert header_label is not None
                assert signer_key is not None
                assert isinstance(signer_address, eth_account.Account)

            message_signature = eip_191_hash(message=payload, private_key=signer_key)
            bundle_signature = f"{signer_address}:{message_signature}"
            bundle_headers[header_label] = bundle_signature

        return bundle_headers

    @staticmethod
    async def send_payload(
        url: str,
        headers: Dict[str, Any],
        data: str,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        close_session_after_post = http_session is None

        session = (
            aiohttp.ClientSession(raise_for_status=True) if http_session is None else http_session
        )

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
        except aiohttp.ClientError as exc:  # pragma: no cover
            raise ExternalServiceError(f"HTTP Error: {exc}") from None
        else:
            return (
                relay_response["error"] if "error" in relay_response else relay_response["result"]
            )
        finally:
            if close_session_after_post:
                await session.close()

    async def call_eth_bundle(
        self,
        bundle: Iterable[HexBytes],
        block_number: int,
        state_block: int | str,
        signer_key: str,
        block_timestamp: int | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the eth_callBundle endpoint for simulation against some block state
        """

        ENDPOINT_METHOD = "eth_callBundle"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        bundle_params: Dict[str, Any] = {
            "txs": (
                # Array[String], A list of signed transactions to execute in an atomic bundle
                [tx.hex() for tx in bundle]
            ),
            "blockNumber": (
                # String, a hex encoded block number for which this bundle is valid on
                hex(block_number)
            ),
        }

        if isinstance(state_block, str):
            if state_block != "latest":  # pragma: no cover
                raise ValueError("state_block tag may only be an integer, or the string 'latest'")
            bundle_params["stateBlockNumber"] = state_block
        elif isinstance(state_block, int):
            bundle_params[
                # String, either a hex encoded number or a block tag for which state to base this simulation on. Can use "latest"
                "stateBlockNumber"
            ] = hex(state_block)

        if block_timestamp is not None:
            bundle_params["timestamp"] = block_timestamp

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [bundle_params],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            header_label=self.authentication_header_label,
            signer_key=signer_key,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result

    async def cancel_eth_bundle(
        self,
        uuid: str | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a cancellation for the specified UUID.
        """

        ENDPOINT_METHOD = "eth_cancelBundle"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [
                    {
                        "replacementUuid": uuid,
                    }
                ],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result

    async def cancel_private_transaction(
        self,
        tx_hash: bytes | str,
        signer_key: str,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a cancellation for the specified private transaction hash.

        The signer key used for the request must match the signer key for the transaction.
        """

        ENDPOINT_METHOD = "eth_cancelPrivateTransaction"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        if isinstance(tx_hash, bytes):
            tx_hash = tx_hash.hex()

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [
                    {
                        "txHash": tx_hash,
                    }
                ],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result

    async def get_user_stats(
        self,
        signer_key: str,
        block_number: int,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Get the Flashbots V2 user stats for the given searcher identity.
        """

        ENDPOINT_METHOD = "flashbots_getUserStatsV2"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        if block_number is None:
            block_number = get_web3().eth.block_number

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [
                    {
                        "blockNumber": hex(block_number),
                    }
                ],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result

    async def get_bundle_stats(
        self,
        bundle_hash: str,
        block_number: int,
        signer_key: str,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Get the Flashbots V2 bundle stats for the given searcher identity.
        """

        ENDPOINT_METHOD = "flashbots_getBundleStatsV2"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [
                    {
                        "bundleHash": bundle_hash,
                        "blockNumber": hex(block_number),
                    }
                ],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result

    async def send_eth_bundle(
        self,
        bundle: Iterable[HexBytes],
        block_number: int,
        min_timestamp: int | None = None,
        max_timestamp: int | None = None,
        reverting_hashes: List[str] | None = None,
        uuid: str | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the eth_sendBundle endpoint
        """

        ENDPOINT_METHOD = "eth_sendBundle"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        bundle_params: Dict[str, Any] = {
            "txs": (
                # Array[String], A list of signed transactions to execute in an atomic bundle
                [tx.hex() for tx in bundle]
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

        if uuid is not None:
            bundle_params[
                # (Optional) String, UUID that can be used to cancel/replace this bundle
                "replacementUuid"
            ] = uuid

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [bundle_params],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            headers=bundle_headers,
            data=payload,
            http_session=http_session,
        )
        return result

    async def send_private_transaction(
        self,
        raw_transaction: bytes | str,
        signer_key: str,
        max_block_number: int | None = None,
        preferences: Dict[str, Any] | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a private raw transaction.
        """

        ENDPOINT_METHOD = "eth_sendPrivateTransaction"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        if isinstance(raw_transaction, bytes):
            raw_transaction = raw_transaction.hex()

        params_dict: Dict[str, Any] = {
            "tx": raw_transaction,
        }

        if max_block_number is not None:
            params_dict["maxBlockNumber"] = max_block_number

        if preferences is not None:
            params_dict["preferences"] = preferences

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": [
                    params_dict,
                ],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result

    async def send_private_raw_transaction(
        self,
        raw_transaction: bytes | str,
        signer_key: str,
        preferences: Dict[str, Any] | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a private raw transaction.
        """

        ENDPOINT_METHOD = "eth_sendPrivateRawTransaction"

        if ENDPOINT_METHOD not in self.endpoints:  # pragma: no cover
            raise ValueError(
                f"{ENDPOINT_METHOD} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        if isinstance(raw_transaction, bytes):
            raw_transaction = raw_transaction.hex()

        params: List[str | Dict[str, Any]] = [
            raw_transaction,
        ]

        if preferences is not None:
            params.append(preferences)

        payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": ENDPOINT_METHOD,
                "params": params,
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
        return result
