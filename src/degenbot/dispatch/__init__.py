"""Companion-layer surface over the Rust dispatch/sim/signer pyclasses.

This package re-exports the Rust-owned dispatch, simulation, and transaction
signing symbols under stable, seam-agnostic names. Driver code (bot
operators, example bots) imports from here — never from the PyO3 wrapper
module degenbot._ffi.

The Py* prefix and *_py suffix on the FFI names are the seam naming itself:
Py* marks a raw pyclass, *_py marks a pyfunction. Those conventions are for
the FFI layer's own bookkeeping and should never leak into driver code. This
package hides them.

.. note::

    Most of these are **direct alias re-exports** (from degenbot._ffi import
    PyX as X), not Python wrapper classes. The Rust engine constructs and
    consumes these pyclasses / pyfunctions directly — driver code constructs
    a SimulateContext / TxSigner in Python and passes it to a Rust
    pyfunction that expects the exact SimulateContext / TxSigner pyclass. A
    wrapper class would break type identity at the FFI boundary.

dispatch_and_submit is the one deliberate exception: it is a thin async
wrapper whose ONLY transformation is decoding the leaf's returned record
dicts into the typed records of degenbot.dispatch.records (the single home
for that wire format). Call-site arguments pass to the FFI pyfunction
unchanged — pyclass identity for candidates/dispatcher/signer is preserved.

Symbol map (FFI name → stable companion name):

- CandidateAssembly → CandidateAssembly
- DispatchCandidate → DispatchCandidate
- DispatchOutcome → DispatchOutcome
- Dispatcher → Dispatcher
- SimulateContext → SimulateContext
- TxSigner → TxSigner
- assemble_dispatch_candidates_py → assemble_dispatch_candidates
- dispatch_profitable_py → dispatch_profitable
- merge_payload_results_py → merge_payload_results
- dispatch_and_submit_py → dispatch_and_submit (wrapper: dict → typed records)
- fetch_fee_history_py → fetch_fee_history
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from degenbot._ffi.simulation import (
    CandidateAssembly,
    DispatchCandidate,
    DispatchOutcome,
    PayloadOutcome,
    PayloadVerdict,
    SimulateContext,
)
from degenbot._ffi.simulation import assemble_dispatch_candidates_py as assemble_dispatch_candidates
from degenbot._ffi.simulation import dispatch_profitable_py as dispatch_profitable
from degenbot._ffi.simulation import merge_payload_results_py as merge_payload_results
from degenbot._ffi.submission import Dispatcher, SubmitCandidate, TxSigner
from degenbot._ffi.submission import dispatch_and_submit_py as _dispatch_and_submit_py
from degenbot._ffi.submission import fetch_fee_history_py as fetch_fee_history
from degenbot.dispatch.records import (
    SkippedRecord,
    SubmitRecord,
    SubmitSkipReason,
    SubmittedRecord,
    typed_submit_record,
)

if TYPE_CHECKING:
    # The FFI pyfunction dispatch_and_submit_py requires the Rust pyclass,
    # not the degenbot.provider wrapper — annotate the seam accordingly.
    from degenbot._ffi.provider import AsyncAlloyProvider


@dataclass(frozen=True, slots=True)
class SubmitContext:
    """Per-call parameters the companion submit seam carries to the FFI leaf.

    The Rust ``dispatch_and_submit_py`` pyfunction takes these as keyword
    arguments; grouping them here keeps the companion signature stable as the
    runner's submit knobs grow. ``broadcast_providers`` is the relay fan-out
    (one Rust provider pyclass per endpoint), or ``None`` for the public
    mempool.

    """

    signer: TxSigner
    operator_nonce: int
    current_block: int
    dry_run: bool
    inject_code: bool
    broadcast_providers: list[AsyncAlloyProvider] | None = None


async def dispatch_and_submit(
    candidates: list[SubmitCandidate],
    dispatcher: Dispatcher,
    provider: AsyncAlloyProvider,
    *,
    context: SubmitContext,
) -> list[SubmitRecord]:
    """Await the Rust submit leaf and decode its records to typed values.

    The FFI pyfunction returns raw dicts (a "kind" discriminator plus
    payloads); this companion wrapper is the single home for decoding them
    into SubmittedRecord / SkippedRecord — unknown wire values raise instead
    of dropping a submission event. ``context`` carries the per-call submit
    knobs through to the pyfunction unchanged; pyclass identity for
    candidates/dispatcher/provider/context.signer is preserved.

    Returns:
        The typed per-candidate records, in submit order.

    """
    raw = await _dispatch_and_submit_py(
        candidates=candidates,
        dispatcher=dispatcher,
        provider=provider,
        signer=context.signer,
        operator_nonce=context.operator_nonce,
        current_block=context.current_block,
        dry_run=context.dry_run,
        inject_code=context.inject_code,
        broadcast_providers=context.broadcast_providers,
    )
    return [typed_submit_record(record) for record in raw]


__all__ = [
    "CandidateAssembly",
    "DispatchCandidate",
    "DispatchOutcome",
    "Dispatcher",
    "PayloadOutcome",
    "PayloadVerdict",
    "SimulateContext",
    "SkippedRecord",
    "SubmitCandidate",
    "SubmitContext",
    "SubmitRecord",
    "SubmitSkipReason",
    "SubmittedRecord",
    "TxSigner",
    "assemble_dispatch_candidates",
    "dispatch_and_submit",
    "dispatch_profitable",
    "fetch_fee_history",
    "merge_payload_results",
    "typed_submit_record",
]
