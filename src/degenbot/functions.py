from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
import eth_account.messages
from eth_typing import BlockNumber, ChecksumAddress
from eth_utils.conversions import to_hex
from eth_utils.crypto import keccak
from hexbytes import HexBytes
from web3 import AsyncWeb3, Web3
from web3.types import BlockIdentifier

from degenbot.cache import get_checksum_address
from degenbot.constants import MAX_UINT256, MIN_UINT256
from degenbot.exceptions import DegenbotValueError, InvalidUint256

if TYPE_CHECKING:
    from eth_account.datastructures import SignedMessage


def create2_address(
    deployer: str | bytes,
    salt: bytes | str,
    init_code_hash: bytes | str,
) -> ChecksumAddress:
    """
    Generate the deterministic CREATE2 address for a given deployer, salt, and the keccak hash of
    the contract creation (init) bytecode.

    References:
        - https://eips.ethereum.org/EIPS/eip-1014
        - https://docs.openzeppelin.com/cli/2.8/deploying-with-create2
    """
    return get_checksum_address(
        keccak(HexBytes(0xFF) + HexBytes(deployer) + HexBytes(salt) + HexBytes(init_code_hash))[
            -20:
        ],  # Contract address is the least significant 20 bytes from the 32 byte hash
    )


def encode_function_calldata(
    function_prototype: str, function_arguments: list[Any] | None
) -> bytes:
    """
    Encode the calldata to execute a call to the given function prototype, with ordered arguments.
    The resulting bytes array will include the 4-byte function selector, followed by the
    ABI-encoded arguments.
    """

    if function_arguments is None:
        function_arguments = []

    return keccak(text=function_prototype)[:4] + eth_abi.abi.encode(
        types=extract_argument_types_from_function_prototype(function_prototype),
        args=function_arguments,
    )


def extract_argument_types_from_function_prototype(function_prototype: str) -> list[str]:
    """
    Extract the argument types from the function prototype.

    e.g. the argument types for the prototype 'function(address,uint256)' are ['address','uint256']
    """

    if function_args := function_prototype[
        function_prototype.find("(") + 1 : function_prototype.find(")") :
    ]:
        return function_args.split(",")

    return []


def eip_1167_clone_address(
    deployer: ChecksumAddress | str | bytes,
    implementation_contract: ChecksumAddress | str | bytes,
    salt: bytes,
) -> ChecksumAddress:
    """
    Calculate the contract address for an EIP-1167 minimal proxy contract deployed by `deployer`,
    using `salt`, delegating calls to the contract at `implementation` address.

    References:
        - https://github.com/ethereum/ercs/blob/master/ERCS/erc-1167.md
        - https://github.com/OpenZeppelin/openzeppelin-contracts/blob/master/contracts/proxy/Clones.sol
        - https://www.rareskills.io/post/eip-1167-minimal-proxy-standard-with-initialization-clone-pattern
    """

    minimal_proxy_code = (
        HexBytes("0x3d602d80600a3d3981f3")
        + HexBytes("0x363d3d373d3d3d363d73")
        + HexBytes(implementation_contract)
        + HexBytes("0x5af43d82803e903d91602b57fd5bf3")
    )

    return create2_address(
        deployer=deployer,
        salt=salt,
        init_code_hash=keccak(minimal_proxy_code),
    )


def eip_191_hash(message: str, private_key: str) -> str:
    """
    Get the signature hash (a hex-formatted string) for a given message and signing key.
    """
    result: SignedMessage = eth_account.Account.sign_message(
        signable_message=eth_account.messages.encode_defunct(
            text=to_hex(keccak(text=message)),
        ),
        private_key=private_key,
    )
    return result.signature.to_0x_hex()


