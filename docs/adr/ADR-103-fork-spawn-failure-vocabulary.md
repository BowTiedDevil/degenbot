# ADR-103: The fork spawn-failure vocabulary — cause-named at the seam, typed across the FFI

**Status: accepted (implemented).** Sharpens
[ADR-013](ADR-013-ffi-seam-is-private.md)'s typed-boundary principle for one
surface: why an anvil spawn failed. The code is the last word on the exact
cause names; this records why the cause is a product of the core crate and a
Python type, not a rendered sentence.

## Context

`degenbot-fork` spawns anvil as a subprocess and knows why a spawn failed: the
program never ran, the child died before readiness, the readiness deadline
expired, or the readiness wait broke. Four layers must agree on that answer —
and only the first one had it:

- `ForkError::spawn_failure` classified the node binding's failure shapes into
  operator causes, but the FFI mapper flattened every variant to one
  `RuntimeError`, the Python shell re-wrapped RPC failures in
  `AnvilError(method, error)` sentences, and the fork lane's retry helper
  decided retry-vs-give-up from an upstream probe over a bare `RuntimeError`.
  The classification's product was consumed by nobody: deleting it would have
  changed error wording only.
- The readiness bucket silently conflated six distinguishable binding shapes
  behind one sentence, and leaked the binding's own error enum into this
  repository's public interface — a type whose shape already differs between
  alloy 2.5 and 2.6.
- The retry policy treated every failure as potentially the upstream's: a
  missing `anvil` burned the retry budget sleeping, while a child exit during a
  healthy upstream — the fork's own problem — looked like the transient one.
- The tests reached their stand-in programs through `#[cfg(test)]` knobs on the
  production builder (`anvil_program`, `spawn_deadline_ms`) and wrote
  executables at runtime (the `ETXTBSY` class of failure), while the retry
  fixture fabricated the child-exit display string to drive a policy that
  nominally "classifies on evidence, never the message text".

## Decision

### D1 — The cause is this repository's vocabulary, classified at the seam

`ReadinessCause` (`NoStdout`, `StdoutRead`, `ExitReap`, `BannerParse`,
`Unrecognized`) names the readiness failures; the binding's error type is
converted at the spawn seam and never forwarded, riding the variant as an
opaque source. A shape the classification does not know lands in
`Unrecognized` — loud, by name, with the binding's text preserved — so an
upstream change reads as a report request rather than another cause's operator
response. The docs state mechanisms, not stories: a dead child is stdout EOF
plus a reaped exit status (never the geth-only `GenesisError`), and an expired
deadline ends with the binding killing the child.

### D2 — The cause survives the FFI as a type

Three `create_exception!` types — `AnvilSpawnError`, `AnvilChildExitError`,
`AnvilSpawnTimeoutError` — are registered on the `_ffi` root's imperative
registration island (a declarative submodule has no init hook) and re-exported
from `degenbot.exceptions`, following the typed-FFI-exception precedent
(`ChainMismatchError`, `VerificationMismatchError`). A driver branches on what
to do — fix the environment, read the child's stderr, widen the deadline — by
type; no wrapper classes and no message parsing. Everything else (the
transport runtime, the IPC connect, a dev-RPC error) stays a `RuntimeError`
carrying the cause's own text.

### D3 — The retry rule is an allowlist over the typed cause, default-deny

A spawn failure gets a second attempt only when its typed cause is
`AnvilChildExitError` — a child killed by its upstream before readiness, the
one failure that can differ on the next attempt once the endpoint answers
again. Everything else propagates at the first attempt: a program that cannot
run is the environment's; a spent deadline (`AnvilSpawnTimeoutError`) is the
budget's — one attempt already cost the whole fork spawn deadline, so a second
would run past the suite's per-test budget; a readiness fault, or a failure the
family does not name, is a fact about the fork or this code. Retrying those
spends budget and can hide the fork's real failure behind the upstream's
answer.

