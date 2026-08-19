"""
Balance-snapshot and event-count verification for three-hop arbitrage tests.

Count transfers by inspecting raw log topic0 hashes from the transaction receipt.
Each ERC20 Transfer event (topic0 = ddf252ad…) and V4 Take event corresponds
to one physical ERC20 transfer() call — ground truth for transfer-count claims.
"""

# ERC20 Transfer(address indexed, address indexed, uint256)
# keccak256("Transfer(address,address,uint256)") =
#   0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef
# We match on the first 8 hex chars of topic0
_TRANSFER_TOPIC0_PREFIX = "ddf252ad"

# ERC6909 Transfer(address indexed, address indexed, address, uint256, uint256)
# Emitted by PM.mint() when converting a positive delta to ERC6909 balance.
# Counts as a "transfer" for the purpose of verifying token flow correctness.
_ERC6909_TRANSFER_TOPIC0_PREFIX = "1b3d7edb"


def _event_name(log) -> str:
    """Extract the event name from an Ape ContractLog object."""
    return getattr(log, "event_name", None) or type(log).__name__


def count_transfers(receipt, v4_pm=None) -> int:
    """Count physical ERC20 transfers + ERC6909 mints from the transaction receipt.

    Counts Transfer events by raw topic0 hash matching. Each ERC20 Transfer
    event corresponds to one physical ERC20 transfer() call that moved tokens.
    ERC6909 Transfer events (from PM.mint()) are also counted, as they represent
    an equivalent token flow (converting a PM delta into an ERC6909 claim).

    NOTE: We do NOT add V4.Take events separately, because every Take
    already produces a Transfer event (take() calls IERC20.transfer()
    internally). Adding both would double-count.
    """
    n_transfer = 0

    for raw_log in receipt.logs:
        if not isinstance(raw_log, dict):
            continue
        topics = raw_log.get("topics", [])
        if not topics:
            continue
        topic0 = topics[0]
        topic0_hex = topic0.hex() if isinstance(topic0, bytes) else str(topic0)
        if topic0_hex[:8] == _TRANSFER_TOPIC0_PREFIX:
            n_transfer += 1
        elif topic0_hex[:8] == _ERC6909_TRANSFER_TOPIC0_PREFIX:
            n_transfer += 1

    return n_transfer


def summarize_events(receipt) -> dict[str, int]:
    """Breakdown of all event types in a transaction receipt.

    Uses decoded logs for named events, plus raw topic0 matching for
    ERC20 Transfer events that may not decode properly.
    """
    counts: dict[str, int] = {}

    # From decoded logs
    for log in receipt.decode_logs():
        name = _event_name(log)
        counts[name] = counts.get(name, 0) + 1

    # Also count raw Transfer events (may be more than decoded)
    raw_transfers = 0
    for raw_log in receipt.logs:
        if not isinstance(raw_log, dict):
            continue
        topics = raw_log.get("topics", [])
        if not topics:
            continue
        topic0 = topics[0]
        topic0_hex = topic0.hex() if isinstance(topic0, bytes) else str(topic0)
        if topic0_hex[:8] == _TRANSFER_TOPIC0_PREFIX:
            raw_transfers += 1

    counts["Transfer(raw)"] = raw_transfers
    return counts


NATIVE_ADDRESS = "0x0000000000000000000000000000000000000000"


def snapshot_balances(tokens, accounts, track_eth=True):
    """Snapshot token + ETH balances: {(token_addr, account_addr): balance}.

    For each token, records balanceOf(account). If track_eth is True,
    also records the native ETH balance of each account using the
    key (NATIVE_ADDRESS, account_addr).
    """
    balances = {}
    for token in tokens:
        for account in accounts:
            addr = account.address if hasattr(account, "address") else account
            balances[(token.address, addr)] = token.balanceOf(addr)
    if track_eth:
        for account in accounts:
            addr = account.address if hasattr(account, "address") else account
            # For contract accounts, read .balance (ETH held by the contract).
            # For EOA accounts, this is the account's ETH balance.
            from ape import chain
            balances[(NATIVE_ADDRESS, addr)] = chain.provider.get_balance(addr)
    return balances


def diff_snapshots(before, after):
    """Non-zero balance changes between two snapshots."""
    changes = {}
    for key in set(list(before.keys()) + list(after.keys())):
        delta = after.get(key, 0) - before.get(key, 0)
        if delta != 0:
            changes[key] = delta
    return changes
