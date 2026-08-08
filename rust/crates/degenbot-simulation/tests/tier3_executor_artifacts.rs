//! Toolchain-free integrity guard for the Vyper executor artifact (BHL2R2 /
//! tier-3b deterministic revm-replay oracle).
//!
//! The tier-3b executor tests (S2/S3, tasks 4O7BPZ/72YZXI) run in the default
//! `cargo test --workspace` suite and load the canonical executor bytecode from
//! the COMMITTED `tier3-oracle/artifacts/executor/` tree, so no vyper is needed
//! to RUN the suite. This test closes the drift hole that committed-binary
//! decision opens: it recomputes the sha256 of the git-tracked
//! `tier3-oracle/src-executor/cmd_executor.vy` and compares against the manifest
//! recorded in `artifacts/executor/manifest.json`, and asserts each committed
//! artifact is present (with non-empty hex bytecode for the `.hex` files).
//! A tracked source edit without a rebuild+re-publish fails here.
//!
//! The AUTHORITATIVE compile-vs-use check (requires the real vyper 0.5.0a3
//! toolchain) is `tier3-oracle/verify-tier3-executor-artifact.sh` — it runs in
//! the CI `tier3-oracle` job. The default cargo-test path needs no toolchain.
//!
//! NOTE (found): vyper (venom AND legacy) emits only a coarse whole-file-span
//! source map with NO per-instruction line attribution, so S2 cannot map a
//! halt PC to a `cmd_executor.vy` line via the source map. It must attribute
//! via `cmd_executor.error_map.json` (arithmetic-revert PCs) + method-
//! delegation + direct source inspection (recorded on task 4O7BPZ).

#![expect(clippy::expect_used, clippy::panic)]
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Repo root = this crate (rust/crates/degenbot-simulation) + three up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("canonicalize repo root")
}

/// The oracle root: `tier3-oracle/` under the repo root.
fn oracle_root() -> PathBuf {
    repo_root().join("tier3-oracle")
}

/// sha256 hex of a file's bytes.
fn sha256_hex(path: &Path) -> String {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Every committed executor artifact must be present, match the sha256 of its
/// git-tracked source recorded in the manifest, and (for `.hex`) carry non-empty
/// EVM bytecode.
#[test]
fn committed_executor_artifacts_match_tracked_source() {
    let root = oracle_root();
    let manifest_path = root.join("artifacts/executor/manifest.json");
    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("missing {}: {e}", manifest_path.display()));
    let manifest: Value = serde_json::from_str(&manifest_raw)
        .unwrap_or_else(|e| panic!("invalid manifest {}: {e}", manifest_path.display()));
    let map = manifest["artifacts"]
        .as_object()
        .unwrap_or_else(|| panic!("manifest missing `artifacts` object"));

    assert!(
        !map.is_empty(),
        "executor manifest {} lists no artifacts — run tier3-oracle/build-tier3-executor-harness.sh",
        manifest_path.display()
    );

    for (artifact, meta) in map {
        let expected_src = meta
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("manifest entry {artifact} missing `source`"));
        let expected_hash = meta
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("manifest entry {artifact} missing `sha256`"));

        // 1. The git-tracked source must be present and hash to the manifest
        //    value — a tracked source edit without a rebuild fails here.
        let source_path = root.join(expected_src);
        let actual_hash = sha256_hex(&source_path);
        assert_eq!(
            actual_hash,
            expected_hash,
            "executor source {} changed but artifacts/executor/manifest.json was not refreshed. \
             Re-run tier3-oracle/build-tier3-executor-harness.sh (PUBLISH=1) and commit the \
             updated artifact + manifest.",
            source_path.display()
        );

        // 2. The committed artifact must exist. Bytecode hex must be even-length
        //    and non-empty.
        let artifact_path = root.join("artifacts").join(artifact);
        let raw = std::fs::read_to_string(&artifact_path).unwrap_or_else(|e| {
            panic!(
                "missing committed artifact {}: {e}",
                artifact_path.display()
            )
        });
        let is_hex = Path::new(artifact)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("hex"));
        if is_hex {
            let hex = raw.trim();
            assert!(
                !hex.is_empty() && hex.len().is_multiple_of(2),
                "{} has empty/non-hex creation bytecode",
                artifact_path.display()
            );
            assert!(
                hex.chars().all(|c| c.is_ascii_hexdigit()),
                "{} contains non-hex chars",
                artifact_path.display()
            );
        } else if artifact.contains(".sol/") {
            // Foundry-shaped Solidity harness JSON: the committed bytecode.object
            // must be present and valid EVM creation code.
            let v: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("{} is invalid json: {e}", artifact_path.display()));
            let obj = v["bytecode"]["object"].as_str();
            assert!(
                obj.is_some(),
                "{} has no bytecode.object (foundry shape)",
                artifact_path.display()
            );
        }
    }
}
