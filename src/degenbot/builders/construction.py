"""The construction request vocabulary (ADR-085 D2).

`PoolConstructionRequest` is the seam's ONE construction input: the resolved
`(family, variant)` identity plus the family's hints, as a value. It is
re-exported from the Rust core verbatim — the request has no Python twin to
translate to, and a wrapper class would only give the same value two names.
"""

from __future__ import annotations

from degenbot._ffi import PoolConstructionRequest

__all__ = ["PoolConstructionRequest"]