def get_number_for_block_identifier(identifier: BlockIdentifier | None, w3: Web3) -> BlockNumber:
    match identifier:
        case None:
            return w3.eth.get_block_number()
        case int() as block_number_as_int:
            return cast("BlockNumber", block_number_as_int)
        case "latest" | "earliest" | "pending" | "safe" | "finalized" as block_tag:
            block = w3.eth.get_block(block_tag)
            block_number = block.get("number")
            if TYPE_CHECKING:
                assert block_number is not None
            return block_number
        case str() as block_number_as_str:
            try:
                return cast("BlockNumber", int(block_number_as_str, 16))
            except ValueError:
                raise DegenbotValueError(
                    message=f"Invalid block identifier {identifier!r}"
                ) from None
        case bytes() as block_number_as_bytes:
            return cast("BlockNumber", int.from_bytes(block_number_as_bytes, byteorder="big"))
        case _:
            raise DegenbotValueError(message=f"Invalid block identifier {identifier!r}")


async def get_number_for_block_identifier_async(
    identifier: BlockIdentifier | None, w3: AsyncWeb3
) -> BlockNumber:
    match identifier:
        case None:
            return await w3.eth.get_block_number()
        case int() as block_number_as_int:
            return cast("BlockNumber", block_number_as_int)
        case "latest" | "earliest" | "pending" | "safe" | "finalized" as block_tag:
            block = await w3.eth.get_block(block_tag)
            block_number = block.get("number")
            if TYPE_CHECKING:
                assert block_number is not None
            return block_number
        case str() as block_number_as_str:
            try:
                return cast("BlockNumber", int(block_number_as_str, 16))
            except ValueError:
                raise DegenbotValueError(
                    message=f"Invalid block identifier {identifier!r}"
                ) from None
        case bytes() as block_number_as_bytes:
            return cast("BlockNumber", int.from_bytes(block_number_as_bytes, byteorder="big"))
        case _:
            raise DegenbotValueError(message=f"Invalid block identifier {identifier!r}")


def next_base_fee(
    parent_base_fee: int,
    parent_gas_used: int,
    parent_gas_limit: int,
    min_base_fee: int | None = None,
    base_fee_max_change_denominator: int = 8,
    elasticity_multiplier: int = 2,
) -> int:
    """
    Calculate next base fee for an EIP-1559 compatible blockchain. The
    formula is taken from the example code in the EIP-1559 proposal (ref:
    https://eips.ethereum.org/EIPS/eip-1559).

    The default values for `base_fee_max_change_denominator` and
    `elasticity_multiplier` are taken from EIP-1559.

    Enforces `min_base_fee` if provided.
    """

    last_gas_target = parent_gas_limit // elasticity_multiplier

    if parent_gas_used == last_gas_target:
        _next_base_fee = parent_base_fee
    elif parent_gas_used > last_gas_target:
        gas_used_delta = parent_gas_used - last_gas_target
        base_fee_delta = max(
            parent_base_fee * gas_used_delta // last_gas_target // base_fee_max_change_denominator,
            1,
        )
        _next_base_fee = parent_base_fee + base_fee_delta
    else:
        gas_used_delta = last_gas_target - parent_gas_used
        base_fee_delta = (
            parent_base_fee * gas_used_delta // last_gas_target // base_fee_max_change_denominator
        )
        _next_base_fee = parent_base_fee - base_fee_delta

    return max(min_base_fee, _next_base_fee) if min_base_fee else _next_base_fee


def raise_if_invalid_uint256(number: int) -> None:
    if (MIN_UINT256 <= number <= MAX_UINT256) is False:
        raise InvalidUint256


def raw_call(
    w3: Web3,
    address: ChecksumAddress,
    calldata: bytes,
    return_types: list[str],
    block_identifier: BlockIdentifier | None = None,
) -> tuple[Any, ...]:
    """
    Perform an eth_call at the given address and returns the decoded response.
    """

    return eth_abi.abi.decode(
        types=return_types,
        data=w3.eth.call(
            transaction={
                "to": address,
                "data": calldata,
            },
            block_identifier=get_number_for_block_identifier(block_identifier, w3),
        ),
    )
