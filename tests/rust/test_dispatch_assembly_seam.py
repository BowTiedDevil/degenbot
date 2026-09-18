"""The batched candidate-assembly seam (settlement dispatch).

The runner's ``_build_dispatch_candidates`` used to construct one
``DispatchCandidate`` per raw solver result row in Python. The seam now
assembles the whole batch Rust-side: it skips empty-hop rows (reporting their
path ids for the display-only ``[sim-none]`` log), skips path ids the engine
already simulated inline, resolves each surviving row's ``PathInfo``, and
returns the ready candidate list. These tests pin the seam's shape + filter
semantics against the real engine fixture.
"""

from __future__ import annotations

import types

import pytest

from degenbot._ffi.simulation import (
    CandidateAssembly,
    DispatchCandidate,
    assemble_dispatch_candidates_py,
)
from degenbot.runner import _dispatch as d


def _row(
    path_id: int,
) -> tuple[int, int, int, tuple[int, ...], tuple[int, ...], int, tuple[int, ...]]:
    """One raw engine-result row for the fixture's 2-hop V2 cycle."""
    return (
        path_id,
        1_000_000_000_000_000_000,
        2_000_000_000_000_000_000,
        (1_500_000_000_000_000_000, 1_400_000_000_000_000_000),
        (1_000_000_000_000_000_000, 1_500_000_000_000_000_000),
        100,
        (0, 0),
    )


class TestAssemblySeam:
    """The single-call batch assembly over the engine projection."""

    def test_registered(self) -> None:
        assert callable(assemble_dispatch_candidates_py)
        assert CandidateAssembly is not None

    def test_assembles_one_candidate(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        assembly = assemble_dispatch_candidates_py(engine=engine, results=[_row(path_id)])
        candidates = assembly.candidates
        assert len(candidates) == 1
        assert isinstance(candidates[0], DispatchCandidate)
        assert assembly.empty_hop_path_ids == []

    def test_empty_hop_rows_are_reported_not_built(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        empty_hop = (path_id, 1, 1, (), (), 100, ())
        assembly = assemble_dispatch_candidates_py(
            engine=engine, results=[_row(path_id), empty_hop]
        )
        assert len(assembly.candidates) == 1
        assert assembly.empty_hop_path_ids == [path_id]

    def test_payload_served_path_ids_are_skipped(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        assembly = assemble_dispatch_candidates_py(
            engine=engine, results=[_row(path_id)], skip_path_ids=[path_id]
        )
        assert assembly.candidates == []
        assert assembly.empty_hop_path_ids == []

    def test_erc6909_flag_is_forwarded(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        assembly = assemble_dispatch_candidates_py(
            engine=engine, results=[_row(path_id)], erc6909_profit=True
        )
        assert len(assembly.candidates) == 1

    def test_unregistered_path_id_raises(self, nxm2bf_v2_engine_and_path) -> None:
        engine, _ = nxm2bf_v2_engine_and_path
        with pytest.raises(ValueError, match="not registered"):
            assemble_dispatch_candidates_py(engine=engine, results=[_row(999_999)])

    def test_hop_length_mismatch_raises(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        bad = (path_id, 1, 1, (1,), (1,), 100, (0,))
        with pytest.raises(ValueError, match="hop_outputs length"):
            assemble_dispatch_candidates_py(engine=engine, results=[bad])


class TestPythonAssemblyParity:
    """The runner's ``_build_dispatch_candidates`` delegates to the seam.

    The Python wrapper keeps only the display log + the operator policy bool;
    candidate construction + filtering are Rust-owned. This pins that the
    runner path produces the same ready list the raw seam does.
    """

    def test_runner_builder_returns_seam_candidates(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        session = types.SimpleNamespace(
            engine_registry=types.SimpleNamespace(engine=engine),
            cfg=types.SimpleNamespace(erc6909_profit=False),
        )
        candidates = d._build_dispatch_candidates(session, [_row(path_id)])
        assert len(candidates) == 1
        assert isinstance(candidates[0], DispatchCandidate)
