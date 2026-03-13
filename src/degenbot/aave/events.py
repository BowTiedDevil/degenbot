"""Aave V3 event topic hashes."""

from enum import Enum, StrEnum, auto

from hexbytes import HexBytes


class ScaledTokenEventType(StrEnum):
    """Types of scaled token events."""

    COLLATERAL_MINT = auto()
    COLLATERAL_BURN = auto()
    COLLATERAL_TRANSFER = auto()  # AToken BalanceTransfer events with index
    ERC20_COLLATERAL_TRANSFER = auto()  # Standard ERC20 Transfer events (no index)
    DEBT_MINT = auto()
    DEBT_BURN = auto()
    DEBT_TRANSFER = auto()  # vToken BalanceTransfer events with index
    ERC20_DEBT_TRANSFER = auto()  # Standard ERC20 Transfer events for debt tokens (no index)
    GHO_DEBT_MINT = auto()
    GHO_DEBT_BURN = auto()
    GHO_DEBT_TRANSFER = auto()
    DISCOUNT_TRANSFER = auto()


class AaveV3PoolEvent(Enum):
    """Aave V3 Pool contract events."""

    SUPPLY = HexBytes("0x2b627736bca15cd5381dcf80b0bf11fd197d01a037c52b927a881a10fb73ba61")
    WITHDRAW = HexBytes("0x3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7")
    BORROW = HexBytes("0xb3d084820fb1a9decffb176436bd02558d15fac9b0ddfed8c465bc7359d7dce0")
    REPAY = HexBytes("0xa534c8dbe71f871f9f3530e97a74601fea17b426cae02e1c5aee42c96c784051")
    LIQUIDATION_CALL = HexBytes(
        "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
    )
    RESERVE_DATA_UPDATED = HexBytes(
        "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a"
    )
    RESERVE_INITIALIZED = HexBytes(
        "0x3a0ca721fc364424566385a1aa271ed508cc2c0949c2272575fb3013a163a45f"
    )
    USER_E_MODE_SET = HexBytes("0xd728da875fc88944cbf17638bcbe4af0eedaef63becd1d1c57cc097eb4608d84")
    DEFICIT_CREATED = HexBytes("0x2bccfb3fad376d59d7accf970515eb77b2f27b082c90ed0fb15583dd5a942699")


class AaveV3ScaledTokenEvent(Enum):
    """Aave V3 aToken/vToken events."""

    MINT = HexBytes("0x458f5fa412d0f69b08dd84872b0215675cc67bc1d5b6fd93300a1c3878b86196")
    BURN = HexBytes("0x4cf25bc1d991c17529c25213d3cc0cda295eeaad5f13f361969b12ea48015f90")
    BALANCE_TRANSFER = HexBytes(
        "0x4beccb90f994c31aced7a23b5611020728a23d8ec5cddd1a3e9d97b96fda8666"
    )


class AaveV3PoolConfigEvent(Enum):
    """Aave V3 pool configuration events."""

    POOL_CONFIGURATOR_UPDATED = HexBytes(
        "0x8932892569eba59c8382a089d9b732d1f49272878775235761a2a6b0309cd465"
    )
    POOL_DATA_PROVIDER_UPDATED = HexBytes(
        "0xc853974cfbf81487a14a23565917bee63f527853bcb5fa54f2ae1cdf8a38356d"
    )
    POOL_UPDATED = HexBytes("0x90affc163f1a2dfedcd36aa02ed992eeeba8100a4014f0b4cdc20ea265a66627")
    UPGRADED = HexBytes("0xbc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b")
    PROXY_CREATED = HexBytes("0x4a465a9bd819d9662563c1e11ae958f8109e437e7f4bf1c6ef0b9a7b3f35d478")
    ADDRESS_SET = HexBytes("0x9ef0e8c8e52743bb38b83b17d9429141d494b8041ca6d616a6c77cebae9cd8b7")


class AaveV3GhoDebtTokenEvent(Enum):
    """Aave V3 GHO debt token events."""

    DISCOUNT_RATE_STRATEGY_UPDATED = HexBytes(
        "0x194bd59f47b230edccccc2be58b92dde3a5dadd835751a621af59006928bccef"
    )
    DISCOUNT_TOKEN_UPDATED = HexBytes(
        "0x6b489e1dbfbe36f55c511c098bcc9d92fec7f04f74ceb75018697ab68f7d3529"
    )
    DISCOUNT_PERCENT_UPDATED = HexBytes(
        "0x74ab9665e7c36c29ddb78ef88a3e2eac73d35b8b16de7bc573e313e320104956"
    )


class AaveV3StkAaveEvent(Enum):
    """Aave V3 stkAAVE token events."""

    STAKED = HexBytes("0x6c86f3fd5118b3aa8bb4f389a617046de0a3d3d477de1a1673d227f802f616dc")
    REDEEM = HexBytes("0x3f693fff038bb8a046aa76d9516190ac7444f7d69cf952c4cbdc086fdef2d6fc")
    SLASHED = HexBytes("0x4ed05e9673c26d2ed44f7ef6a7f2942df0ee3b5e1e17db4b99f9dcd261a339cd")


class ERC20Event(Enum):
    """Standard ERC20 events."""

    TRANSFER = HexBytes("0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef")
