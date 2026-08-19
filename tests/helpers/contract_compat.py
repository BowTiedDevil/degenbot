"""eth.contract-style contract-call wrapper using AlloyProvider + degenbot.abi.

Historical note: this replaced the retired ``web3.Web3(url).eth.contract``

wrapper from the parity tests: it uses the Rust ``AlloyProvider`` for
``eth_call`` and the Rust-backed ``degenbot.abi`` core for ABI
encoding/decoding, so no ``web3``/``eth_abi`` runtime dependency is needed.

Supports both positional and keyword arguments (web3's contract interface
accepts both ``.functions.foo(a, b)`` and ``.functions.foo(arg1=a, arg2=b)``).
"""

from __future__ import annotations

from typing import Any

from degenbot.abi import decode as abi_decode
from degenbot.abi import encode as abi_encode
from degenbot.crypto import function_selector
from degenbot.exceptions import ContractLogicError
from degenbot.provider import AlloyProvider
from degenbot.utils.bytes import to_bytes


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
        encoded_args = abi_encode(types=input_types, args=all_args) if input_types else b""
        calldata = selector + encoded_args

        # Execute eth_call
        raw = self._provider.call(self._address, calldata, block_identifier)
        raw_bytes = to_bytes(raw)

        # Handle reverts (empty data means revert)
        if raw_bytes is None or len(raw_bytes) == 0:
            msg = "execution reverted"
            raise ContractLogicError(msg)

        # Decode return values
        if not output_types:
            return None

        decoded = abi_decode(types=output_types, data=raw_bytes)

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
        self._method_names = {entry["name"] for entry in abi if entry.get("type") == "function"}

    def __getattr__(self, name: str) -> Any:
        if name not in self._method_names:
            msg = f"Function {name} not found in ABI"
            raise AttributeError(msg)

        def _call(*args: Any, **kwargs: Any) -> _FunctionsResult:
            return _FunctionsResult(
                self._provider, self._address, self._abi, name, list(args), kwargs
            )

        return _call


class ContractCompat:
    """eth.contract-style function-call surface backed by AlloyProvider + degenbot.abi."""

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
) -> ContractCompat:
    """Create a web3-compatible contract wrapper.

    Usage::

        contract_compat = make_contract(fork.http_url, POOL_ADDR, POOL_ABI)
        result = contract_compat.functions.getAmountOut(amountIn=100, tokenIn=addr).call()

    """
    provider = AlloyProvider(provider_url)
    return ContractCompat(address, abi, provider)
