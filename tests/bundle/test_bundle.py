import degenbot.bundle
import degenbot.fork
import pytest
from degenbot.bundle.block_builder import BlockBuilder

BEAVERBUILD_URL = "https://rpc.beaverbuild.org"
BUILDER0X69_URL = "https://builder0x69.io"
FLASHBOTS_URL = "https://relay.flashbots.net"
RSYNCBUILDER_URL = "https://rsync-builder.xyz"
TITANBUILDER_URL = "https://rpc.titanbuilder.xyz"


BUILDER_FIXTURES = [
    "beaverbuild",
    "builder0x69",
    "flashbots",
    "rsyncbuilder",
    "titanbuilder",
]

# Taken from https://privatekeys.pw/keys/ethereum/random
SIGNER_ADDRESS = "0x4796921B22704f52ea7e015600a6233B60C6bc25"
SIGNER_KEY = "52661f05c0512d64e2dc681900f45996e9946856ec352b7a2950203b150dbd28"


@pytest.fixture()
def beaverbuild() -> BlockBuilder:
    return BlockBuilder(
        url=BEAVERBUILD_URL,
        endpoints=["eth_sendBundle"],
    )


@pytest.fixture()
def builder0x69() -> BlockBuilder:
    return BlockBuilder(
        url=BUILDER0X69_URL,
        endpoints=["eth_sendBundle"],
        # ref: https://docs.builder0x69.io/
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.fixture()
def flashbots() -> BlockBuilder:
    return BlockBuilder(
        url=FLASHBOTS_URL,
        endpoints=[
            "eth_callBundle",
            "eth_sendBundle",
        ],
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.fixture()
def rsyncbuilder() -> BlockBuilder:
    return BlockBuilder(
        url=RSYNCBUILDER_URL,
        endpoints=[
            "eth_cancelBundle",
            "eth_sendBundle",
            "eth_sendPrivateRawTransaction",
        ],
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.fixture()
def titanbuilder() -> BlockBuilder:
    return BlockBuilder(
        url=TITANBUILDER_URL,
        endpoints=["eth_sendBundle"],
        # ref: https://docs.titanbuilder.xyz/authentication
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.mark.parametrize(
    "builder_name",
    BUILDER_FIXTURES,
)
def test_create_builders(builder_name: str, request: pytest.FixtureRequest):
    builder = request.getfixturevalue(builder_name)
    assert isinstance(builder, BlockBuilder)


async def test_blank_eth_send_bundle(
    beaverbuild: BlockBuilder,
    fork_mainnet: degenbot.fork.AnvilFork,
):
    current_block = fork_mainnet.w3.eth.block_number
    response = await beaverbuild.send_eth_bundle(
        bundle=[],
        block_number=current_block + 1,
    )
    assert isinstance(response, dict)
    assert "bundleHash" in response


async def test_eth_call_bundle(
    flashbots: BlockBuilder,
    fork_mainnet: degenbot.fork.AnvilFork,
):
    current_block = fork_mainnet.w3.eth.block_number
    current_base_fee = fork_mainnet.w3.eth.get_block("latest")["baseFeePerGas"]

    transaction_1 = {
        "chainId": 1,
        "data": b"",
        "from": SIGNER_ADDRESS,
        "to": SIGNER_ADDRESS,
        "value": 1,
        "nonce": fork_mainnet.w3.eth.get_transaction_count(SIGNER_ADDRESS),
        "gas": 50_000,
        "maxFeePerGas": int(1.5 * current_base_fee),
        "maxPriorityFeePerGas": 0,
    }
    transaction_2 = {
        "chainId": 1,
        "data": b"",
        "from": SIGNER_ADDRESS,
        "to": SIGNER_ADDRESS,
        "value": 1,
        "nonce": fork_mainnet.w3.eth.get_transaction_count(SIGNER_ADDRESS) + 1,
        "gas": 50_000,
        "maxFeePerGas": int(1.5 * current_base_fee),
        "maxPriorityFeePerGas": 0,
    }
    signed_tx_1 = fork_mainnet.w3.eth.account.sign_transaction(
        transaction_1, SIGNER_KEY
    ).rawTransaction
    signed_tx_2 = fork_mainnet.w3.eth.account.sign_transaction(
        transaction_2, SIGNER_KEY
    ).rawTransaction

    response = await flashbots.call_eth_bundle(
        bundle=[signed_tx_1, signed_tx_2],
        block_number=current_block + 1,
        state_block=current_block,
        signer_address=SIGNER_ADDRESS,
        signer_key=SIGNER_KEY,
    )
    assert isinstance(response, dict)
