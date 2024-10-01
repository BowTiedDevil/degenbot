from eth_typing import ChainId, ChecksumAddress
from eth_utils.address import to_checksum_address

from .types import SolidlyExchangeDeployment, SolidlyFactoryDeployment

# Base DEX --------------- START
BaseMainnetAerodromeV2 = SolidlyExchangeDeployment(
    name="Base Mainnet Aerodrome V2",
    chain_id=ChainId.ETH,
    factory=SolidlyFactoryDeployment(
        address=to_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da"),
        deployer=None,
        pool_init_hash="0xa29c9b69e1a80d2352a264163a9012501d60dbd7cf552a87e681c62e81af9937",
    ),
)


FACTORY_DEPLOYMENTS: dict[
    int,  # chain ID
    dict[ChecksumAddress, SolidlyFactoryDeployment],
] = {
    ChainId.BASE: {
        BaseMainnetAerodromeV2.factory.address: BaseMainnetAerodromeV2.factory,
    },
}
