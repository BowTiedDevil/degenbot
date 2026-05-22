"""Balancer V2 math constants (ONE, MIN_TOKEN_BALANCES, PowVersion)."""
import enum

ONE = 1 * 10**18
TWO = 2 * 10**18
FOUR = 4 * 10**18

MAX_POW_RELATIVE_ERROR = 10000


class PowVersion(enum.Enum):
    """
    Identifies which FixedPoint.pow implementation a deployed pool contract uses.

    Different deployed versions of Balancer V2 WeightedPool contracts embed different
    versions of the FixedPoint library:

    - V1 (no fast paths): older contracts like WeightedPool2Tokens use the general
      LogExpMath.pow path with MAX_POW_RELATIVE_ERROR error bound for all exponent
      values. This was the original implementation.
    - V2 (with fast paths): newer WeightedPool contracts include fast paths for
      y == 1, y == 2, and y == 4 that bypass the error-bound calculation, producing
      different rounding behavior.

    The version is detected from the pool's deployed bytecode at build time and
    stored on the pool instance so swap calculations match the on-chain contract
    exactly.
    """

    V1 = "v1"  # General path only (no fast paths) — e.g. WeightedPool2Tokens
    V2 = "v2"  # Fast paths for y == ONE, TWO, FOUR — e.g. WeightedPool
