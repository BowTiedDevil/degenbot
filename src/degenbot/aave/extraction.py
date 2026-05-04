"""Raw amount extraction from Pool events."""

from collections.abc import Callable

import eth_abi.abi
from web3.types import LogReceipt

from degenbot.aave.events import AaveV3PoolEvent
from degenbot.aave.models import EnrichmentError

EVENT_EXTRACTORS: dict[AaveV3PoolEvent, Callable[[LogReceipt], int]] = {}


class RawAmountExtractor:
    """
    Extracts raw amounts from Pool events.

    Each Pool event type has specific data encoding. This class provides
    type-safe extraction of the raw amount (the input to TokenMath).
    """

    def __init__(self, pool_event: LogReceipt, pool_revision: int) -> None:
        self.pool_event = pool_event
        self.pool_revision = pool_revision
        self._event_type: AaveV3PoolEvent = self._get_event_type()

    def extract(self) -> int:
        """
        Extract raw amount from the Pool event.
        """

        extractor = self._get_extractor()
        return extractor(self.pool_event)

    def _get_event_type(self) -> AaveV3PoolEvent:
        """
        Determine Pool event type from topics.
        """

        topic0 = self.pool_event["topics"][0]
        # Map topic to event type
        for event in AaveV3PoolEvent:
            if event.value == topic0:
                return event
        msg = f"Unknown Pool event topic: {topic0.hex()}"
        raise EnrichmentError(msg)

    def _get_extractor(self) -> Callable[[LogReceipt], int]:
        """
        Get extraction function for this event type.
        """

        extractor = EVENT_EXTRACTORS.get(self._event_type)
        if extractor is None:
            msg = f"No extractor for Pool event type: {self._event_type.name}"
            raise EnrichmentError(msg)
        return extractor

    @staticmethod
    def extract_supply(event: LogReceipt) -> int:
        """
        Extract amount from Supply event.

        Event definition:
            event Supply(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                uint16 indexed referralCode
            );
        """

        supply_amount: int
        (_, supply_amount) = eth_abi.abi.decode(
            types=["address", "uint256"],
            data=event["data"],
        )
        return supply_amount

    @staticmethod
    def extract_borrow(event: LogReceipt) -> int:
        """
        Extract amount from Borrow event.

        Event definition:
            event Borrow(
                address indexed reserve,
                address user,
                address indexed onBehalfOf,
                uint256 amount,
                DataTypes.InterestRateMode interestRateMode,
                uint256 borrowRate,
                uint16 indexed referralCode
            );
        """

        borrow_amount: int
        (_, borrow_amount, _, _) = eth_abi.abi.decode(
            types=["address", "uint256", "uint8", "uint256"],
            data=event["data"],
        )
        return borrow_amount

    @staticmethod
    def extract_repay(event: LogReceipt) -> int:
        """
        Extract amount from Repay event.

        Event definition:
            event Repay(
                address indexed reserve,
                address indexed user,
                address indexed repayer,
                uint256 amount,
                bool useATokens
            );
        """

        repay_amount: int
        repay_amount, _ = eth_abi.abi.decode(
            types=["uint256", "bool"],
            data=event["data"],
        )
        return repay_amount

    @staticmethod
    def extract_withdraw(event: LogReceipt) -> int:
        """
        Extract amount from Withdraw event.

        Event definition:
            event Withdraw(
                address indexed reserve,
                address indexed user,
                address indexed to,
                uint256 amount
            );
        """

        withdraw_amount: int
        (withdraw_amount,) = eth_abi.abi.decode(
            types=["uint256"],
            data=event["data"],
        )
        return withdraw_amount

    @staticmethod
    def extract_liquidation_debt(event: LogReceipt) -> int:
        """
        Extract debt amount from LiquidationCall event.

        Event definition:
            event LiquidationCall(
                address indexed collateralAsset,
                address indexed debtAsset,
                address indexed user,
                uint256 debtToCover,
                uint256 liquidatedCollateralAmount,
                address liquidator,
                bool receiveAToken
            );
        """

        debt_to_cover: int
        debt_to_cover, _, _, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=event["data"],
        )
        return debt_to_cover

    @staticmethod
    def extract_liquidation_collateral(event: LogReceipt) -> int:
        """
        Extract collateral amount from LiquidationCall event.

        Event definition:
            event LiquidationCall(
                address indexed collateralAsset,
                address indexed debtAsset,
                address indexed user,
                uint256 debtToCover,
                uint256 liquidatedCollateralAmount,
                address liquidator,
                bool receiveAToken
            );
        """

        liquidated_collateral: int
        _, liquidated_collateral, _, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "address", "bool"],
            data=event["data"],
        )
        return liquidated_collateral


# Register extractors for each Pool event type
EVENT_EXTRACTORS[AaveV3PoolEvent.SUPPLY] = RawAmountExtractor.extract_supply
EVENT_EXTRACTORS[AaveV3PoolEvent.WITHDRAW] = RawAmountExtractor.extract_withdraw
EVENT_EXTRACTORS[AaveV3PoolEvent.BORROW] = RawAmountExtractor.extract_borrow
EVENT_EXTRACTORS[AaveV3PoolEvent.REPAY] = RawAmountExtractor.extract_repay
# Liquidation uses different extractors for debt vs collateral - handled in extract method
