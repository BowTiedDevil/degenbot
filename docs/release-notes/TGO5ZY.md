TGO5ZY — completed 2026-08-20 (fix commit 8bfa983e6).
G1 (cargo publish --workspace --dry-run --allow-dirty, just publish-dry-run)
GREEN: 26/26 publishable crates packaged + verification-built in dependency
order, ending with the degenbot umbrella; 0 error lines in the full log.
G2 (check-no-pyo3-in-cores) green.
Fix: degenbot-uniswap include_str! escaping the crate boundary — vendored
byte-identical src/deployments.json mirror, embed re-pointed, just test-rust
cmp gate, just publish-dry-run one-command oracle (handoff §3.1/T1 option A).
Key finding: per-crate cargo publish --dry-run -p CANNOT pass pre-publish for
the non-leaf crates (path deps are rewritten to crates.io versions and resolved
against the registry); the workspace-level form resolves inter-member deps in
the workspace graph and is the correct gate (astral uv/ruff pattern). See
docs/handoffs/crates-io-publishing-prep.md.
