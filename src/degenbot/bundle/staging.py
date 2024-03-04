from typing import Any, Dict, Iterable

import aiohttp
import ujson
from hexbytes import HexBytes


class stuff:
    async def send_mev_bundle(
        self,
        bundle: Iterable[HexBytes],
        block_number: int,
        max_block_number: int | None = None,
        signer_key: str | None = None,
        http_session: aiohttp.ClientSession | None = None,
    ) -> Any:
        """
        Send a formatted bundle to the mev_sendBundle endpoint
        """

        if "mev_sendBundle" not in self.endpoints:
            raise ValueError("eth_sendBundle was not included in the list of supported endpoints.")

        if self.authentication_header_label is not None and signer_key is None:
            raise ValueError(
                f"Must provide signing address and key for required header {self.authentication_header_label}"
            )

        mev_bundle_params: Dict[str, Any] = {
            "version": "v0.1",
            "inclusion": {
                "block": hex(block_number),
                # optional - "maxBlock": hex(number)
            },
            "body": [
                """
                {"hash": "string"} | 
                {"tx": "string", "canRevert": "bool"} |
                {"bundle": "MevSendBundleParams"}
                """
            ],
            # optional
            "validity": {
                # optional
                "refund": [
                    {
                        "bodyIdx": "int",
                        "percent": "int",
                    }
                ],
                # optional
                "refundConfig": [
                    {
                        "address": "string",
                        "percent": "number",
                    }
                ],
            },
            # optional
            "privacy": {
                # optional
                "hints": [
                    """                    
                    "calldata" |
                    "contract_address" |
                    "logs" |
                    "function_selector" |
                    "hash" |
                    "tx_hash"
                    """
                ],
                # optional
                "builders": ["string", "string"],
            },
            # optional
            "metadata": {
                # optional
                "originId": "string",
            },
        }

        if max_block_number is not None:
            mev_bundle_params["inclusion"]["maxBlock"] = hex(max_block_number)

        bundle_payload = ujson.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "mev_sendBundle",
                "params": [mev_bundle_params],
            }
        )

        bundle_headers = self._build_authentication_header(
            payload=bundle_payload,
            header_label=self.authentication_header_label,
            signer_key=signer_key,
        )

        result = await self.send_payload(
            url=self.url,
            http_session=http_session,
            headers=bundle_headers,
            data=bundle_payload,
        )
        return result
