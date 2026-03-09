"""Structured JSON logging for Aave updater debugging.

Provides machine-parseable debug output for autonomous agent analysis.
"""

import contextlib
import json
import os
import sys
import traceback
from collections.abc import Sequence
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, ClassVar, Self

import eth_abi.abi
from eth_typing import ChainId
from hexbytes import HexBytes
from web3.types import LogReceipt

from degenbot.aave.events import (
    AaveV3GhoDebtTokenEvent,
    AaveV3PoolEvent,
    AaveV3ScaledTokenEvent,
    ERC20Event,
)
from degenbot.cli.aave_types import TransactionContext


class AaveDebugLogger:
    """Structured debug logger for Aave event processing.

    Outputs JSON Lines format for machine-parseable debugging.
    Each log entry includes timestamp, level, context, and structured data.
    """

    _instance: ClassVar[Self]

    def __new__(cls) -> Self:
        with contextlib.suppress(AttributeError):
            return cls._instance

        cls._instance = super().__new__(cls)
        return cls._instance

    def __init__(self) -> None:
        self._output_path: Path | None = None
        self._file_handle: Any = None
        self._chain_id: ChainId | None = None
        self._market_id: int | None = None
        self._buffer: list[dict[str, Any]] = []
        self._buffer_size: int = 100
        self._enabled: bool = False

    def configure(
        self,
        output_path: Path | str | None = None,
        chain_id: ChainId | None = None,
        market_id: int | None = None,
    ) -> bool:
        """Configure the debug logger.

        Args:
            output_path: Path to write JSONL debug output. If None, uses env var
                or existing path if already configured.
            chain_id: Chain ID for context
            market_id: Market ID for context

        Returns:
            True if logging is enabled, False otherwise
        """
        if output_path is None:
            output_path = os.environ.get("DEGENBOT_DEBUG_OUTPUT")

        # If already configured with a path, just update context
        if self._output_path is not None and output_path is None:
            if chain_id is not None:
                self._chain_id = chain_id
            if market_id is not None:
                self._market_id = market_id
            return self._enabled

        if not output_path:
            self._enabled = False
            return False

        # Close existing file if reconfiguring with new path
        if self._file_handle is not None:
            self.close()

        self._output_path = Path(output_path)
        self._chain_id = chain_id
        self._market_id = market_id
        self._enabled = True

        # Ensure parent directory exists
        self._output_path.parent.mkdir(parents=True, exist_ok=True)

        # Open file for writing (append mode)
        self._file_handle = self._output_path.open("a", buffering=1, encoding="utf-8")

        # Write header entry
        self._write_entry({
            "type": "session_start",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "chain_id": chain_id.value if chain_id else None,
            "market_id": market_id,
        })

        return True

    def is_enabled(self) -> bool:
        """Check if debug logging is enabled."""
        return self._enabled

    def _write_entry(self, entry: dict[str, Any]) -> None:
        """Write a single log entry to the file."""
        if not self._enabled or self._file_handle is None:
            return

        entry["_chain_id"] = self._chain_id.value if self._chain_id else None
        entry["_market_id"] = self._market_id

        try:
            self._file_handle.write(json.dumps(entry, default=str) + "\n")
        except OSError as e:
            # Log to stderr if file write fails
            sys.stderr.write(f"Failed to write debug log: {e}\n")

    def log_event(
        self,
        *,
        level: str,
        message: str,
        tx_hash: HexBytes | str | None = None,
        block_number: int | None = None,
        user_address: str | None = None,
        user_addresses: list[str] | None = None,
        event_type: str | None = None,
        event_data: dict[str, Any] | None = None,
        context: dict[str, Any] | None = None,
    ) -> None:
        """Log a structured event.

        Args:
            level: Log level (DEBUG, INFO, WARNING, ERROR, CRITICAL)
            message: Human-readable message
            tx_hash: Transaction hash for correlation
            block_number: Block number for correlation
            user_address: User address for correlation
            user_addresses: List of user addresses for correlation
            event_type: Type of event being processed
            event_data: Structured event data
            context: Additional structured context data
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "log",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "level": level.upper(),
            "message": message,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
            "block_number": block_number,
            "user_address": user_address,
            "event_type": event_type,
            "context": context or {},
        }

        if user_addresses is not None:
            entry["user_addresses"] = sorted(user_addresses)

        if event_data is not None:
            entry["event_data"] = event_data

        self._write_entry(entry)

    def log_transaction_start(
        self,
        *,
        tx_hash: HexBytes | str,
        block_number: int,
        event_count: int,
        context: TransactionContext | None = None,
    ) -> None:
        """Log the start of transaction processing.

        Args:
            tx_hash: Transaction hash
            block_number: Block number
            event_count: Number of events in transaction
            context: Transaction context for detailed logging
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "transaction_start",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
            "block_number": block_number,
            "event_count": event_count,
        }

        if context is not None:
            entry["tx_context"] = self._serialize_tx_context(context)

        self._write_entry(entry)

    def log_transaction_end(
        self,
        *,
        tx_hash: HexBytes | str,
        block_number: int,
        success: bool,
        duration_ms: float | None = None,
    ) -> None:
        """Log the end of transaction processing.

        Args:
            tx_hash: Transaction hash
            block_number: Block number
            success: Whether processing succeeded
            duration_ms: Processing duration in milliseconds
        """
        if not self._enabled:
            return

        entry = {
            "type": "transaction_end",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
            "block_number": block_number,
            "success": success,
            "duration_ms": duration_ms,
        }

        self._write_entry(entry)

    def log_exception(
        self,
        *,
        exc: Exception,
        tx_context: TransactionContext | None = None,
        event: LogReceipt | None = None,
        extra_context: dict[str, Any] | None = None,
    ) -> None:
        """Log an exception with full context for replay.

        Args:
            exc: The exception that was raised
            tx_context: Transaction context at time of exception
            event: The event being processed when exception occurred
            extra_context: Additional context data
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "exception",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "exception_type": type(exc).__name__,
            "exception_message": str(exc),
            "traceback": traceback.format_exc(),
        }

        if tx_context is not None:
            entry["tx_context"] = self._serialize_tx_context(tx_context)

        if event is not None:
            entry["event"] = self._serialize_event(event)

        if extra_context is not None:
            entry["extra_context"] = extra_context

        self._write_entry(entry)

    @staticmethod
    def _serialize_tx_context(context: TransactionContext) -> dict[str, Any]:
        """Serialize TransactionContext to a JSON-serializable dict."""
        event_topics: list[str] = []
        for event in context.events:
            topics = event.get("topics", [])
            if topics:
                first_topic = topics[0]
                if isinstance(first_topic, HexBytes):
                    event_topics.append(first_topic.hex())
                else:
                    event_topics.append(str(first_topic))

        # Extract all user addresses from transaction events
        user_addresses: set[str] = set()
        for event in context.events:
            topics = event.get("topics", [])
            if not topics:
                continue

            topic = topics[0]

            if topic == AaveV3ScaledTokenEvent.MINT.value:
                """
                Event definition:
                    event Mint(
                        address indexed caller,
                        address indexed onBehalfOf,
                        uint256 value,
                        uint256 balanceIncrease,
                        uint256 index
                    );
                """
                assert len(topics) == 3  # noqa:PLR2004
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
                user_addr_old = "0x" + topics[2].hex()[-40:]
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3ScaledTokenEvent.BURN.value:
                """
                Event definition:
                    event Burn(
                        address indexed from,
                        address indexed target,
                        uint256 value,
                        uint256 balanceIncrease,
                        uint256 index
                    );
                """
                assert len(topics) == 3  # noqa:PLR2004
                user_addr_old = "0x" + topics[1].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3ScaledTokenEvent.BALANCE_TRANSFER.value:
                """
                Event definition:
                    event BalanceTransfer(
                        address indexed from,
                        address indexed to,
                        uint256 value,
                        uint256 index
                    );
                """
                assert len(topics) == 3  # noqa:PLR2004
                from_addr_old = "0x" + topics[1].hex()[-40:]
                (from_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
                to_addr_old = "0x" + topics[2].hex()[-40:]
                (to_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
                assert from_addr_old == from_addr
                assert to_addr_old == to_addr
                user_addresses.add(from_addr)
                user_addresses.add(to_addr)

            elif topic == AaveV3PoolEvent.DEFICIT_CREATED.value:
                """
                Event definition:
                    event DeficitCreated(
                        address indexed user,
                        address indexed debtAsset,
                        uint256 amountCreated
                    );
                """
                assert len(topics) == 3  # noqa:PLR2004
                user_addr_old = "0x" + topics[1].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3PoolEvent.BORROW.value:
                """
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
                assert len(topics) == 4  # noqa:PLR2004
                user_addr_old = "0x" + topics[2].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3PoolEvent.REPAY.value:
                """
                Event definition:
                    event Repay(
                        address indexed reserve,
                        address indexed user,
                        address indexed repayer,
                        uint256 amount,
                        bool useATokens
                    );
                """
                assert len(topics) == 4  # noqa:PLR2004
                user_addr_old = "0x" + topics[2].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3PoolEvent.SUPPLY.value:
                """
                Event definition:
                    event Supply(
                        address indexed reserve,
                        address user,
                        address indexed onBehalfOf,
                        uint256 amount,
                        uint16 indexed referralCode
                    );
                """
                assert len(topics) == 4  # noqa:PLR2004
                user_addr_old = "0x" + topics[2].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3PoolEvent.WITHDRAW.value:
                """
                Event definition:
                    event Withdraw(
                        address indexed reserve,
                        address indexed user,
                        address indexed to,
                        uint256 amount
                    );
                """
                assert len(topics) == 4  # noqa:PLR2004
                user_addr_old = "0x" + topics[2].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][2])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3PoolEvent.LIQUIDATION_CALL.value:
                """
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
                assert len(topics) == 4  # noqa:PLR2004
                user_addr_old = "0x" + topics[3].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][3])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3GhoDebtTokenEvent.DISCOUNT_PERCENT_UPDATED.value:
                """
                Event definition:
                    event DiscountPercentUpdated(
                        address indexed user,
                        uint256 oldDiscountPercent,
                        uint256 indexed newDiscountPercent
                    );
                """
                assert len(topics) == 2  # noqa:PLR2004
                user_addr_old = "0x" + topics[1].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic == AaveV3PoolEvent.USER_E_MODE_SET.value:
                """
                Event definition:
                    event UserEModeSet(
                        address indexed user,
                        uint8 categoryId
                    );
                """
                assert len(topics) == 2  # noqa:PLR2004
                user_addr_old = "0x" + topics[1].hex()[-40:]
                (user_addr,) = eth_abi.abi.decode(types=["address"], data=event["topics"][1])
                assert user_addr_old == user_addr
                user_addresses.add(user_addr)

            elif topic in {
                AaveV3PoolEvent.RESERVE_DATA_UPDATED.value,
                ERC20Event.TRANSFER.value,
            }:
                pass
            else:
                msg = f"UNKNOWN EVENT: {topic.to_0x_hex()}"
                raise ValueError(msg)

        return {
            "tx_hash": context.tx_hash.hex()
            if isinstance(context.tx_hash, HexBytes)
            else str(context.tx_hash),
            "block_number": context.block_number,
            "event_count": len(context.events),
            "event_topics": event_topics,
            "user_discounts_count": len(context.user_discounts),
            "discount_updates_count": len(context.discount_updates_by_log_index),
            "user_addresses": sorted(user_addresses),
        }

    @staticmethod
    def _serialize_event(event: LogReceipt) -> dict[str, Any]:
        """Serialize a LogReceipt event to JSON-serializable dict."""
        if event is None:
            return {}

        return {
            "address": event.get("address"),
            "blockNumber": event.get("blockNumber"),
            "blockHash": event.get("blockHash").hex()
            if isinstance(event.get("blockHash"), HexBytes)
            else event.get("blockHash"),
            "transactionHash": event.get("transactionHash").hex()
            if isinstance(event.get("transactionHash"), HexBytes)
            else event.get("transactionHash"),
            "logIndex": event.get("logIndex"),
            "topics": [
                t.hex() if isinstance(t, HexBytes) else str(t) for t in event.get("topics", [])
            ],
            "data": event.get("data").hex()
            if isinstance(event.get("data"), (HexBytes, bytes))
            else event.get("data"),
        }

    def log_block_boundary(
        self,
        *,
        block_number: int,
        event_count: int,
        user_count: int,
        user_addresses: list[str] | None = None,
    ) -> None:
        """Log block boundary processing.

        Args:
            block_number: Block number
            event_count: Number of events in block
            user_count: Number of users affected in block
            user_addresses: List of user addresses affected in block
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "block_boundary",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "block_number": block_number,
            "event_count": event_count,
            "user_count": user_count,
        }

        if user_addresses is not None:
            entry["user_addresses"] = sorted(user_addresses)

        self._write_entry(entry)

    def log_user_creation(
        self,
        *,
        user_address: str,
        block_number: int,
        tx_hash: HexBytes | str,
        gho_discount: int | None = None,
        e_mode: int | None = None,
    ) -> None:
        """Log when a new user is created.

        Args:
            user_address: The user's address
            block_number: Block number where user was created
            tx_hash: Transaction hash
            gho_discount: Initial GHO discount percent
            e_mode: Initial E-mode category
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "user_creation",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "user_address": user_address,
            "block_number": block_number,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
        }

        if gho_discount is not None:
            entry["gho_discount"] = gho_discount
        if e_mode is not None:
            entry["e_mode"] = e_mode

        self._write_entry(entry)

    def log_position_update(
        self,
        *,
        user_address: str,
        position_type: str,
        token_address: str,
        block_number: int,
        tx_hash: HexBytes | str,
        operation: str,
        balance_before: int,
        balance_after: int,
        balance_delta: int,
        index: int | None = None,
    ) -> None:
        """Log a position balance update.

        Args:
            user_address: The user's address
            position_type: "collateral" or "debt"
            token_address: The aToken or vToken address
            block_number: Block number
            tx_hash: Transaction hash
            operation: Operation type (e.g., "DEPOSIT", "BORROW", "REPAY")
            balance_before: Balance before update
            balance_after: Balance after update
            balance_delta: Change in balance
            index: Liquidity or borrow index
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "position_update",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "user_address": user_address,
            "position_type": position_type,
            "token_address": token_address,
            "block_number": block_number,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
            "operation": operation,
            "balance_before": balance_before,
            "balance_after": balance_after,
            "balance_delta": balance_delta,
        }

        if index is not None:
            entry["index"] = index

        self._write_entry(entry)

    def log_verification_start(
        self,
        *,
        block_number: int,
        user_addresses: list[str],
        position_type: str,
    ) -> None:
        """Log the start of position verification.

        Args:
            block_number: Block being verified
            user_addresses: List of user addresses being verified
            position_type: "collateral" or "debt"
        """
        if not self._enabled:
            return

        entry = {
            "type": "verification_start",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "block_number": block_number,
            "user_addresses": sorted(user_addresses),
            "user_count": len(user_addresses),
            "position_type": position_type,
        }

        self._write_entry(entry)

    def log_liquidation_call(
        self,
        *,
        user_address: str,
        liquidator: str,
        collateral_asset: str,
        debt_asset: str,
        debt_to_cover: int,
        liquidated_collateral: int,
        block_number: int,
        tx_hash: HexBytes | str,
        is_gho: bool = False,
    ) -> None:
        """Log a LIQUIDATION_CALL event.

        Args:
            user_address: The liquidated user's address
            liquidator: The liquidator's address
            collateral_asset: Collateral token address being seized
            debt_asset: Debt token address being repaid
            debt_to_cover: Amount of debt covered
            liquidated_collateral: Amount of collateral liquidated
            block_number: Block number
            tx_hash: Transaction hash
            is_gho: Whether this is a GHO liquidation
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "liquidation_call",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "user_address": user_address,
            "liquidator": liquidator,
            "collateral_asset": collateral_asset,
            "debt_asset": debt_asset,
            "debt_to_cover": debt_to_cover,
            "liquidated_collateral": liquidated_collateral,
            "block_number": block_number,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
            "is_gho": is_gho,
        }

        self._write_entry(entry)

    def log_liquidation_operation_start(
        self,
        *,
        operation_id: int,
        user_address: str,
        operation_type: str,
        collateral_asset: str,
        debt_asset: str,
        debt_to_cover: int,
        liquidated_collateral: int,
        scaled_events: Sequence[str],
        block_number: int,
        tx_hash: HexBytes | str,
    ) -> None:
        """Log the start of liquidation operation processing.

        Args:
            operation_id: Unique operation identifier
            user_address: The liquidated user's address
            operation_type: Type of operation (LIQUIDATION, GHO_LIQUIDATION, etc.)
            collateral_asset: Collateral token address
            debt_asset: Debt token address
            debt_to_cover: Amount of debt covered
            liquidated_collateral: Amount of collateral liquidated
            scaled_events: List of scaled token event types involved
            block_number: Block number
            tx_hash: Transaction hash
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "liquidation_operation_start",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "operation_id": operation_id,
            "user_address": user_address,
            "operation_type": operation_type,
            "collateral_asset": collateral_asset,
            "debt_asset": debt_asset,
            "debt_to_cover": debt_to_cover,
            "liquidated_collateral": liquidated_collateral,
            "scaled_events": scaled_events,
            "block_number": block_number,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
        }

        self._write_entry(entry)

    def log_liquidation_match(
        self,
        *,
        operation_id: int,
        user_address: str,
        scaled_event_type: str,
        token_address: str,
        matched_amount: int,
        extraction_data: dict[str, Any],
        block_number: int,
        tx_hash: HexBytes | str,
    ) -> None:
        """Log a liquidation event match.

        Args:
            operation_id: Operation identifier
            user_address: User being liquidated
            scaled_event_type: Type of scaled token event (DEBT_BURN, COLLATERAL_BURN, etc.)
            token_address: Token contract address
            matched_amount: Amount matched from extraction data
            extraction_data: Full extraction data from the match
            block_number: Block number
            tx_hash: Transaction hash
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "liquidation_match",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "operation_id": operation_id,
            "user_address": user_address,
            "scaled_event_type": scaled_event_type,
            "token_address": token_address,
            "matched_amount": matched_amount,
            "extraction_data": extraction_data,
            "block_number": block_number,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
        }

        self._write_entry(entry)

    def log_liquidation_verification(
        self,
        *,
        operation_id: int,
        user_address: str,
        debt_asset: str,
        collateral_asset: str,
        expected_debt_burn: int,
        actual_debt_burn: int | None,
        expected_collateral_liquidation: int,
        actual_collateral_burn: int | None,
        collateral_transfers: list[dict[str, Any]],
        verified: bool,
        block_number: int,
        tx_hash: HexBytes | str,
    ) -> None:
        """Log liquidation verification results.

        Args:
            operation_id: Operation identifier
            user_address: User being liquidated
            debt_asset: Debt token address
            collateral_asset: Collateral token address
            expected_debt_burn: Expected debt burn amount from LIQUIDATION_CALL
            actual_debt_burn: Actual debt burn from scaled token event (None if missing)
            expected_collateral_liquidation: Expected collateral from LIQUIDATION_CALL
            actual_collateral_burn: Actual collateral burn from scaled token event (None if missing)
            collateral_transfers: List of collateral transfer events
            verified: Whether verification passed
            block_number: Block number
            tx_hash: Transaction hash
        """
        if not self._enabled:
            return

        entry: dict[str, Any] = {
            "type": "liquidation_verification",
            "timestamp": datetime.now(tz=UTC).isoformat(),
            "operation_id": operation_id,
            "user_address": user_address,
            "debt_asset": debt_asset,
            "collateral_asset": collateral_asset,
            "expected_debt_burn": expected_debt_burn,
            "actual_debt_burn": actual_debt_burn,
            "expected_collateral_liquidation": expected_collateral_liquidation,
            "actual_collateral_burn": actual_collateral_burn,
            "collateral_transfers": collateral_transfers,
            "verified": verified,
            "block_number": block_number,
            "tx_hash": tx_hash.hex() if isinstance(tx_hash, HexBytes) else tx_hash,
        }

        self._write_entry(entry)

    def close(self) -> None:
        """Close the debug log file and write session end marker."""
        if not self._enabled or self._file_handle is None:
            return

        self._write_entry({
            "type": "session_end",
            "timestamp": datetime.now(tz=UTC).isoformat(),
        })

        self._file_handle.close()
        self._file_handle = None
        self._enabled = False


# Global instance
aave_debug_logger = AaveDebugLogger()
