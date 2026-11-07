# rust/vendor — the vendored evm2 family (temporary)

This tree is a **verbatim source copy** of [`alloy-rs/evm2`](https://github.com/alloy-rs/evm2)
at rev `baf0d38f3b4f971e59b4aeb51de65e44d868d703`, package-renamed into the
`degenbot-evm2*` family so the engine can be **published to crates.io**.

## Why it exists

crates.io accepts only packages whose dependencies already exist on crates.io.
`evm2` does not: upstream reserved the name (`0.0.0-reserved`) but has never
uploaded code, so it lives only on GitHub. Any of our crates that runs the
engine therefore cannot be uploaded at all — and `cargo add degenbot` cannot
resolve. Vendoring the family under names we own fixes both today.

## How it is wired

- **Names.** The packages are `degenbot-evm2`, `degenbot-evm2-jit`, … Every
  dependency key stays upstream's (`evm2`, `evm2-jit`, …) via `package =`
  renames, so all `use evm2::…` and `X.workspace = true` spellings — here and
  in our crates — are unchanged. The single wiring point is the family block in
  `rust/Cargo.toml`'s `[workspace.dependencies]`.
- **Source files are verbatim.** Only `Cargo.toml` is rewritten: package name,
  lockstep `version`/`rust-version`/`repository` with the workspace, upstream's
  `edition` (2024) / `license` / `authors` / metadata kept, dependency specs
  materialized from upstream's workspace table, and test targets off
  (`test = false`). Upstream's in-crate test modules stay in the source
  verbatim, but their dev-deps are not declared and nothing compiles them here:
  the workspace-wide lanes select the family only as a dependency
  (`scripts/vendor_members.sh` supplies the excludes). Our own parity suites
  are the behavioral gate.
- **Default features** drop upstream's native precompile backends
  (`gmp`, `bn254-mcl`, `p256-aws-lc-rs`): our build contract forbids GMP/MCL/
  aws-lc native builds. The features remain declared and unused. The family's
  `nightly` feature likewise never fires here: the workspace-wide lanes do not
  select the family as a primary package, and our toolchain is stable.
- **Layout mirrors upstream's** (`crates/…`) so the `include_str!` fixture paths
  in test modules resolve inside this tree (`rust/vendor/data/` holds the two
  fixtures they read).
- `rustfmt.toml` here sets `disable_all_formatting`, so the source stays
  byte-identical to upstream's, and `just check-vendor-tracked` asserts every
  file here is in the git index (the copy is only verbatim if it is complete).

## The switch (this tree is temporary)

The day upstream publishes `evm2` to crates.io:

1. Point the family block in `rust/Cargo.toml`'s `[workspace.dependencies]` at
   the registry release (drop the `package =` renames and `path =` keys).
2. Delete `rust/vendor/`.
3. Drop the ten names from `EXPECTED_NAMES` in
   `rust/crates/facade/degenbot/tests/architecture_gates.rs`, the catalog row in
   `docs/capabilities.md`, this tree's exclusions in
   `scripts/hooks/comment-hygiene.sh` and `prek.toml`, and the
   `scripts/vendor_members.sh` call sites.

Recorded as the deletion condition of ADR-096. Deleting the tree is the
complete retirement — no gate keeps it gone afterwards.

## License

Upstream is `MIT OR Apache-2.0`; the texts are in `LICENSE-MIT` and
`LICENSE-APACHE` here. Individual crate manifests declare the same SPDX.
