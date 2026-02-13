---
description: Debug Aave update failures
agent: build
---

!`DEGENBOT_VERBOSE_USERS=$ARGUMENTS uv run degenbot aave update --no-progress-bar --one-chunk 2>&1`

## DIRECTION: Investigate and debug this failed Aave update command. Find the root cause of the bug and fix it.

## PROCESS:
### 1. Gather Information
- Parse the output to identify information about the events, processes, and state logs leading up to the failed verification
- @evm-investigator Perform a thorough investigation of the transaction; use all known information about the blocks, transactions, and operations leading to the invalid state

### 2. Investigate Code
- Determine the execution path leading to the error
- Generate a failure hypothesis

### 3. Validate Execution Path and Failure Hypothesis
- Consider enabling function call logging along the execution path by decorating functions and methods, e.g.,
    ```python
    @log_function_call  # added
    def some_func(...): ...  
    ```
- Determine if an debugging env var is useful:
    - `DEGENBOT_VERBOSE_USER=0x123...,0x456...`
    - `DEGENBOT_VERBOSE_TX=0xabc...,0xdef...`
    - `DEGENBOT_VERBOSE_ALL=1`
    - `DEGENBOT_DEBUG=1`
    - `DEGENBOT_DEBUG_FUNCTION_CALLS=1` which will show function call logs
- Run the updater again with selected verbosity env vars prepended

### 4. Validate & Fix
- Determine the root cause, e.g., a processing function failed to determine the correct value from an event, the database had a stale value, a previous processing action set a value incorrectly which was used by another processing action
- Fix the error and run the update again to confirm it works

### 5. Improve
- @explore Review opportunities to refactor and clean up code that contributed to this failure

### 6. Document Findings
Append to @aave_debug_progress.md. Follow this format:
- **Issue:** Brief title
- **Date:** Current date
- **Symptom:** Error message verbatim
- **Root Cause:** Technical explanation
- **Transaction Details:** Hash, block, type, user, asset
- **Fix:** Code location and changes
- **Key Insight:** Lesson learned for future debugging
- **Refactoring:** Concise summary of proposed improvements to code that processes these transactions

### 7. Cleanup
- Remove leftover files in @.opencode/tmp