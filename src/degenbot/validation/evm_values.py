"""EVM value validators (uint256, address, etc.)."""

from typing import Annotated

from pydantic import Field

from degenbot.constants import (
    MAX_INT128,
    MAX_UINT128,
    MAX_UINT256,
    MIN_INT128,
    MIN_UINT128,
    MIN_UINT256,
)

type ValidatedInt128 = Annotated[int, Field(ge=MIN_INT128, le=MAX_INT128)]

type ValidatedUint128 = Annotated[int, Field(ge=MIN_UINT128, le=MAX_UINT128)]
type ValidatedUint256 = Annotated[int, Field(ge=MIN_UINT256, le=MAX_UINT256)]
