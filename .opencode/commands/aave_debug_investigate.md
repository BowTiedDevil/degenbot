---
description: Investigate Aave update failures
agent: build
---

**FUNDAMENTAL PREMISE**: Values in the database have been validated and should be treated as accurate. A failure to verify a given block is the result of processing error(s) that do not reflect the actual operation of the smart contracts.

**CRITICAL**: Do not modify any code during this investigation. Wait for review before implementing any fixes.

## Direction: 
This session has been called within `tmux`. Investigate a failed Aave update by splitting the current pane vertically and sending an update command to it, e.g. `DEGENBOT_DEBUG=1 DEGENBOT_PROGRESS_BAR=0 uv run degenbot aave update`. Tee the output to a new file in /tmp for analysis later.

## Issue ID Assignment
Run the helper script to gather relevant information for the market:
```bash
uv run python scripts/aave_debug_helper.py --market-id <MARKET_ID>
```

This outputs:
- **Next issue ID** (e.g., 0030) - use this for your debug report filename
- Market information (name, chain_id)
- RPC URL for the chain
- Pool contract revision
- All asset token revisions (aToken and vToken)

## Inspect the output and filter for relevant info
Grep the update result as needed to identify the failure, information related to the user/operation/transaction/token, etc, and the block where the failed verification occurred.

**Common grep patterns:**
- `grep -i "verification failed"` - Find verification failures
- `grep -i "mismatch"` - Locate balance or state mismatches
- `grep -E "block [0-9]+"` - Extract block numbers from output
- `grep -iE "(supply|borrow|repay|withdraw)"` - Identify operation types

## Investigate the Transaction
Assign @evm-investigator to inspect the transaction and prepare a detailed report in `/tmp` showing the smart contracts used, the external and internal control flow, the events emitted, state modified, and asset transfers involved.

Use the RPC URL from the helper script output.

## Investigate the Aave Deployment
Use the contract revisions provided by the helper script:
- Pool contract revision from `aave_v3_contracts` table
- Asset token revisions (aToken and vToken) from `aave_v3_assets` table

All revisions are accurate as of the `last_update_block` in the `aave_v3_markets` table.

## Investigate Code
1. **Review the contract flow diagram**: Use @explore to examine the flow diagrams in @docs/aave and understand the execution path for the given operations
2. **Inspect the smart contract source**: Use @explore to review the specific revision of source code in `contract_reference/aave` that matches the deployment revision
3. **Trace the execution path**: Determine the exact smart contract execution path used in the transaction
4. **Generate a failure hypothesis**: Explain why the local processing code failed to replicate the contract behavior. The hypothesis must include:
   - What operation is being modeled
   - What functions and data structures were used
   - What arithmetic was performed
   - Which events were matched with the operation and what values were used from those events

## Propose a Fix
Check the recent git commits and the debug reports in @debug/aave 

Propose a fix that will address the root cause while preserving the invariants and fixes already in place

**CRITICAL**: Adding a tolerance to any verification function is unacceptable. The amounts must match exactly.

Propose changes to architecture if needed to cleanly separate problematic code

Consider alternatives to the proposed fix and evaluate them for architectural cleanliness and robustness 

## Document Findings
Create a new report in `debug/aave` following this template:
- **Filename Format**: `{four digit ID} - {issue title}.md` (e.g., `0030 - supply_borrow_mismatch.md`)
- **Issue:** Brief title
- **Date:** Current date
- **Symptom:** Error message verbatim
- **Root Cause:** Technical explanation
- **Transaction Details:** Hash, block, type, user, asset
- **Fix:** File, function, line number, with brief description of that code's purpose
- **Key Insight:** Lesson learned for future debugging
- **Refactoring:** Concise summary of proposed improvements to code that processes these transactions

## Summarize
Summarize the failure, the root cause, the proposed fix, and alternatives

Reference existing debug reports in @debug/aave for examples of format and detail level