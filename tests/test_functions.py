import pytest
import web3
from eth_typing import BlockNumber, Hash32, HexStr
from eth_utils.crypto import keccak
from hexbytes import HexBytes

import degenbot.config
from degenbot import AnvilFork
from degenbot.cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.functions import (
    create2_address,
    encode_function_calldata,
    extract_argument_types_from_function_prototype,
    get_number_for_block_identifier,
    next_base_fee,
    raw_call,
)


def test_extract_argument_types_from_function_prototype():
    assert extract_argument_types_from_function_prototype("func()") == []
    assert extract_argument_types_from_function_prototype("func(uint256)") == [
        "uint256",
    ]
    assert extract_argument_types_from_function_prototype("func(uint256,address)") == [
        "uint256",
        "address",
    ]
    assert extract_argument_types_from_function_prototype("func(uint256,address,bytes)") == [
        "uint256",
        "address",
        "bytes",
    ]
    assert extract_argument_types_from_function_prototype("func(uint256,address,bytes[])") == [
        "uint256",
        "address",
        "bytes[]",
    ]


def test_encode_function_calldata():
    assert (
        encode_function_calldata(function_prototype="factory()", function_arguments=[])
        == HexBytes("0xc45a01550ceb4bc5c6b2e6f722b5033a03078f9bd6673457375ba94c26ac1cf0")[:4]
    )
    assert encode_function_calldata(
        function_prototype="transfer(address,uint256)",
        function_arguments=[
            "0xA69babEF1cA67A37Ffaf7a485DfFF3382056e78C",
            26535330612692929974,
        ],
    ) == HexBytes(
        "0xa9059cbb000000000000000000000000a69babef1ca67a37ffaf7a485dfff3382056e78c00000000000000000000000000000000000000000000000170406e9a1f1c4db6"
    )


def test_low_level_call_for_factory_address(ethereum_archive_node_web3: web3.Web3):
    degenbot.config.set_web3(ethereum_archive_node_web3)

    pool_address = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")

    function_prototype = "factory()"

    (result,) = raw_call(
        w3=ethereum_archive_node_web3,
        block_identifier=ethereum_archive_node_web3.eth.block_number,
        address=pool_address,
        calldata=encode_function_calldata(
            function_prototype=function_prototype,
            function_arguments=extract_argument_types_from_function_prototype(function_prototype),
        ),
        return_types=["address"],
    )
    assert get_checksum_address(result) == get_checksum_address(
        "0x1F98431c8aD98523631AE4a59f267346ea31F984"
    )


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

    w3 = fork_mainnet.w3
    degenbot.config.set_web3(w3)

    # Known string literals
    latest_block = get_number_for_block_identifier("latest", w3)
    earliest_block = get_number_for_block_identifier("earliest", w3)
    pending_block = get_number_for_block_identifier("pending", w3)
    safe_block = get_number_for_block_identifier("safe", w3)
    finalized_block = get_number_for_block_identifier("finalized", w3)

    assert isinstance(latest_block, int)
    assert isinstance(earliest_block, int)
    assert isinstance(pending_block, int)
    assert isinstance(safe_block, int)
    assert isinstance(finalized_block, int)

    assert latest_block != earliest_block != pending_block != safe_block != finalized_block

    # BlockNumber
    assert isinstance(
        get_number_for_block_identifier(BlockNumber(1), w3),
        int,
    )

    # Hash32
    assert isinstance(
        get_number_for_block_identifier(Hash32((1).to_bytes(length=32, byteorder="big")), w3),
        int,
    )

    # HexStr
    assert isinstance(
        get_number_for_block_identifier(
            HexStr("0x" + (128).to_bytes(32, byteorder="big").hex()), w3
        ),
        int,
    )

    # HexBytes
    assert isinstance(get_number_for_block_identifier(HexBytes(1), w3), int)

    # int
    assert isinstance(get_number_for_block_identifier(1, w3), int)

    for invalid_tag in ["Latest", "latest ", "next", "previous"]:
        with pytest.raises(DegenbotValueError):
            get_number_for_block_identifier(invalid_tag, w3)  # type: ignore[arg-type]

    with pytest.raises(DegenbotValueError):
        get_number_for_block_identifier(1.0, w3)  # type: ignore[arg-type]


def test_fee_calcs():
    base_fee = 100 * 10**9

    # EIP-1559 target is 50% full blocks, so a 50% full block should return the same base fee
    assert (
        next_base_fee(
            parent_base_fee=base_fee,
            parent_gas_used=15_000_000,
            parent_gas_limit=30_000_000,
        )
        == base_fee
    )

    # Fee should be higher
    assert (
        next_base_fee(
            parent_base_fee=base_fee,
            parent_gas_used=20_000_000,
            parent_gas_limit=30_000_000,
        )
        == 104166666666
    )

    # Fee should be lower
    assert (
        next_base_fee(
            parent_base_fee=base_fee,
            parent_gas_used=10_000_000,
            parent_gas_limit=30_000_000,
        )
        == 95833333334
    )

    min_base_fee = 95 * 10**9

    # Enforce minimum fee
    assert (
        next_base_fee(
            parent_base_fee=base_fee,
            parent_gas_used=0,
            parent_gas_limit=30_000_000,
            min_base_fee=min_base_fee,
        )
        == min_base_fee
    )