Eligibility is never automatic: the re-probe (the endpoint must still be
unhealthy) and a cost gate (a failed spawn that spent past
`SPAWN_RETRY_DEADLINE_S`) each independently refuse a second attempt. The rule
lives with the policy that consumes it — the fork lane's one catch — and is
pinned by tests on both the eligible and the refused side. The core carries the
vocabulary, not the verdict: there is no `is_retryable()` on `ForkError`,
because no core caller retries a spawn (see D6).

### D4 — The spawn shape owns its knobs

`program` is a production builder knob (a pinned build, a wrapper, or the
committed stand-ins), replacing the test-only field. The readiness budget
derives from the spawn shape alone (forked: the upstream-latency budget;
in-memory: the binding's own default), and one seam — `spawn_node` — applies it
to both the wait and the failure report, so a diagnostic can never name a
deadline the wait did not run under. `spawn_deadline_ms` retires with it.

### D5 — The anchors pin the binding's production behavior

The stand-in programs are committed fixtures (`tests/fixtures/*.sh`, exec bit
tracked). Nothing opens them for writing, so no process can hold a write
descriptor on them and the `ETXTBSY` class of failure cannot exist. One anchor
per real-world class — a dead child (`Fatal`), an unrunnable program
(`SpawnError`), a child that never reports readiness (`Timeout`) — is what
detects a binding bump demoting one of those shapes: the loud `Unrecognized`
path names it instead of silently re-bucketing.

### D6 — The retry policy stays a test-lane composition

No production spawn-retry entry ships. Such an entry would have to own the
upstream probe, the attempt count, and the budget story — and the operator's
endpoint declaration (`tests.env`) lives in the test lane, not in a core the
bot ships. Deferred, not rejected: re-open when a production caller spawns
forks (the entry and the core's eligibility verdict land together) or when a
second retrying consumer appears.

`ForkError::Spawn` keeps one `io::Error`, errno and all. The `ETXTBSY` class is
fixed at its root (D5), and any remaining transient spawn fault is bounded by
the cost gate to one extra attempt. Splitting the variant into
transient/permanent io kinds would be vocabulary with no consumer — and
`std::io::ErrorKind` does not name the interesting kinds with confidence on
every platform this workspace compiles for.

## Consequences

- The classification is one total pure table over every shape the binding's
  anvil spawn path produces, plus the shapes only the geth bindings (or no code
  at all) produce. Callers match our variants; the binding's enum is
  implementation.
- The retry rule is one catch and a default-deny, tested on both sides — the
  vocabulary's consumer proves the classification is a product, not wording.
- The public builder carries a real capability instead of two test knobs, and
  downstream crates' tests can reach the same seam.
- A binding upgrade that changes a failure shape surfaces as a named
  `Unrecognized` error (and a failing anchor), not as a different operator
  response.

## Alternatives considered

- **Forward the binding's error type and match it in Python.** Rejected: the
  caller's contract would be an upstream enum this repository does not own,
  and the match would live in the driver instead of at the seam that knows the
  spawn shape.
- **Keep one `RuntimeError` and parse the sentence.** Rejected: that is the
  stringly coupling the fixture's docstring already apologized for, and it
  fails silently on a wording change.
- **Default-allow: retry any spawn failure a re-probe can blame on the
  upstream.** Rejected: it is what the first policy version did, and a failure
  the family does not name then burns the probe budget and re-raises the
  upstream's unavailability over the fork's real failure — the exact shape a
  red test caught before the rule was named.

- **Python-side wrapper classes under `AnvilError`.** Rejected: the repository's
  precedent for FFI-raised causes is a typed exception re-exported from
  `degenbot.exceptions`, and a second hierarchy would split one vocabulary in
  two.

## Migration

None outside the fork surface. The Python spawn failures change shape from
`RuntimeError` to its typed subclasses (same base), so `except RuntimeError`
callers are unaffected; callers that want the cause import the family from
`degenbot.exceptions`. The builder's `anvil_program`/`spawn_deadline_ms` test
knobs are gone (`program` is the production knob).
