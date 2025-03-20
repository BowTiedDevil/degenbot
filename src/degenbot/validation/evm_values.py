from typing import Annotated

from pydantic import Field

from degenbot.constants import (
    MAX_INT16,
    MAX_INT24,
    MAX_INT128,
    MAX_INT256,
    MAX_UINT8,
    MAX_UINT24,
    MAX_UINT128,
    MAX_UINT160,
    MAX_UINT256,
    MIN_INT16,
    MIN_INT24,
    MIN_INT128,
    MIN_INT256,
    MIN_UINT8,
    MIN_UINT24,
    MIN_UINT128,
    MIN_UINT160,
    MIN_UINT256,
)

type ValidatedInt16 = Annotated[int, Field(strict=True, ge=MIN_INT16, le=MAX_INT16)]
type ValidatedInt24 = Annotated[int, Field(strict=True, ge=MIN_INT24, le=MAX_INT24)]
type ValidatedInt128 = Annotated[int, Field(strict=True, ge=MIN_INT128, le=MAX_INT128)]
type ValidatedInt256 = Annotated[int, Field(strict=True, ge=MIN_INT256, le=MAX_INT256)]

type ValidatedUint8 = Annotated[int, Field(strict=True, ge=MIN_UINT8, le=MAX_UINT8)]
type ValidatedUint24 = Annotated[int, Field(strict=True, ge=MIN_UINT24, le=MAX_UINT24)]
type ValidatedUint128 = Annotated[int, Field(strict=True, ge=MIN_UINT128, le=MAX_UINT128)]
type ValidatedUint128NonZero = Annotated[int, Field(strict=True, gt=MIN_UINT128, le=MAX_UINT128)]
type ValidatedUint160 = Annotated[int, Field(strict=True, ge=MIN_UINT160, le=MAX_UINT160)]
type ValidatedUint160NonZero = Annotated[int, Field(strict=True, gt=MIN_UINT160, le=MAX_UINT160)]
type ValidatedUint256 = Annotated[int, Field(strict=True, ge=MIN_UINT256, le=MAX_UINT256)]
type ValidatedUint256NonZero = Annotated[int, Field(strict=True, gt=MIN_UINT256, le=MAX_UINT256)]
