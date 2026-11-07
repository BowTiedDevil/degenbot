# ADR-096: Vendor the evm2 family under names we own until upstream publishes

**Status:** implemented (the vendored tree lands with this ADR; the deletion condition is below)


## Context

crates.io accepts only packages whose dependencies already exist on crates.io,
and `cargo publish` enforces it at packaging: a path or git dependency carrying
a `version` key is rewritten into that registry requirement and must resolve
there, while a version-less dependency outside the dev slots is rejected
outright (the measurements behind ADR-089's corrected publish-contract bullet).
Only version-less dev-dependencies are stripped.

`evm2` is not on crates.io. Upstream reserved the name (`0.0.0-reserved`,
2026-10-23) but has never uploaded code, so the engine lives only on GitHub.
The consequence is structural: the engine closure — `degenbot-simulation`,
`degenbot-arbitrage`, and every member whose retained requirements reach them —
cannot be uploaded at all, and the umbrella cannot resolve from the registry.
The promise the architecture makes for `cargo add degenbot` is unsatisfiable
while the engine is GitHub-only, by any declaration shape.

Three ways out were on the table: withhold the closure from releases (honest,
but the promise stays broken), hold all releases until upstream publishes
(undated), or take the engine in-house.


## Decision

**Vendor the family.** `rust/vendor/` carries a verbatim source copy of
alloy-rs/evm2 at a pinned rev, package-renamed into `degenbot-evm2*` so the
family uploads with the workspace. The dependency keys stay upstream's
(`evm2`, `evm2-jit`, …) through `package =` renames, so no `use evm2::…` or
`X.workspace = true` spelling changes anywhere; the single wiring point is the
family block in `rust/Cargo.toml`'s `[workspace.dependencies]`. The tree's own
README records what is transformed (manifests only: names, lockstep version,
materialized dep specs, test targets off, the fork's default features drop the
native precompile backends) and what stays byte-identical (every source file).

**Deletion condition.** The day upstream publishes `evm2` to crates.io: point
the family block at the registry release, delete `rust/vendor/`, and drop its
names from the membership expectation, the capability catalog row, and the
comment-hygiene exclusion. Deletion is the complete retirement (ADR-087) —
nothing is built to keep the fork gone after that.


## Consequences

- All 46 workspace members package and upload; `cargo add degenbot` resolves
  from crates.io again, and the Python track is untouched (it builds from
  source either way).
- The fork is upstream's code under our release cadence: re-vendoring at a new
  upstream rev is a copy, and `rustfmt.toml` in the tree keeps the source
  byte-identical so those copies stay diffable.
- Upstream's in-crate test modules compile against the dev-deps they import but
  never run (`test = false`): our parity suites, Tier-3 byte-exactness gates and
  the live-node harnesses remain the behavioral authority.
- The publish-blocked census (docs/gates.md section 7) now records an empty
  population. It stays: it is the gate that keeps a git-only dependency from
  silently re-entering a publishable member's manifest.
- ADR-089's clause 2 (dependency tracking) is superseded in practice by this
  tree: the family is pinned and vendored rather than tracked at git HEAD.
