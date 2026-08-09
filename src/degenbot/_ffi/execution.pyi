"""Stub for the dynamically-created ``degenbot._ffi.execution`` submodule.

Created at runtime by ``add_execution_module`` in the PyO3 wrapper crate
(``degenbot-python/src/execution/mod.rs``). ADR-025 lift: adapt an arbitrary
Python callable (``SolveResult -> bytes``) into the core
``PayloadComposer`` / ``ExecutionStrategy`` seam (Polars ``map_elements``
model — Rust holds the ``Py<PyAny>`` and calls back under the GIL).

This is the foreign-contract path (a user's own execution contract); nothing
here is threaded into the canonical ``dispatch_profitable_*`` fan-out
(ADR-025 D3).
"""

from collections.abc import Callable
from typing import Any, Self

__all__ = ["PyPayloadComposer", "PySolveResult", "abi_encode_call"]

class PySolveResult:
    """The typed solve-result view passed to a compose callable (ADR-025 D4)."""

    @property
    def path_id(self) -> int:
        """The path id."""

    @property
    def hop_count(self) -> int:
        """Number of hops in the path."""

    @property
    def optimal_input(self) -> int:
        """The flash input amount (uint256 integer)."""

    @property
    def hop_outputs(self) -> list[int]:
        """Per-hop output amounts (uint256 integers)."""

    @property
    def consumed_inputs(self) -> list[int]:
        """Per-hop consumed input amounts (uint256 integers)."""

    @property
    def net_profit(self) -> int:
        """The net profit (uint256 integer)."""

    @property
    def hop_descriptors(self) -> list[dict[str, Any]]:
        """Per-hop descriptors: ``{family, pool_address, token0, token1, zfo, v4_pool_id}``."""

class PyPayloadComposer:
    """Wrap a Python callable (``result: PySolveResult -> bytes``) into the execution seam.

    Implements the core ``PayloadComposer`` / ``ExecutionStrategy`` trait.

    Args:
        callback: A callable taking a ``PySolveResult`` and returning the
            ``bytes`` payload for the composer's own execution contract.

    Raises:
        TypeError: If ``callback`` is not callable.

    """

    def __new__(cls, callback: Callable[[PySolveResult], bytes]) -> Self: ...

def abi_encode_call(signature: str, values: list[Any]) -> bytes:
    """ABI-encode a call against the caller's own contract.

    Args:
        signature: A Solidity function signature (``"transfer(address,uint256)"``).
        values: The argument list consumed left-to-right.

    Returns:
        The full calldata: 4-byte function selector followed by the
        ABI-encoded arguments. Backed by the canonical ``degenbot.abi``
        encoder.

    Raises:
        ValueError: If the signature cannot be parsed or values cannot be
            encoded.

    """
