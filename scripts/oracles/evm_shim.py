"""Revert/constant vocabulary the restored oracle modules share.

The reference math under this directory was retired from the Python driver, and
these modules are its restored copies. They keep their own local copies of the
few names they used to import from the production tree, so nothing here is
importable as ``degenbot``: an oracle that imported the driver it once validated
would not be an independent check.
"""

from __future__ import annotations

import enum

ONE = 1 * 10**18
TWO = 2 * 10**18
FOUR = 4 * 10**18

MAX_POW_RELATIVE_ERROR = 10000

MIN_UINT256 = 0
MAX_UINT256 = 2**256 - 1


class PowVersion(enum.Enum):
    """Which deployed ``FixedPoint.pow`` implementation a pool contract embeds.

    ``V1`` is the general ``LogExpMath.pow`` path with the
    ``MAX_POW_RELATIVE_ERROR`` bound (2021-era ``WeightedPool2Tokens``).
    ``V2`` adds the ``y == ONE / TWO / FOUR`` fast paths (post-2022
    ``WeightedPool``), which bypass the error bound and so round differently.
    """

    V1 = "v1"
    V2 = "v2"


class EVMRevertError(Exception):
    """An on-chain revert, carrying the contract's reason string.

    The generators render these as the snapshots' ``ERROR:<reason>`` rows.
    """

    def __init__(self, *, error: str) -> None:
        """Record the revert reason.

        Args:
            error: The contract's revert reason string.
        """
        super().__init__(error)
        self.error = error


class DegenbotValueError(Exception):
    """A caller-input error (the retired driver exception, restored locally)."""

    def __init__(self, *, message: str = "") -> None:
        """Record the message.

        Args:
            message: The failure description.
        """
        super().__init__(message)
        self.message = message


class InvalidUint256(Exception):
    """A value outside the ``uint256`` range."""


def raise_if_invalid_uint256(number: int) -> None:
    """Reject a value outside the ``uint256`` range.

    Args:
        number: The value to range-check.

    Raises:
        InvalidUint256: The value does not fit ``uint256``.
    """
    if (MIN_UINT256 <= number <= MAX_UINT256) is False:
        raise InvalidUint256
