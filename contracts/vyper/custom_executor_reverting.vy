#pragma version ^0.5.0b2
"""
CustomExecutorReverting — the honest asymmetry, as a contract (ADR-100 D8).

Its ABI declares the same `record(uint256)` that ``custom_executor_probe.vy``
declares, so the identical calldata decodes against the identical declaration:
tier 1 accepts the call. Its code refuses to store anything, so no decoder can
tell the two apart — only running the code can. The `last_value` getter is
present so the certified read set is the same on both contracts, which leaves the
execution verdict as the single thing that differs.
"""

last_value: public(uint256)


@external
def record(_value: uint256) -> bool:
    raise "custom_executor_reverting: record stores nothing"
