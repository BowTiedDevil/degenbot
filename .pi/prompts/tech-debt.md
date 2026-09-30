---
description: Scan the repository for technical debt
argument-hint: "[path]"
---

Perform a thorough survey of ${1:-the project code} for technical debt:

## Code
- slightly different data structures used at the boundary between related operations that require manipulation
- reimplementing functionality available in a dependency
- detailed comments describing temporal sequencing that should be handled by a helper or setup function
- similar code found in multiple modules
- modules with too many responsibilities
- external drivers (e.g. Python) that reimplement functionality already in the Rust core
- string-based checks that could be an enum
- inputs checked against explicitly permitted types or values, but the unmatched case fails silently instead of raising an exception or returning an error

## Tests
- using arbitrary timing thresholds as a proxy for sequencing, ordering, concurrency, or actual behavior
- tautological tests that never fail
- global data used across multiple tests
- monkeypatching a production object instead of using a test fake against its interface
- complex setup that should be extracted to a fixture or helper
- enforcing an implementation detail of a dependency
- relying on access to an idempotent external service instead of a golden capture
- modules that could be easily tested by refactoring to use dependency injection

## Comments
- references to ephemeral tasks, epics, sprints, refactors, etc., e.g. 'checkpoint A', 'phase 1', 'slice B2'
- stale references to an unknown document, e.g. '§4.2 byte-divergence'

## Names
- tersely named functions, methods, data structures, and variables
- generic or vague names with a comment clarifying their real purpose

Record findings in a new file under `.scratch/` (e.g. `.scratch/tech-debt-<topic>.md`), grouped by category and ranked by payoff.
