import json
from collections.abc import Iterable
from typing import TYPE_CHECKING, Annotated, Any, Literal

import aiohttp
import eth_account.datastructures
import eth_account.messages
import eth_account.signers.local
import pydantic
from eth_typing import HexStr

from degenbot.exceptions import DegenbotValueError, ExternalServiceError
from degenbot.functions import eip_191_hash

if TYPE_CHECKING:
    from eth_account.signers.local import LocalAccount
from hexbytes import HexBytes


class BuilderEndpoint:  # pragma: no cover
    """
    An external HTTP endpoint with one or more bundle-related methods defined by the Flashbots RPC
    specification at https://docs.flashbots.net/flashbots-auction/advanced/rpc-endpoint
    """

    def __init__(
        self,
        url: str,
        endpoints: Iterable[str],
        authentication_header_label: str | None = None,
    ):
        if not url.startswith(("http://", "https://")):
            raise DegenbotValueError(message="Invalid URL")
        self.url = url
        self.endpoints = tuple(endpoints)
        self.authentication_header_label = authentication_header_label

    @staticmethod
    def _build_authentication_header(
        payload: str,
        header_label: str | None,
        signer_key: str | None,
    ) -> dict[str, Any]:
        """
        Build a MIME type header with an optional EIP-191 signature of the provided payload.
        """

        bundle_headers = {"Content-Type": "application/json"}

        if signer_key and header_label:
            signer_account: LocalAccount = eth_account.Account.from_key(signer_key)
            signer_address = signer_account.address
            message_signature = eip_191_hash(message=payload, private_key=signer_key)
            bundle_signature = f"{signer_address}:{message_signature}"
            bundle_headers[header_label] = bundle_signature

        return bundle_headers

    @staticmethod
    async def send_payload(
        url: str,
        headers: dict[str, Any],
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
        except aiohttp.ClientError as exc:
            raise ExternalServiceError(error=str(exc)) from exc
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
        state_block: int | Literal["latest"],
        signer_key: str,
        block_timestamp: int | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the eth_callBundle endpoint for simulation against state at a
        given block number
        """

        class EthCallBundleParams(pydantic.BaseModel):
            txs: list[HexStr]
            block: Annotated[
                int | Literal["latest"],
                pydantic.PlainSerializer(
                    lambda block_number: hex(block_number)
                    if isinstance(block_number, int)
                    else block_number,
                    return_type=HexStr,
                    when_used="json",
                ),
            ] = pydantic.Field(serialization_alias="blockNumber")
            state_block: Annotated[
                int | Literal["latest"],
                pydantic.PlainSerializer(
                    lambda state_block_number: hex(state_block_number)
                    if isinstance(state_block_number, int)
                    else state_block_number,
                    return_type=HexStr,
                    when_used="json",
                ),
            ] = pydantic.Field(serialization_alias="stateBlockNumber")
            timestamp: int | None = None

        class EthCallBundlePayload(pydantic.BaseModel):
            jsonrpc: str = "2.0"
            id: int = 1
            method: str = "eth_callBundle"
            params: list[EthCallBundleParams]

        endpoint_method = "eth_callBundle"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        bundle_params = EthCallBundleParams(
            txs=[tx.to_0x_hex() for tx in bundle],
            block=block_number,
            state_block=state_block,
            timestamp=block_timestamp,
        )

        payload = EthCallBundlePayload(params=[bundle_params]).model_dump_json(by_alias=True)

        bundle_headers = self._build_authentication_header(
            payload=payload,
            header_label=self.authentication_header_label,
            signer_key=signer_key,
        )

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )

    async def cancel_eth_bundle(
        self,
        uuid: str | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a cancellation for the specified UUID.
        """

        endpoint_method = "eth_cancelBundle"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise DegenbotValueError(
                message=f"Must provide signing address and key for required header {self.authentication_header_label}"  # noqa: E501
            )

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
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

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )

    async def cancel_private_transaction(
        self,
        tx_hash: HexBytes | str,
        signer_key: str,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a cancellation for the specified private transaction hash.

        The signer key used for the request must match the signer key for the transaction.
        """

        endpoint_method = "eth_cancelPrivateTransaction"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        if isinstance(tx_hash, HexBytes):
            tx_hash = tx_hash.to_0x_hex()

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
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

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )

    async def get_user_stats(
        self,
        signer_key: str,
        recent_block_number: int,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Get the Flashbots V2 user stats for the given searcher identity.
        """

        endpoint_method = "flashbots_getUserStatsV2"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
                "params": [
                    {
                        "blockNumber": hex(recent_block_number),
                    }
                ],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )

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

        endpoint_method = "flashbots_getBundleStatsV2"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
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

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )

    async def send_eth_bundle(
        self,
        bundle: Iterable[HexBytes],
        block_number: int,
        min_timestamp: int | None = None,
        max_timestamp: int | None = None,
        reverting_hashes: list[str] | None = None,
        uuid: str | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the eth_sendBundle endpoint
        """

        endpoint_method = "eth_sendBundle"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        if self.authentication_header_label is not None and signer_key is None:
            raise DegenbotValueError(
                message=f"Must provide signing address and key for required header {self.authentication_header_label}"  # noqa:E501
            )

        bundle_params: dict[str, Any] = {
            "txs": (
                # Array[String], A list of signed transactions to execute in an atomic bundle
                [tx.to_0x_hex() for tx in bundle]
            ),
            "blockNumber": (
                # String, a hex encoded block number for which this bundle is valid on
                hex(block_number)
            ),
        }

        if min_timestamp is not None:
            bundle_params[
                # (Optional) Number, the minimum timestamp for which this bundle is valid, in
                # seconds since the unix epoch
                "minTimestamp"
            ] = min_timestamp

        if max_timestamp is not None:
            bundle_params[
                # (Optional) Number, the maximum timestamp for which this bundle is valid, in
                # seconds since the unix epoch
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

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
                "params": [bundle_params],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        return await self.send_payload(
            url=self.url,
            headers=bundle_headers,
            data=payload,
            http_session=http_session,
        )

    async def send_private_transaction(
        self,
        raw_transaction: HexBytes | str,
        signer_key: str,
        max_block_number: int | None = None,
        preferences: dict[str, Any] | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a private raw transaction.
        """

        endpoint_method = "eth_sendPrivateTransaction"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        if isinstance(raw_transaction, HexBytes):
            raw_transaction = raw_transaction.to_0x_hex()

        params_dict: dict[str, Any] = {
            "tx": raw_transaction,
        }

        if max_block_number is not None:
            params_dict["maxBlockNumber"] = max_block_number

        if preferences is not None:
            params_dict["preferences"] = preferences

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
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

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )

    async def send_private_raw_transaction(
        self,
        raw_transaction: HexBytes | str,
        signer_key: str,
        preferences: dict[str, Any] | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a private raw transaction.
        """

        endpoint_method = "eth_sendPrivateRawTransaction"

        if endpoint_method not in self.endpoints:
            raise DegenbotValueError(
                message=f"{endpoint_method} was not included in the list of supported endpoints."
            )

        if isinstance(raw_transaction, HexBytes):
            raw_transaction = raw_transaction.to_0x_hex()

        params: list[str | dict[str, Any]] = [
            raw_transaction,
        ]

        if preferences is not None:
            params.append(preferences)

        payload = json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": endpoint_method,
                "params": params,
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=payload,
            signer_key=signer_key,
            header_label=self.authentication_header_label,
        )

        return await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=payload,
        )
