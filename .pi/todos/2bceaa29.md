{
  "id": "2bceaa29",
  "title": "Add built-in bribe system integration",
  "tags": [
    "bribe",
    "MEV",
    "encoding",
    "P2"
  ],
  "status": "ready-for-agent",
  "created_at": "2026-06-13T23:08:37.208Z"
}

## Summary
The new cmd_executor has built-in MEV bribe support via BRIBE_COINBASE (0x02) and BRIBE_ADDRESS (0x03) preprocessing commands. These compute `profit × bips / 10000` ETH and send it to `block.coinbase` or an arbitrary address. This replaces the current approach of manually computing priority fees — the bribe is proportional to ACTUAL on-chain profit, not estimated.

## Bribe Encoding (preprocessing section)

```python
# BRIBE_COINBASE: Send 5% of profit to block builder
preamble = BEGIN_PREPROCESSING
preamble += enc_set_addresses(address_table)
preamble += CMD_BRIBE_COINBASE + _e(500, 2)  # 500 bips = 5%
preamble += BEGIN_EXECUTION
```

```python
# BRIBE_ADDRESS: Send 10% to a specific address (e.g., relay)
preamble += CMD_BRIBE_ADDRESS + _e(recipient_idx, 1) + _e(1000, 2)
```

## Key Properties
- **Calculated AFTER execution**: bribe = profit × bips / 10000
- **Auto-wraps**: if insufficient ETH but has WETH, automatically unwraps
- **Never reverts**: sends whatever is available if full bribe can't be paid
- **Max bips**: 10,000 = 100% of profit

## Integration Points

### 1. Add encoding helpers to cmd_stream.py
```python
def enc_bribe_coinbase(bips: int) -> bytes:
    """BRIBE_COINBASE: [0x02][bips:2] — 3 bytes (preprocessing)."""
    return CMD_BRIBE_COINBASE + _e(bips, 2)

def enc_bribe_address(recipient_idx: int, bips: int) -> bytes:
    """BRIBE_ADDRESS: [0x03][recipient_idx:1][bips:2] — 4 bytes (preprocessing)."""
    return CMD_BRIBE_ADDRESS + _e(recipient_idx, 1) + _e(bips, 2)
```

### 2. Update enc_preamble() to accept bribe parameters
```python
def enc_preamble(
    address_table: AddressTable,
    skip_profit: bool = False,
    bribe_bips: int = 0,  # 0 = no bribe
    bribe_address_idx: int | None = None,  # None = coinbase, int = address table index
) -> bytes:
```

### 3. Integrate with dispatch pipeline
The bot currently computes priority fees via `_compute_priority_fee()`. With built-in bribing:
- Use a fixed bribe_bips (e.g., 80% = 8000 bips) instead of computing priority fees
- The bribe is proportional to on-chain profit — no estimation error
- Priority fee can be set to 0 or minimum (the bribe handled by the contract)
- Or use a hybrid: minimum priority fee + contract-level bribe

### 4. Gas Impact
The bribe logic adds ~300 gas when bribe_bips > 0 (profit calculation + ETH transfer). When bribe_bips == 0 AND skip_profit_check, the fast path saves ~300 gas by skipping all balance reads.

## Depends On
TODO-8c71dfa9 (encoding fixes), TODO-87bb26eb (expected_balance for profit check when bribe > 0)
