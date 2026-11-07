"""`Erc20Builder.build` keeps its documented `DegenbotValueError` contract.

A build that fails before admission raises the typed build-failure taxonomy
(``PoolBuildRpcError`` / ``PoolBuildDecodeError``, both under
``PoolRegistrationError`` — a ``ValueError``). None of those is a
``RuntimeError``, so the builder's translation must catch the admission parent;
catching ``RuntimeError`` alone lets a decode refusal escape the documented
contract.

The translation re-derives NOTHING: the message is the seam's FINAL text
(rendered once, in the PyO3 adapter) passed through verbatim, and ``__cause__``
carries the typed refusal with its ``kind`` / ``detail``. There is no prefix
stripping and no downstream reclassification.
"""

from __future__ import annotations

import json

import pytest

from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import (
    DegenbotValueError,
    PoolBuildDecodeError,
    PoolBuildRpcError,
    PoolRegistrationError,
)
from degenbot.registry import TokenRegistry
from tests.helpers.seam import AlloyProvider as RustAlloyProvider
from tests.helpers.seam import Bot as _Engine
from tests.helpers.seam import BuildFailureKind

# A random address no cassette entry deploys code for.
NO_CODE_ADDRESS = get_checksum_address("0x1234567890123456789012345678901234567890")


def test_pool_build_errors_are_not_runtime_errors() -> None:
    """The catch shape's premise: the taxonomy hangs off ValueError, not RuntimeError."""
    assert issubclass(PoolRegistrationError, ValueError)
    assert not issubclass(PoolRegistrationError, RuntimeError)
    assert issubclass(PoolBuildDecodeError, PoolRegistrationError)
    assert issubclass(PoolBuildRpcError, PoolRegistrationError)


def test_no_contract_build_raises_degenbot_value_error() -> None:
    """A decode refusal (`No contract deployed`) surfaces as DegenbotValueError."""
    py_bot = _Engine(chain_id=1)
    py_bot.attach_construction_io(
        RustAlloyProvider.offline_from_json_string(
            json.dumps({
                "chain_id": 1,
                "block_number": 100,
                "timestamp": 1_700_000_000,
                "calls": {},
                "code": {},
            })
        ),
        None,
    )
    erc20 = Erc20Builder(
        default_chain_id=1,
        tokens=TokenRegistry(py_bot=py_bot),
        py_bot=py_bot,
    )

    with pytest.raises(DegenbotValueError) as exc_info:
        erc20.build(NO_CODE_ADDRESS, chain_id=1, silent=True)

    # The final text passes through verbatim — one renderer, upstream — and
    # the typed refusal survives as the cause with its typed data.
    cause = exc_info.value.__cause__
    assert isinstance(cause, PoolBuildDecodeError)
    assert exc_info.value.message == str(cause)
    assert exc_info.value.message == "No contract deployed at this address"
    assert cause.kind == BuildFailureKind.Decoding
    assert cause.detail == "No contract deployed at this address"
    # Nothing downstream re-classifies: the wrapper's message carries no
    # edited form of the refusal's text.
    assert set(vars(cause)) == {"kind", "detail"}
