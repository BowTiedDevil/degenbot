import pytest
from eth_typing import BlockNumber, Hash32, HexStr
from eth_utils.crypto import keccak
from hexbytes import HexBytes

import degenbot.config
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.functions import create2_address, get_number_for_block_identifier, next_base_fee


def test_create2():
    """
    Tests taken from https://eips.ethereum.org/EIPS/eip-1014
    """

    assert (
        create2_address(
            deployer="0x0000000000000000000000000000000000000000",
            salt="0x0000000000000000000000000000000000000000000000000000000000000000",
            init_code_hash=keccak(hexstr="0x00"),
        )
        == "0x4D1A2e2bB4F88F0250f26Ffff098B0b30B26BF38"
    )
    assert (
        create2_address(
            deployer="0xdeadbeef00000000000000000000000000000000",
            salt="0x0000000000000000000000000000000000000000000000000000000000000000",
            init_code_hash=keccak(hexstr="0x00"),
        )
        == "0xB928f69Bb1D91Cd65274e3c79d8986362984fDA3"
    )
    assert (
        create2_address(
            deployer="0xdeadbeef00000000000000000000000000000000",
            salt="0x000000000000000000000000feed000000000000000000000000000000000000",
            init_code_hash=keccak(hexstr="0x00"),
        )
        == "0xD04116cDd17beBE565EB2422F2497E06cC1C9833"
    )
    assert (
        create2_address(
            deployer="0x0000000000000000000000000000000000000000",
            salt="0x0000000000000000000000000000000000000000000000000000000000000000",
            init_code_hash=keccak(hexstr="0xdeadbeef"),
        )
        == "0x70f2b2914A2a4b783FaEFb75f459A580616Fcb5e"
    )
    assert (
        create2_address(
            deployer="0x00000000000000000000000000000000deadbeef",
            salt="0x00000000000000000000000000000000000000000000000000000000cafebabe",
            init_code_hash=keccak(hexstr="0xdeadbeef"),
        )
        == "0x60f3f640a8508fC6a86d45DF051962668E1e8AC7"
    )
    assert (
        create2_address(
            deployer="0x00000000000000000000000000000000deadbeef",
            salt="0x00000000000000000000000000000000000000000000000000000000cafebabe",
            init_code_hash=keccak(
                hexstr="0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
            ),
        )
        == "0x1d8bfDC5D46DC4f61D6b6115972536eBE6A8854C"
    )


def test_converting_block_identifier_to_int(fork_mainnet: AnvilFork):
    """
    Check that all inputs for web3 type `BlockIdentifier` can be converted to an integer
    """

    degenbot.config.set_web3(fork_mainnet.w3)

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

    with pytest.raises(ValueError):
        get_number_for_block_identifier(1.0)  # type: ignore[arg-type]


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
