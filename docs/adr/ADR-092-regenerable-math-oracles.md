# ADR-092: Frozen math oracles are regenerable — one generator per snapshot, the deployed contract is the authority

**Status:** accepted (three generators in tree; each reproduces its frozen snapshot byte-for-byte)

## Context

Three cross-check suites in `degenbot-math` replay a frozen snapshot of expected
values through the Rust math leaf and assert byte equality:

| Snapshot | Suite |
|---|---|
| `tests/oracle_curve_snapshot.txt` | `tests/oracle_crosscheck_curve.rs` |
| `tests/oracle_balancer_snapshot.txt` | `tests/oracle_crosscheck_balancer.rs` |
| `tests/oracle_solidly_snapshot.txt` | `tests/oracle_crosscheck_solidly.rs` |

The values in those files were produced by throwaway dumper scripts driving the
old Python reference implementations. Those implementations were retired as the
math routed through Rust (the last of them with
`src/degenbot/calculations/stableswap.py`, `solidly_stable.py`, `camelot.py` and
`src/degenbot/balancer/libraries/*`), and the dumper scripts lived in `/tmp`.
Both are gone; the snapshots outlived their provenance.

That left the oracles in an awkward state. They are still load-bearing — they are
the only byte-level check that the leaves implement the contracts rather than
just agreeing with themselves — but nothing in tree could recompute a single
expected value. Re-deriving one meant re-porting the math from memory, so in
practice nobody would: the snapshot would silently become unfalsifiable, and the
next legitimate regeneration request would get a hand-edited file.

Two further facts constrain the fix. First, the snapshots' expected values are
*frozen*: they are the parity ground truth the suites replay, so a change to one
is a change of contract, not a consequence of refactoring. Second, what those
values mean is settled — **the deployed contract semantics are authoritative,
and the frozen Python was one port of them** — which is why the leaves' headers
cite the contracts rather than the Python that happened to be the first port.

## Decision

### D1 — One generator per snapshot, checked into `scripts/oracles/`

| Generator | Snapshot it renders | Oracle math it computes with |
|---|---|---|
| `scripts/oracles/generate_curve_snapshot.py` | `oracle_curve_snapshot.txt` | `scripts/oracles/curve_stableswap_reference.py` |
| `scripts/oracles/generate_balancer_snapshot.py` | `oracle_balancer_snapshot.txt` | `scripts/oracles/balancer_{fixed_point,log_exp_math,weighted_math,stable_math}_reference.py` |
| `scripts/oracles/generate_solidly_snapshot.py` | `oracle_solidly_snapshot.txt` | `scripts/oracles/solidly_stable_reference.py`, `scripts/oracles/camelot_reference.py` |

Each generator owns the fixture sweep, computes every expected value itself, and
renders the complete snapshot body — including the row format its suite parses
(each module docstring states that format). `scripts/oracles/snapshot_io.py`
carries the shared plumbing and `scripts/oracles/evm_shim.py` the small
revert/constant vocabulary the reference math needs.

### D2 — The oracle math is the retired reference, restored, not the Rust leaf

An oracle that imported the code it validates would be a tautology. The
reference modules are the retired Python implementations restored verbatim from
git history, with their imports re-pointed at `evm_shim` instead of the driver:
same functions, same rounding, no `degenbot` import, not importable as
`degenbot`. They sit under `scripts/`, outside the production library surface
and outside every build, install and stub path.

This is the language choice recorded deliberately: the snapshots' expected values
*are* that Python's output, so a generator in any other language would be a third
port whose first job is to prove it agrees with the second. Python also keeps the
sweep tables readable as data and adds no dependency (it runs under
`uv run --no-sync python` via the justfile's `py_run` convention:
`just py python scripts/oracles/generate_<family>_snapshot.py`).

### D3 — Each generator names its own output path

The frozen snapshots are read-only. Every generator renders a body and either
writes it to `--out` (default `target/oracle-regen/<snapshot name>`, outside the
tree) or, with `--check`, compares it against the frozen snapshot byte-for-byte
and reports match/mismatch with a diff. There is no mode that writes over a
committed snapshot: an `--out` that names one is refused outright.

Determinism is a property of each generator, not an intention: identical inputs
render identical bytes. That makes `--check` a usable gate — the fidelity of a
reference port is self-verifying, and a snapshot's regeneration is a reviewable
command rather than an act of faith.

### D4 — A mismatch is a leaf bug, never a snapshot edit

The authority chain runs deployed contract → frozen snapshot → Rust leaf. When a
judged-correct reference disagrees with a snapshot row, the snapshot is right and
the leaf is wrong: the finding is reported and the leaf is fixed in its own
change, with the snapshot untouched and the suite's assertion as the failure
proving it. Editing the snapshot to match a regenerated value would move the
parity ground truth to whatever the leaf currently does.

Correspondingly, the Solidly generator reads its *inputs* back from the frozen
snapshot: that corpus was drawn from a seeded RNG whose draw sequence was never
archived, so the arguments cannot be re-derived and only the results are
recomputed. Regenerating that snapshot from new random draws is not supported by
this ADR and would need the seed recorded at generation time — the honest
statement of a loss, not a silently weaker generator.

## Consequences

- The three oracles are falsifiable again. Recomputing an expected value is one
  command; verifying a leaf edit against a second implementation is one
  `--check`. The snapshots themselves did not move: all three regenerate
  byte-identically.
- The reference ports are dev-only code. They carry no production consumer, are
  not built, not installed, not stubbed, and cannot be imported as `degenbot`.
  The pyo3-free core invariant and the driver's surface are untouched by them.
- The cost is a second port per math family to keep honest. `--check` is what
  keeps the cost payable: a drifted port shows up as a diff against a frozen
  file, not as a slow rot nobody notices.
- A leaf fix landed under D4 keeps the suite green only by matching the frozen
  value; a suite failure after a leaf change is the oracle doing its job, and the
  generator's `--check` distinguishes "the leaf moved" from "the reference was
  never faithful".

## References

- `docs/architecture/curve-stableswap-reference.md` — the Curve variant axes and
  the deployed solvers the Curve leaf ports.
- `docs/architecture/balancer-contract-reference.md` — the pinned vendored
  Balancer sources (`FixedPoint` / `LogExpMath` / `WeightedMath` / `StableMath`)
  and the two deployed pow generations.
- ADR-020 — the Tier-3 on-chain accuracy oracle, the live-bytecode complement to
  these frozen ones.
- ADR-087 — deletion is the retirement: the retired Python reference is gone from
  the driver, and these restorations are a test oracle with no production
  standing, not a revival of the driver module.
