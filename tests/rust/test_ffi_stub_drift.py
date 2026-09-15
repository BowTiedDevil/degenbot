"""Registration↔stub drift gate (ADR-053).

The hand-maintained ``src/degenbot/_ffi/*.pyi`` stubs stay the surface of
record for the PyO3 extension seam (ADR-013). ``mypy.stubtest`` now owns the
existence-direction checks this file used to hand-roll in R1/R2/R3/R4
(the bespoke AST/introspection machinery was ~370 lines and needed a curated
52-row class table that had to be extended by hand for every new pyclass):

- **R1 (coverage)** — every runtime symbol, on every module AND every class
  member, must appear in its stub. stubtest walks the installed extension.
- **R2 (honest ``__all__``)** — stub ``__all`` matches runtime ``__all__`` in
  BOTH directions. Verified: stubtest flags both the missing
  re-export and the phantom promise (a stub-exported name
  removed from the runtime ``__all__`` fails the run).
- **R3 (no phantom definitions)** — every stub-defined symbol must exist at
  runtime. Verified: a stub-declared top-level function deleted from the
  runtime fails stubtest. Annotation-only type imports/aliases document
  parameter typing rather than the module surface and were exempted by the
  old R3; they now live in ``tests/rust/stubtest_allowlist.txt``.
- **R4 (class-member drift, generalized)** — stubtest checks member presence
  in BOTH directions on EVERY class, which is what the curated
  ``_CLASS_STUBS`` table approximated. PyO3-synthetic introspection noise
  (``@final`` semantics, opaque constructor/signature introspection,
  object-protocol dunders) is encoded as rule-annotated allowlist entries,
  not test code.

Run it via ``just lint-stubtest`` (stubtest against the installed ``.so``;
staleness of that artifact is owned by the build-receipt gates, AGENTS.md).

This file keeps ONLY the checks stubtest cannot perform:

- **R0 (submodule coverage)** — every runtime ``degenbot._ffi.*`` submodule
  must have a stub file. stubtest's module walk cannot be relied on across
  PyO3's ``add_submodule`` registration, and this check is a dozen lines.
- **R5 (retired-surface tombstones, SLIMMED)** — tombstones for surface
  *deletions*, kept only for names whose runtime home stubtest never visits:
  pure-Python driver modules (stubtest runs solely against ``degenbot._ffi``).
  Exposing each retired
  ``degenbot._ffi.ArbitrageEngine`` name at runtime (which is the very object
  ``degenbot.arbitrage.engine_registry`` re-exports) FAILS stubtest — a
  resurrected runtime name on the compiled seam must also be added to the
  stub, and that stub edit is the review-forcing event ADR-053 accepts in
  place of tombstone data. The 11 engine-mapped tombstones were therefore
  deleted; the two ``degenbot.uniswap.snapshot_binary`` rows below stay
  because stubtest never imports that module and the proof does not hold for
  Python-side surface.
"""

from __future__ import annotations

import importlib
import pathlib
import sys

import pytest

_STUB_DIR = pathlib.Path(__file__).resolve().parents[2] / "src" / "degenbot" / "_ffi"


def test_every_runtime_submodule_has_a_stub() -> None:
    """R0: every runtime `degenbot._ffi.*` submodule has a stub file."""
    import degenbot._ffi  # ruff: ignore[unused-import]  (registers the submodules)

    stub_stems = {p.stem for p in _STUB_DIR.glob("*.pyi") if p.name != "__init__.pyi"}
    runtime_subs = {
        name.removeprefix("degenbot._ffi.")
        for name in sys.modules
        if name.startswith("degenbot._ffi.")
    }
    missing = sorted(runtime_subs - stub_stems)
    assert not missing, f"runtime submodules missing a stub: {missing}"


# ---------------------------------------------------------------------------
# R5 — tombstones for retired PURE-PYTHON surface (slimmed; see docstring).
# stubtest cannot see these: their runtime home is a driver module, not the
# compiled seam. Resurrecting one requires deleting its row here in the same
# commit, with justification, so the decision is reviewed rather than silent.
# ---------------------------------------------------------------------------
_RETIRED_NAMES: tuple[tuple[str, str], ...] = (
    # DADWUP: the SQLAlchemy yield_per loops.
    ("degenbot.uniswap.snapshot_binary", "stream_v3_snapshot_to_engine"),
    ("degenbot.uniswap.snapshot_binary", "stream_v4_snapshot_to_engine"),
)


@pytest.mark.parametrize(("owner", "name"), _RETIRED_NAMES)
def test_retired_names_stay_absent(owner: str, name: str) -> None:
    """R5: tombstoned driver-module surface never returns to the runtime."""
    assert not hasattr(importlib.import_module(owner), name), (
        f"{owner} exposes retired name {name!r} — it was deleted by DADWUP. "
        f"If resurrecting it deliberately, remove the R5 tombstone row in "
        f"tests/rust/test_ffi_stub_drift.py with justification in the same change."
    )
