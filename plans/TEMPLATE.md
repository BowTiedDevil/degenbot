# Plan NNN: [Imperative Title]

## Overview

One-paragraph summary of what this plan changes and why. State the architectural
goal, not the implementation mechanism. A reader should understand the intent after
this paragraph alone.

## Problem

### Deletion test

If you deleted [the code this plan changes], what would happen? This question
reveals whether the current design is earning its keep or is a pass-through.
Answer honestly — if deletion would simplify things, the plan should remove, not
reorganize.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| [Concrete symptom] | [File/method/line] | [Why a reader or contributor suffers] |
| … | … | … |

Use rows, not prose. Each row is a testable claim — "6 instanceof branches in
`_dispatch()`" is falsifiable; "the code is messy" is not.

## Solution

### [Step 1 name]

Describe the change. Include inline code examples for non-trivial logic.

```python
# Before (or current signature)
...

# After (or new signature)
...
```

### [Step 2 name]

Continue one logical chunk per heading. Keep each step independently shippable
with a red-green test cycle.

### Design decisions

Record non-obvious choices here so reviewers don't have to infer them:

- **Decision A vs B**: Why A. [One sentence of rationale.]

## Files Involved

**Primary:**
- `path/to/changed/file.py` — [what changes]

**Secondary:**
- `path/to/touched/file.py` — [what changes]

**No change needed:**
- `path/to/untouched/file.py` — [why, e.g. "already satisfies protocol structurally"]

## Implementation Order

Numbered vertical slices. Each slice leaves the test suite green and can
ship on its own.

### Slice 1: [Name]

1. [Specific action]
2. Run: `just test-python` — expect [result]

### Slice 2: [Name]

1. [Specific action]
2. Run: `just test-python` — expect [result]

### Slice N: Validate and clean up

1. Run `just lint` + `just test-all`
2. Update `CONTEXT.md` if terminology changed
3. Remove any deprecated shims introduced during migration

## Testing

### Per-slice test runs

Each slice runs `just test-python`. If a migration requires a compatibility
period, both old and new paths must pass.

### New unit tests

List new test files or major test cases this plan requires:

```python
# tests/path/test_thing.py


def test_[behavior]():
    """[What and why]."""
    ...
```

### Integration tests

Note which existing test suites cover the changed behavior. If existing tests
already fully cover the change, say so explicitly.

## Benefits

- **[Leverage / Locality / Depth / etc.]**: [One sentence per benefit.]

Use the skill vocabulary where it fits: *leverage* (one interface, many
implementations), *locality* (related things in one place), *depth* (shallow
seam → deep seam), *seam* (injectable boundary), *adapter* (bridge between
interfaces).

## Risks

- **[Risk]**: [Mitigation.]

Be specific. "Performance regression" is vague; "one extra method call per
`get_dy()` invocation, negligible vs. invariant solve" is evaluable.

## Relationship to Other Plans

- **Plan NNN** ([title]): [Prerequisite / complementary / orthogonal / superseded-by].
  [One sentence explaining the relationship.]

List every active plan that intersects with this one. If none, say "Independent."

## Status

[ ] Slice 1: [name]
[ ] Slice 2: [name]
…
[ ] Slice N: validate and clean up
