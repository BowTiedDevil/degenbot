"""
Event filtering utilities for Aave V3 transaction processing.

Provides semantic filtering helpers that match events by user, token, and type
rather than relying on amount comparisons or log index proximity.
"""

from collections.abc import Iterator
from collections.abc import Set as AbstractSet

from degenbot.aave.events import ScaledTokenEventType
from degenbot.cli.aave_transaction_operations import ScaledTokenEvent


def filter_scaled_events[T: ScaledTokenEvent](
    *,
    events: list[T],
    assigned_indices: AbstractSet[int],
    event_type: ScaledTokenEventType | AbstractSet[ScaledTokenEventType] | None = None,
    user_address: str | None = None,
    token_address: str | None = None,
) -> Iterator[T]:
    """
    Filter scaled token events by semantic criteria.

    This helper eliminates repetitive filtering code across operation creation
    functions. It filters by:
    - Event type (single type or set of types)
    - User address (the onBehalfOf/from/to address)
    - Token contract address

    All filters are optional - if None, that criterion is not checked.

    Args:
        events: List of scaled token events to filter
        assigned_indices: Set of already-assigned log indices to skip
        event_type: Expected event type(s), or None for any type
        user_address: Expected user address, or None for any user
        token_address: Expected token contract address, or None for any token

    Yields:
        Events matching all specified criteria
    """

    # Normalize event_type to a set for uniform checking
    type_set: AbstractSet[ScaledTokenEventType] | None
    if event_type is None:
        type_set = None
    elif isinstance(event_type, ScaledTokenEventType):
        type_set = {event_type}
    else:
        type_set = event_type

    for ev in events:
        # Skip already assigned events
        if ev.event["logIndex"] in assigned_indices:
            continue

        # Check event type
        if type_set is not None and ev.event_type not in type_set:
            continue

        # Check user address
        if user_address is not None and ev.user_address != user_address:
            continue

        # Check token address
        if token_address is not None:
            ev_token = ev.event["address"]
            if ev_token != token_address:
                continue

        yield ev


def find_first_scaled_event[T: ScaledTokenEvent](
    events: list[T],
    assigned_indices: AbstractSet[int],
    *,
    event_type: ScaledTokenEventType | AbstractSet[ScaledTokenEventType] | None = None,
    user_address: str | None = None,
    token_address: str | None = None,
) -> T | None:
    """
    Find the first scaled event matching criteria, or None if not found.

    This is a convenience wrapper around filter_scaled_events for the common
    case where you only need the first match (e.g., finding a primary burn).

    Args:
        events: List of scaled token events to search
        assigned_indices: Set of already-assigned log indices to skip
        event_type: Expected event type(s), or None for any type
        user_address: Expected user address, or None for any user
        token_address: Expected token contract address, or None for any token

    Returns:
        First matching event, or None if no match found

    Example:
        >>> matched_mint = find_first_scaled_event(
        ...     events=scaled_events,
        ...     assigned_indices=assigned_indices,
        ...     event_type=ScaledTokenEventType.COLLATERAL_MINT,
        ...     user_address=on_behalf_of,
        ...     token_address=a_token_address,
        ... )
        >>> if matched_mint:
        ...     process_mint_event(matched_mint)
    """

    try:
        return next(
            filter_scaled_events(
                events=events,
                assigned_indices=assigned_indices,
                event_type=event_type,
                user_address=user_address,
                token_address=token_address,
            )
        )
    except StopIteration:
        return None
