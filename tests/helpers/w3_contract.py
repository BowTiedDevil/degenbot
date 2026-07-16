"""web3.eth.contract compatibility wrapper using AlloyProvider + eth_abi.

Replaces ``web3.Web3(url).eth.contract(address=addr, abi=ABI)`` with a
shim that uses the Rust ``AlloyProvider`` for ``eth_call`` and ``eth_abi``
for ABI encoding/decoding, eliminating the ``web3`` runtime dependency for
on-chain parity tests.

Supports both positional and keyword arguments (web3's contract interface
accepts both ``.functions.foo(a, b)`` and ``.functions.foo(arg1=a, arg2=b)``).
"""

from __future__ import annotations

from typing import Any

import eth_abi
from hexbytes import HexBytes

from degenbot.crypto import function_selector
from degenbot.exceptions import ContractLogicError
from degenbot.provider import AlloyProvider


class _FunctionsResult:
    """Pending contract call — call ``.call()`` to execute."""

    def __init__(
        self,
        provider: AlloyProvider,
        address: str,
        abi: list[dict[str, Any]],
        method_name: str,
        args: list[Any],
        kwargs: dict[str, Any],
    ) -> None:
        self._provider = provider
        self._address = address
        self._abi = abi
        self._method_name = method_name
        self._args = args
        self._kwargs = kwargs

    def call(self, block_identifier: int | None = None) -> Any:
        func_entry = _find_abi_entry(self._abi, self._method_name)
        input_types = [i["type"] for i in func_entry["inputs"]]
        output_types = [o["type"] for o in func_entry.get("outputs", [])]

        # Build the function selector
        sig = f"{self._method_name}({','.join(input_types)})"
        selector = function_selector(sig)

        # Encode args — merge positional and keyword (in ABI order)
        all_args = [*self._args, *self._kwargs.values()]
        if input_types:
            encoded_args = eth_abi.abi.encode(types=input_types, args=all_args)
        else:
            encoded_args = b""
        calldata = selector + encoded_args

        # Execute eth_call
        raw = self._provider.call(self._address, calldata, block_identifier)
        raw_bytes = HexBytes(raw)

        # Handle reverts (empty data means revert)
        if raw_bytes is None or len(raw_bytes) == 0:

            msg = "execution reverted"
            raise ContractLogicError(msg)

        # Decode return values
        if not output_types:
            return None

        decoded = eth_abi.abi.decode(types=output_types, data=raw_bytes)

        # Unwrap single-element tuples (web3 returns single value for single return)
        if len(decoded) == 0:
            return None
        if len(decoded) == 1:
            return decoded[0]
        return tuple(decoded)


class _FunctionsAccessor:
    """``.functions.methodName(*args, **kwargs)`` accessor."""

    def __init__(self, provider: AlloyProvider, address: str, abi: list[dict[str, Any]]) -> None:
        self._provider = provider
        self._address = address
        self._abi = abi
        # Pre-build the set of available function names
        self._method_names = {
            entry["name"] for entry in abi if entry.get("type") == "function"
        }

    def __getattr__(self, name: str) -> Any:
        if name not in self._method_names:
            msg = f"Function {name} not found in ABI"
            raise AttributeError(msg)

        def _call(*args: Any, **kwargs: Any) -> _FunctionsResult:
            return _FunctionsResult(
                self._provider, self._address, self._abi, name, list(args), kwargs
            )

        return _call


class W3ContractCompat:
    """web3.eth.contract replacement backed by AlloyProvider + eth_abi."""

    def __init__(self, address: str, abi: list[dict[str, Any]], provider: AlloyProvider) -> None:
        self._provider = provider
        self._address = address
        self._abi = abi

    @property
    def address(self) -> str:
        """The contract address."""
        return self._address

    @property
    def functions(self) -> _FunctionsAccessor:
        return _FunctionsAccessor(self._provider, self._address, self._abi)


def _find_abi_entry(abi: list[dict[str, Any]], method_name: str) -> dict[str, Any]:
    """Find a function entry in the ABI by name."""
    for entry in abi:
        if entry.get("name") == method_name and entry.get("type") == "function":
            return entry
    msg = f"Method {method_name} not found in ABI"
    raise ValueError(msg)


def make_contract(
    provider_url: str,
    address: str,
    abi: list[dict[str, Any]],
) -> W3ContractCompat:
    """Create a web3-compatible contract wrapper.

    Usage::

        w3_contract = make_contract(fork.http_url, POOL_ADDR, POOL_ABI)
        result = w3_contract.functions.getAmountOut(amountIn=100, tokenIn=addr).call()

    """
    provider = AlloyProvider(provider_url)
    return W3ContractCompat(address, abi, provider)
