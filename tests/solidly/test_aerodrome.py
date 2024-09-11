import web3
from eth_utils.address import to_checksum_address

from degenbot import set_web3
from degenbot.fork.anvil_fork import AnvilFork
from degenbot.solidly.solidly_functions import generate_aerodrome_v2_pool_address
from degenbot.solidly.solidly_liquidity_pool import AerodromeV2LiquidityPool

TBTC_USDBC_POOL_ADDRESS = to_checksum_address("0x723AEf6543aecE026a15662Be4D3fb3424D502A9")
AERODROME_V2_FACTORY_ADDRESS = to_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da")
AERODROME_IMPLEMENTATION_ADDRESS = to_checksum_address("0xA4e46b4f701c62e14DF11B48dCe76A7d793CD6d7")


def test_aerodrome_v2_address_generator():
    # Should generate address for Aerodrome V2 tBTC/USDBc pool
    # factory ref: https://basescan.org/address/0x420dd381b31aef6683db6b902084cb0ffece40da
    # pool ref: https://basescan.org/address/0x723AEf6543aecE026a15662Be4D3fb3424D502A9
    assert (
        generate_aerodrome_v2_pool_address(
            deployer_address=AERODROME_V2_FACTORY_ADDRESS,
            token_addresses=[
                "0x236aa50979D5f3De3Bd1Eeb40E81137F22ab794b",
                "0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA",
            ],
            implementation_address=AERODROME_IMPLEMENTATION_ADDRESS,
            stable=False,
        )
        == TBTC_USDBC_POOL_ADDRESS
    )


def test_create_pool(
    base_full_node_web3: web3.Web3,
):
    set_web3(base_full_node_web3)

    lp = AerodromeV2LiquidityPool(
        address=TBTC_USDBC_POOL_ADDRESS,
    )
    assert lp.address == TBTC_USDBC_POOL_ADDRESS
    assert lp.factory == AERODROME_V2_FACTORY_ADDRESS
    assert lp.deployer_address == AERODROME_V2_FACTORY_ADDRESS


def test_calculation_volatile(
    fork_base: AnvilFork,
):
    set_web3(fork_base.w3)

    TOKEN0_ADDRESS = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"  # USDC
    TOKEN1_ADDRESS = "0x940181a94A35A4569E4529A3CDfB74e38FD98631"  # AERO

    lp = AerodromeV2LiquidityPool(
        address="0x6cdcb1c4a4d1c3c6d054b27ac5b77e89eafb971d",
    )
    for token_in_amount in [1 * 10**18, 5 * 10**18, 25 * 10**18]:
        contract_amount_out = lp.w3_contract.functions.getAmountOut(
            token_in_amount, TOKEN0_ADDRESS
        ).call()
        helper_amount_out = lp.calculate_tokens_out_from_tokens_in(
            token_in=lp.token0, token_in_quantity=token_in_amount
        )
        print(f"{contract_amount_out} == {helper_amount_out}?")
        assert contract_amount_out == helper_amount_out
