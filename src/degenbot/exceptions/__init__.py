"""Exception classes for degenbot.

``degenbot.exceptions`` is the single documented import home for every
FFI-raised exception. The FFI-raised types (the ``PoolRegistrationError``
fairly, the verifier errors) are **direct aliases**
of the ``degenbot._ffi`` pyclasses — never Python subclasses: Rust raises
the pyclass instances, so ``except`` / ``isinstance`` matching requires the
exact same class object. The identity contract is pinned by
``tests/rust/test_exception_reexport_identity.py`` and
``tests/rust/test_updater_reexport_identity.py``.
"""

from degenbot._ffi import (
    # FF-T1 (BPHR6F): the typed fleet boot refusal — the library never
    # aborts the host process on the boot-refusal arm; the binary maps
    # this exception to its loud named fail-fast exit.
    BootRefused,
    DynamicFeePoolRejectedError,
    HighFeePoolRejectedError,
    HookedPoolRejectedError,
    PathRegistryFullError,
    PoolAlreadyRegisteredError,
    PoolRegistrationError,
    SpecViolationError,
)
from degenbot.exceptions.arbitrage import (
    ArbCalculationError,
    ArbitrageError,
    DirectionResolutionError,
    DuplicatePoolError,
    HopCountExceededError,
    HopCountInsufficientError,
    IncompatiblePoolInvariant,
    InsufficientLiquidityError,
    InvalidForwardAmount,
    InvalidSwapPathError,
    NoLiquidity,
    NoSolverSolution,
    OptimizationError,
    PathRejectedError,
    RateOfExchangeBelowMinimum,
    TokenDenylistedError,
    Unprofitable,
)
from degenbot.exceptions.base import DegenbotError, DegenbotTypeError, DegenbotValueError
from degenbot.exceptions.infrastructure import (
    AnvilError,
    BackupExists,
    Erc20TokenError,
    NoPriceOracle,
)
from degenbot.exceptions.pool import (
    AddressMismatch,
    BrokenPool,
    CurveError,
    EVMRevertError,
    ExternalUpdateError,
    HookedPoolResult,
    IncompleteSwap,
    InvalidSwapInputAmount,
    InvalidUint256,
    LateUpdateError,
    LiquidityMapWordMissing,
    LiquidityPoolError,
    MissingCurveData,
    NoPoolStateAvailable,
    PoolCreationFailed,
    PoolNotAssociated,
    PossibleInaccurateResult,
    StaleRateResult,
    TrackerAlreadyInitialized,
    TrackerError,
    UnknownPool,
    UnknownPoolId,
)
from degenbot.exceptions.rpc import (
    ContractLogicError,
    RpcError,
    TransactionNotFound,
)
from degenbot.exceptions.verification import (
    VerificationMismatchError,
    VerificationRpcError,
)

__all__ = (
    "AddressMismatch",
    "AnvilError",
    "ArbCalculationError",
    "ArbitrageError",
    "BackupExists",
    "BootRefused",
    "BrokenPool",
    "ContractLogicError",
    "CurveError",
    "DegenbotError",
    "DegenbotTypeError",
    "DegenbotValueError",
    "DirectionResolutionError",
    "DuplicatePoolError",
    "DynamicFeePoolRejectedError",
    "EVMRevertError",
    "Erc20TokenError",
    "ExternalUpdateError",
    "HighFeePoolRejectedError",
    "HookedPoolRejectedError",
    "HookedPoolResult",
    "HopCountExceededError",
    "HopCountInsufficientError",
    "IncompatiblePoolInvariant",
    "IncompleteSwap",
    "InsufficientLiquidityError",
    "InvalidForwardAmount",
    "InvalidSwapInputAmount",
    "InvalidSwapPathError",
    "InvalidUint256",
    "LateUpdateError",
    "LiquidityMapWordMissing",
    "LiquidityPoolError",
    "MissingCurveData",
    "NoLiquidity",
    "NoPoolStateAvailable",
    "NoPriceOracle",
    "NoSolverSolution",
    "OptimizationError",
    "PathRegistryFullError",
    "PathRejectedError",
    "PoolAlreadyRegisteredError",
    "PoolCreationFailed",
    "PoolNotAssociated",
    "PoolRegistrationError",
    "PossibleInaccurateResult",
    "RateOfExchangeBelowMinimum",
    "RpcError",
    "SpecViolationError",
    "StaleRateResult",
    "TokenDenylistedError",
    "TrackerAlreadyInitialized",
    "TrackerError",
    "TransactionNotFound",
    "UnknownPool",
    "UnknownPoolId",
    "Unprofitable",
    "VerificationMismatchError",
    "VerificationRpcError",
)
