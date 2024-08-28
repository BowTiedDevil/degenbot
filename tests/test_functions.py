import pytest
from eth_typing import (
    BlockNumber,
    Hash32,
    HexStr,
)
from hexbytes import HexBytes

import degenbot.config
from degenbot.functions import get_number_for_block_identifier, next_base_fee


def test_converting_block_identifier_to_int(fork_mainnet_archive):
    """
    Check that all inputs for web3 type `BlockIdentifier` can be converted to an integer
    """

    degenbot.config.set_web3(fork_mainnet_archive.w3)

    # Known string literals
    latest_block = get_number_for_block_identifier("latest")
    earliest_block = get_number_for_block_identifier("earliest")
    pending_block = get_number_for_block_identifier("pending")
    safe_block = get_number_for_block_identifier("safe")
    finalized_block = get_number_for_block_identifier("finalized")

    assert isinstance(latest_block, int)
    assert isinstance(earliest_block, int)
    assert isinstance(pending_block, int)
    assert isinstance(safe_block, int)
    assert isinstance(finalized_block, int)

    assert latest_block != earliest_block != pending_block != safe_block != finalized_block

    # BlockNumber
    assert isinstance(
        get_number_for_block_identifier(BlockNumber(1)),
        int,
    )

    # Hash32
    assert isinstance(
        get_number_for_block_identifier(Hash32((1).to_bytes(length=32, byteorder="big"))),
        int,
    )

    # HexStr
    assert isinstance(
        get_number_for_block_identifier(HexStr("0x" + (128).to_bytes(32, byteorder="big").hex())),
        int,
    )

    # HexBytes
    assert isinstance(get_number_for_block_identifier(HexBytes(1)), int)

    # int
    assert isinstance(get_number_for_block_identifier(1), int)

    for invalid_tag in ["Latest", "latest ", "next", "previous"]:
        with pytest.raises(ValueError):
            get_number_for_block_identifier(invalid_tag)  # type: ignore[arg-type]


def test_fee_calcs():
    BASE_FEE = 100 * 10**9

    # EIP-1559 target is 50% full blocks, so a 50% full block should return the same base fee
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=15_000_000,
            parent_gas_limit=30_000_000,
        )
        == BASE_FEE
    )

    # Fee should be higher
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=20_000_000,
            parent_gas_limit=30_000_000,
        )
        == 104166666666
    )

    # Fee should be lower
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=10_000_000,
            parent_gas_limit=30_000_000,
        )
        == 95833333334
    )

    MIN_BASE_FEE = 95 * 10**9

    # Enforce minimum fee
    assert (
        next_base_fee(
            parent_base_fee=BASE_FEE,
            parent_gas_used=0,
            parent_gas_limit=30_000_000,
            min_base_fee=MIN_BASE_FEE,
        )
        == MIN_BASE_FEE
    )
