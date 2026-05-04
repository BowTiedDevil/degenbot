"""Type definitions for Aave V3 CLI processing."""

from dataclasses import dataclass, field
from enum import Enum, auto
from typing import Any

from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy.orm import Session
from web3.types import LogReceipt

from degenbot.aave.pattern_types import LiquidationPatternContext
from degenbot.database.models.aave import (
    AaveGhoToken,
    AaveV3CollateralPosition,
    AaveV3DebtPosition,
    AaveV3Market,
)
from degenbot.provider.interface import ProviderAdapter


class TokenType(Enum):
    """Token types in Aave V3."""

    A_TOKEN = auto()
    V_TOKEN = auto()
    GHO_DISCOUNT = auto()


@dataclass
class TransactionContext:
    """Context for processing a single transaction as a sequence of events."""

    provider: ProviderAdapter
    tx_hash: HexBytes
    block_number: int
    events: list[LogReceipt]
    market: AaveV3Market
    session: Session
    gho_asset: AaveGhoToken

    # Snapshot of user discount percents at the start of transaction processing
    # Key: user address, Value: discount percent at transaction start
    user_discounts: dict[ChecksumAddress, int] = field(default_factory=dict)

    # Track discount percent updates by log index for transactions with multiple updates.
    # Key: user address, Value: list of (log_index, old_discount_percent) tuples sorted by
    # log_index. This allows determining the discount in effect at any point in the transaction.
    discount_updates_by_log_index: dict[ChecksumAddress, list[tuple[int, int]]] = field(
        default_factory=dict
    )

    # Set of user addresses with stkAAVE Transfer events in this transaction
    # Used to skip balance initialization when events provide authoritative values
    stk_aave_transfer_users: set[ChecksumAddress] = field(default_factory=set)

    # Track which stkAAVE transfer events have been processed (by log index)
    # This prevents double-counting in pending delta calculations
    processed_stk_aave_transfers: set[int] = field(default_factory=set)

    # Track modified positions to ensure we use the same object across operations.
    # Key: (user_address, asset_id, position_table_class), Value: position object
    # Using table class as discriminator to distinguish collateral vs debt positions
    # for the same user and asset (e.g., user supplying and borrowing USDC)
    # Using Any for the table class type to satisfy mypy with generic parameters
    modified_positions: dict[
        tuple[ChecksumAddress, int, Any],
        AaveV3CollateralPosition | AaveV3DebtPosition,
    ] = field(default_factory=dict)

    # Store the most recent WITHDRAW amount for accurate interest accrual calculations.
    # When a collateral burn occurs as part of interest accrual during a withdraw, we need the
    # original withdrawAmount (not the reverse-calculated value from the Burn event) to
    # avoid 1 wei rounding errors from rayDivCeil/rayMulFloor asymmetry.
    last_withdraw_amount: int = 0
    last_withdraw_token_address: ChecksumAddress | None = None
    last_withdraw_user_address: ChecksumAddress | None = None

    # Pool revision for TokenMath calculations
    # Used to determine if scaled amounts need to be pre-calculated (rev 9+)
    pool_revision: int = 0

    # Scaled token events for pattern detection
    # Set during transaction operations parsing
    scaled_token_events: list[Any] = field(default_factory=list)
    """List of scaled token events for this transaction."""

    # Pattern-aware liquidation context for multi-liquidation scenarios
    # Replaces liquidation_aggregates, liquidation_counts, and processed_liquidations
    # See debug/aave/0056 and debug/aave/0065 for pattern details.
    liquidation_patterns: LiquidationPatternContext = field(
        default_factory=LiquidationPatternContext
    )
    """Pattern detection and processing state for liquidations."""

    @property
    def gho_vtoken_address(self) -> ChecksumAddress | None:
        """Get the GHO vToken address if GHO asset exists."""
        if self.gho_asset is None or self.gho_asset.v_token is None:
            return None
        return self.gho_asset.v_token.address

    def is_gho_vtoken(self, token_address: ChecksumAddress) -> bool:
        """Check if the given token address is the GHO vToken."""
        gho_addr = self.gho_vtoken_address
        return gho_addr is not None and token_address == gho_addr

    def get_effective_discount_at_log_index(
        self,
        user_address: ChecksumAddress,
        log_index: int,
        default_discount: int,
    ) -> int:
        """
        Get the discount percent in effect at a specific log index.

        When a user has multiple DiscountPercentUpdated events in a transaction,
        each Mint/Burn event must use the discount that was in effect at that
        specific point in time (before any subsequent discount updates).

        Args:
            user_address: The user's address
            log_index: The log index to check
            default_discount: The fallback discount if no updates occurred before this log_index

        Returns:
            The discount percent in effect at the given log index
        """
        updates = self.discount_updates_by_log_index.get(user_address, [])
        if not updates:
            return default_discount

        # Find the most recent discount update before this log_index
        # updates is a list of (log_index, old_discount_percent) tuples sorted by log_index
        effective_discount = default_discount
        for update_log_index, old_discount in updates:
            if update_log_index < log_index:
                effective_discount = old_discount
            else:
                break

        return effective_discount
