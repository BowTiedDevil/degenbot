# KL2EZ3 result — suite noise tripwire

## What shipped

- `scripts/pytest_noise_gate.sh` (NEW, executable): the tripwire. `run [pytest args...]` captures the whole child-process output (`2>&1 | tee`), reads the suite exit from `PIPESTATUS[0]` SEPARATELY from the scan, then scans the capture for `--- Logging error ---` and `Reconnection attempt`, printing every offending line with its line number. Exit 0 clean, 3 on a signature, the suite's own nonzero code propagated otherwise, 2 on usage. `scan <logfile>` scans an existing capture (the negative-probe entry point).
- `justfile`: `test-python-noise-gate kw="" *args:` — a pure addition (the existing `test-python` recipe is untouched). The single positional is the pytest keyword filter, quoted as one just argument; trailing tokens forward verbatim. The recipe comment documents the negative probe and the CI wiring.
- `.github/workflows/ci.yml`: the python job's "Run Tests" step now runs `just test-python-noise-gate` (same suite plus the scan).

## Why recipe-level

Post-session output never reaches pytest plugins — the historical spray printed to the real stdout/stderr after the session ended while the exit code stayed 0. The scan must live at the process/recipe level.

## PM review (all runs re-executed by the PM)

- Negative probe: a doctored log with both signatures trips rc 3 with the offending lines printed.
- Green run: `just test-python-noise-gate 'not readme'` — 3085 passed, 35 skipped, rc 0, "clean capture (suite exit 0)".
- Exit-code separation (criterion 2): the full-suite run exits 2 (the concurrent foreign stream's README sybil breakage) with "clean capture (suite exit 2)" — a genuine failure is not masked by a clean scan.
- Worker surface verified as exactly: the script, the justfile addition, the CI step. Zero spray signatures in every capture.
