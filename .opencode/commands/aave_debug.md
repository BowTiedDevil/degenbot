---
description: Debug Aave update failures
agent: build
model: synthetic/hf:moonshotai/Kimi-K2.5
---

!`uv run degenbot aave update --no-progress-bar --one-chunk`

## DIRECTION: Investigate and debug this failed Aave update command

## PROCESS:
### 1. Gather Information
- Parse the output to identify the transaction hash associated with the event triggering the processing error
- Delegate to @evm-investigator

### 2. Investigate Code
- Determine the execution path leading to the error
- Generate a failure hypothesis

### 3. Validate Execution Path and Failure Hypotheses
- If the execution path is unclear, apply the `log_function_call` decorator to confirm function calls, e.g.,
    ```python
    @log_function_call
    def some_func(...): ...  
    ```
- Determine if a debugging env var is useful:
    - `DEGENBOT_VERBOSE_USER=0x123...,0x456...`
    - `DEGENBOT_VERBOSE_TX=0xabc...,0xdef...`
    - `DEGENBOT_VERBOSE_ALL=1`
    - `DEGENBOT_DEBUG=1`
    - `DEGENBOT_DEBUG_FUNCTION_CALLS=1`
- Run with any verbosity flags prepended, e.g., `DEGENBOT_DEBUG=1 uv run degenbot aave update --no-progress-bar --one-chunk`

### 4. Fix & Validate
- If a hypothesis is validated and the root cause is clear, implement a fix and run the update again

### 5. Document Findings
Append to @aave_debug_progress.md. Follow this format:
- **Issue:** Brief title
- **Date:** Current date
- **Symptom:** Error message verbatim
- **Root Cause:** Technical explanation
- **Transaction Details:** Hash, block, type, user, asset
- **Fix:** Code location and changes
- **Key Insight:** Lesson learned for future debugging
- **Refactoring:** Concise summary of proposed improvements to code that processes these transactions