from degenbot.functions import next_base_fee, get_number_for_block_identifier
from degenbot.constants import MAX_UINT256
import pytest
import degenbot.config
from eth_typing import (
    BlockNumber,
    Hash32,
    HexStr,
)
from hexbytes import HexBytes


def test_convert_identifier(fork_mainnet_archive):
    """
    Check that all inputs for type BlockIdentifier can be converted to an integer
    """

    degenbot.config.set_web3(fork_mainnet_archive.w3)
    for block_identifier in [
        "latest",  # BlockParams
        "earliest",  # BlockParams
        "pending",  # BlockParams
        "safe",  # BlockParams
        "finalized",  # BlockParams
        BlockNumber(1),  # BlockNumber
        Hash32(int(1).to_bytes(length=32, byteorder="big")),  # Hash32
        HexStr("0x" + int(128).to_bytes(32, byteorder="big").hex()),  # HexStr
        HexBytes(1),  # HexBytes
        1,  # int
    ]:
        assert isinstance(get_number_for_block_identifier(block_identifier), int)

    for invalid_block_number in [-1, MAX_UINT256 + 1]:
        with pytest.raises(ValueError):
            get_number_for_block_identifier(invalid_block_number)

    for invalid_tag in ["Latest", "latest ", "next", "previous"]:
        with pytest.raises(ValueError):
            get_number_for_block_identifier(invalid_tag)


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
