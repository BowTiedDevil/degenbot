{
  "id": "cbfabb70",
  "title": "Use V4_MINT_COMPACT for profit capture on V4→V4 paths",
  "tags": [
    "encoding",
    "ERC6909",
    "V4-MINT",
    "gas-optimization",
    "P1"
  ],
  "status": "ready-for-agent",
  "created_at": "2026-06-13T23:08:10.549Z"
}

## Summary
The V4_MINT_COMPACT command (0x58) captures profit as ERC6909 internal balance inside the PoolManager instead of taking physical WETH. This saves ~20K gas (−18.3%) by eliminating the ERC20 `transfer()` call. For pure V4→V4 paths, profit should be minted as ERC6909, not physically taken.

## When to Use V4_MINT_COMPACT
- ✅ V4→V4 2-hop: profit as ERC6909 (0 physical transfers)
- ✅ V4→V4→V4 3-hop: profit as ERC6909 (0 physical transfers)
- ❌ Profit in non-WETH tokens (V2/V3 need physical ERC-20)
- ❌ Paths where the recipient is not the executor (V2/V3 need physical tokens)

## Encoding Change

### 2-hop V4→V4 (same currency)
```python
# Old:
inner += enc_v4_take_delta(weth_idx, executor_idx)
inner += enc_v4_settle_all()

# New:
inner += enc_v4_mint_compact(weth_idx, executor_idx, profit_amount)
inner += enc_v4_settle_all()
```

### 3-hop V4→V4→V4
```python
# Old:
inner += enc_v4_settle_all()  # no explicit profit take — settled by SETTLE_ALL

# New:
inner += enc_v4_mint_compact(weth_idx, executor_idx, profit_amount)
inner += enc_v4_settle_all()
```

## Integration with Profit Check
When V4_MINT_COMPACT is used for profit capture, use check_mode=2:
```python
expected_balance = (2 << 248) | erc6909_before
```
This reads `PM.balanceOf(executor, weth_id)` (warm ~100 gas) instead of `WETH.balanceOf(executor)` (cold ~4,700 gas for pure-V4 paths with no physical transfers).

## V4_BURN_COMPACT (0x59) for Multi-TX Compounding
When the executor already holds ERC6909 from a prior V4_MINT, V4_BURN_COMPACT converts it back to a +delta to settle debts:
```python
inner += enc_v4_burn_compact(weth_idx, needed_amount)
```
This is a future optimization for multi-transaction compounding — not needed for the initial V4_MINT integration.

## Encoder Changes (eth_backrun_helpers.py)
- `_encode_cmd_v4_v4()`: Use V4_MINT_COMPACT for same-currency profit, V4_TAKE for cross-currency
- `_3hop_v4_v4_v4()`: Use V4_MINT_COMPACT for profit
- Also applicable to V2-V4-V4 and V3-V4-V4 3-hop paths where V4_TAKE is used for WETH profit

## Depends On
TODO-8c71dfa9 (encoding fixes), TODO-87bb26eb (expected_balance for mode 2)
