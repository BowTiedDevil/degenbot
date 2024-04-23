import aiohttp
import eth_account
import pytest
from degenbot.builder_endpoint import BuilderEndpoint
from degenbot.fork.anvil_fork import AnvilFork
from eth_account.signers.local import LocalAccount

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

TEST_BUNDLE_HASH: str
TEST_BUNDLE_BLOCK: int

# Taken from https://privatekeys.pw/keys/ethereum/random
SIGNER_KEY = "52661f05c0512d64e2dc681900f45996e9946856ec352b7a2950203b150dbd28"


@pytest.fixture()
def beaverbuild() -> BuilderEndpoint:
    return BuilderEndpoint(
        url=BEAVERBUILD_URL,
        endpoints=["eth_sendBundle"],
    )


@pytest.fixture()
def builder0x69() -> BuilderEndpoint:
    return BuilderEndpoint(
        url=BUILDER0X69_URL,
        endpoints=["eth_sendBundle"],
        # ref: https://docs.builder0x69.io/
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.fixture()
def flashbots() -> BuilderEndpoint:
    return BuilderEndpoint(
        url=FLASHBOTS_URL,
        endpoints=[
            "eth_callBundle",
            "eth_sendBundle",
            "flashbots_getUserStatsV2",
            "flashbots_getBundleStatsV2",
        ],
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.fixture()
def rsyncbuilder() -> BuilderEndpoint:
    return BuilderEndpoint(
        url=RSYNCBUILDER_URL,
        endpoints=[
            "eth_cancelBundle",
            "eth_sendBundle",
            "eth_sendPrivateRawTransaction",
        ],
        authentication_header_label="X-Flashbots-Signature",
    )


@pytest.fixture()
def titanbuilder() -> BuilderEndpoint:
    return BuilderEndpoint(
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
    assert isinstance(builder, BuilderEndpoint)


async def test_bad_url():
    with pytest.raises(ValueError):
        BuilderEndpoint(url="ws://www.google.com", endpoints=["eth_sendBundle"])


async def test_blank_eth_send_bundle(
    beaverbuild: BuilderEndpoint,
    fork_mainnet: AnvilFork,
):
    current_block = fork_mainnet.w3.eth.block_number
    response = await beaverbuild.send_eth_bundle(
        bundle=[],
        block_number=current_block + 1,
    )
    assert isinstance(response, dict)
    assert "bundleHash" in response


async def test_blank_eth_send_bundle_with_session(
    beaverbuild: BuilderEndpoint,
    fork_mainnet: AnvilFork,
):
    async with aiohttp.ClientSession(raise_for_status=True) as session:
        current_block = fork_mainnet.w3.eth.block_number
        response = await beaverbuild.send_eth_bundle(
            bundle=[], block_number=current_block + 1, http_session=session
        )
        assert isinstance(response, dict)
        assert "bundleHash" in response


async def test_eth_call_bundle(
    flashbots: BuilderEndpoint,
    fork_mainnet: AnvilFork,
):
    current_block = fork_mainnet.w3.eth.block_number
    current_base_fee = fork_mainnet.w3.eth.get_block("latest")["baseFeePerGas"]
    current_block_timestamp = fork_mainnet.w3.eth.get_block("latest")["timestamp"]

    signer: LocalAccount = eth_account.Account.from_key(SIGNER_KEY)
    SIGNER_ADDRESS = signer.address

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
        signer_key=SIGNER_KEY,
    )
    assert isinstance(response, dict)

    # Test with "latest" state block alias
    response = await flashbots.call_eth_bundle(
        bundle=[signed_tx_1, signed_tx_2],
        block_number=current_block + 1,
        state_block="latest",
        signer_key=SIGNER_KEY,
    )
    assert isinstance(response, dict)

    # Test with timestamp
    response = await flashbots.call_eth_bundle(
        bundle=[signed_tx_1, signed_tx_2],
        block_number=current_block + 1,
        block_timestamp=current_block_timestamp + 12,
        state_block=current_block,
        signer_key=SIGNER_KEY,
    )
    assert isinstance(response, dict)


async def test_eth_send_bundle(
    flashbots: BuilderEndpoint,
    fork_mainnet: AnvilFork,
):
    current_block = fork_mainnet.w3.eth.block_number
    current_base_fee = fork_mainnet.w3.eth.get_block("latest")["baseFeePerGas"]

    signer: LocalAccount = eth_account.Account.from_key(SIGNER_KEY)
    SIGNER_ADDRESS = signer.address

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

    response = await flashbots.send_eth_bundle(
        bundle=[signed_tx_1, signed_tx_2],
        block_number=current_block + 1,
        signer_key=SIGNER_KEY,
    )
    assert isinstance(response, dict)

    global TEST_BUNDLE_HASH
    global TEST_BUNDLE_BLOCK
    TEST_BUNDLE_HASH = response["bundleHash"]
    TEST_BUNDLE_BLOCK = current_block + 1


async def test_get_user_stats(
    flashbots: BuilderEndpoint,
    fork_mainnet: AnvilFork,
):
    current_block = fork_mainnet.w3.eth.block_number
    await flashbots.get_user_stats(
        signer_key=SIGNER_KEY,
        block_number=current_block,
    )


async def test_get_bundle_stats(flashbots: BuilderEndpoint):
    await flashbots.get_bundle_stats(
        bundle_hash=TEST_BUNDLE_HASH,
        block_number=TEST_BUNDLE_BLOCK,
        signer_key=SIGNER_KEY,
    )
