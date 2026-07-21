"""Provider/RPC-layer exceptions owned by degenbot.

These are the degenbot-owned equivalents of the ``web3.exceptions`` types the
codebase previously imported: ``Web3Exception`` (the broad RPC base),
``ContractLogicError`` (an ``eth_call`` execution revert), and
``TransactionNotFound`` (a transaction hash the provider could not find).

Owning them here lets probe sites throughout the codebase
(:mod:`degenbot.builders.type_resolution`, :mod:`degenbot.builders.erc20_builder`,
:mod:`degenbot.curve.detection`) catch a single degenbot-owned type regardless
of which provider backend (Alloy or the transitional web3 backend) raised the
failure. The adapter seam
(:mod:`degenbot.provider.sync_adapter`, :mod:`degenbot.provider.async_adapter`,
:mod:`degenbot.provider.alloy_errors`) converts each backend's native failure
into these types so downstream code is backend-agnostic.

Layering note: these are **provider-layer** exceptions — the RPC node reported a
revert or a missing transaction. They are distinct from the pool-math-layer
:exc:`~degenbot.exceptions.EVMRevertError` (a local pool-state simulation that
would revert). :meth:`CurveDataProvider._wrap_revert` bridges the two:
provider-layer :exc:`ContractLogicError` → pool-layer :exc:`EVMRevertError`.
"""

from degenbot.exceptions.base import DegenbotError


class RpcError(DegenbotError):
    """Base for provider/RPC-layer failures.

    The degenbot-owned equivalent of ``web3.exceptions.Web3Exception``: the
    broad base a caller can catch to mean "an RPC call failed for some reason."
    Probe sites that previously caught ``Web3Exception`` catch this instead,
    so they remain backend-agnostic once the web3 backend is retired
    (its adapter seam converts web3's native failures into
    :exc:`ContractLogicError` / :exc:`TransactionNotFound`, both subclasses
    of this base).
    """


class ContractLogicError(RpcError):
    """An ``eth_call`` execution revert reported by the provider.

    The degenbot-owned equivalent of ``web3.exceptions.ContractLogicError``.
    Raised at the provider adapter seam — by
    :func:`degenbot.provider.alloy_errors.alloy_revert_error` for the Alloy
    backend and by the web3 adapter's ``call``/``call_raw`` conversion for
    the transitional web3 backend — so probe sites catch one type across both
    backends.

    Constructed with a positional revert message (mirroring the web3 type's
    constructor) so call sites can write ``ContractLogicError("... reverted")
    `` and ``str(exc)`` returns that message.
    """

    def __init__(self, message: str = "") -> None:
        """Initialize with a revert message (empty by default)."""
        self.message = message or None
        super().__init__(message=self.message)


class TransactionNotFound(RpcError):
    """A transaction hash the provider could not find.

    The degenbot-owned equivalent of
    ``web3.exceptions.TransactionNotFound``. Raised by the provider adapter
    seam when a receipt lookup misses; callers that previously caught the
    web3 type catch this instead.
    """
