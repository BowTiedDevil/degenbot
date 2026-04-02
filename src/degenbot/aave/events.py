"""Aave V3 event topic hashes."""

from enum import Enum, auto

from hexbytes import HexBytes


class ScaledTokenEventType(Enum):
    """
    Types of scaled token events.
    """

    # Balance modifying event types
    COLLATERAL_BURN = auto()
    COLLATERAL_MINT = auto()
    COLLATERAL_TRANSFER = auto()  # AToken BalanceTransfer events with index
    DEBT_BURN = auto()
    DEBT_MINT = auto()
    DEBT_TRANSFER = auto()  # vToken BalanceTransfer events with index
    DISCOUNT_TRANSFER = auto()
    GHO_DEBT_BURN = auto()
    GHO_DEBT_MINT = auto()
    GHO_DEBT_TRANSFER = auto()

    # ERC20 transfer events associated with collateral (aToken) and debt (VariableDebtToken)
    ERC20_COLLATERAL_TRANSFER = auto()
    ERC20_DEBT_TRANSFER = auto()

    # Interest accrual event types (derived during enrichment)
    COLLATERAL_INTEREST_BURN = auto()
    COLLATERAL_INTEREST_MINT = auto()
    DEBT_INTEREST_BURN = auto()
    DEBT_INTEREST_MINT = auto()
    GHO_DEBT_INTEREST_BURN = auto()
    GHO_DEBT_INTEREST_MINT = auto()


class AaveV3PoolEvent(Enum):
    """
    Aave V3 Pool contract events.
    """

    BORROW = HexBytes("0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0")
    DEFICIT_CREATED = HexBytes("0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699")
    LIQUIDATION_CALL = HexBytes(
        "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
    )
    MINTED_TO_TREASURY = HexBytes(
        "0xbfa21aa5d5f9a1f0120a95e7c0749f389863cbdbfff531aa7339077a5bc919de"
    )
    REPAY = HexBytes("0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")
    RESERVE_DATA_UPDATED = HexBytes(
        "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a"
    )
    RESERVE_USED_AS_COLLATERAL_DISABLED = HexBytes(
        "0x44c58d81365b66dd4b1a7f36c25aa97b8c71c361ee4937adc1a00000227db5dd"
    )
    RESERVE_USED_AS_COLLATERAL_ENABLED = HexBytes(
        "0x00058a56ea94653cdf4f152d227ace22d4c00ad99e2a43f58cb7d9e3feb295f2"
    )
    SUPPLY = HexBytes("0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61")
    USER_E_MODE_SET = HexBytes("0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84")
    WITHDRAW = HexBytes("0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7")


class AaveV3ScaledTokenEvent(Enum):
    """
    Aave V3 aToken/vToken events.
    """

    BALANCE_TRANSFER = HexBytes(
        "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666"
    )
    BURN = HexBytes("0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90")
    MINT = HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196")


class AaveV3PoolConfigEvent(Enum):
    """
    Aave V3 pool configuration events.
    """

    ADDRESS_SET = HexBytes("0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7")
    ASSET_COLLATERAL_IN_EMODE_CHANGED = HexBytes(
        "0x79409190108b26fcb0e4570f8e240f627bf18fd01a55f751010224d5bd486098"
    )
    COLLATERAL_CONFIGURATION_CHANGED = HexBytes(
        "0x637febbda9275aea2e85c0ff690444c8d87eb2e8339bbede9715abcc89cb0995"
    )
    EMODE_ASSET_CATEGORY_CHANGED = HexBytes(
        "0x5bb69795b6a2ea222d73a5f8939c23471a1f85a99c7ca43c207f1b71f10c6264"
    )
    EMODE_CATEGORY_ADDED = HexBytes(
        "0x0acf8b4a3cace10779798a89a206a0ae73a71b63acdd3be2801d39c2ef7ab3cb"
    )
    POOL_CONFIGURATOR_UPDATED = HexBytes(
        "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465"
    )
    POOL_DATA_PROVIDER_UPDATED = HexBytes(
        "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d"
    )
    POOL_UPDATED = HexBytes("0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627")
    PRICE_ORACLE_UPDATED = HexBytes(
        "0x56b5f80d8cac1479698aa7d01605fd6111e90b15fc4d2b377417f46034876cbd"
    )
    PROXY_CREATED = HexBytes("0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478")
    RESERVE_INITIALIZED = HexBytes(
        "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f"
    )
    UPGRADED = HexBytes("0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b")


class AaveV3GhoDebtTokenEvent(Enum):
    """
    Aave V3 GHO debt token events.
    """

    DISCOUNT_PERCENT_UPDATED = HexBytes(
        "0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956"
    )
    DISCOUNT_RATE_STRATEGY_UPDATED = HexBytes(
        "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef"
    )
    DISCOUNT_TOKEN_UPDATED = HexBytes(
        "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529"
    )


class AaveV3StkAaveEvent(Enum):
    """
    Aave V3 stkAAVE token events.
    """

    REDEEM = HexBytes("0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc")
    STAKED = HexBytes("0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc")


class AaveV3RewardsControllerEvent(Enum):
    """
    Aave V3 RewardsController events.
    """

    REWARDS_CLAIMED = HexBytes("0xc052130bc4ef84580db505783484b067ea8b71b3bca78a7e12db7aea8658f004")


class AaveV3OracleEvent(Enum):
    """
    Aave V3 Oracle contract events.

    These events are emitted by the AaveOracle contract when its
    configuration changes.
    """

    ASSET_SOURCE_UPDATED = HexBytes(
        "0x22c5b7b2d8561d39f7f210b6b326a1aa69f15311163082308ac4877db6339dc1"
    )


class ERC20Event(Enum):
    """
    Standard ERC20 events.
    """

    TRANSFER = HexBytes("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
