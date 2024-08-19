from degenbot.solidly.functions import generate_ramses_pool_address


def test_ramses_address_generation():
    assert (
        generate_ramses_pool_address(
            token_addresses=[
                "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
                "0xAAA6C1E32C55A7Bfa8066A6FAE9b42650F262418",
            ],
            deployer="0xAAA20D08e59F6561f242b08513D36266C5A29415",
            stable=False,
        )
        == "0x1E50482e9185D9DAC418768D14b2F2AC2b4DAF39"
    )

    assert (
        generate_ramses_pool_address(
            token_addresses=[
                "0x9EfCFc5b49390FC3fb9B58607D2e89445Bb380BF",
                "0xAAA6C1E32C55A7Bfa8066A6FAE9b42650F262418",
            ],
            deployer="0xAAA20D08e59F6561f242b08513D36266C5A29415",
            stable=True,
        )
        == "0xD58A9ef71e8AD9334e158835C4E5b59aC9131452"
    )
