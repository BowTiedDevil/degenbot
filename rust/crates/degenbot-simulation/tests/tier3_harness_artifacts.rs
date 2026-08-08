//! Toolchain-free integrity guard for the Tier-3 on-chain oracle artifacts.
//!
//! The Tier-3 revm tests run in the default `cargo test --workspace` suite and
//! load their canonical-reference harness bytecode from the COMMITTED
//! `tier3-oracle/artifacts/` tree (so no solc/forge is needed to RUN the
//! suite). This test closes the drift hole a committed-binary decision opens:
//! it recomputes the sha256 of each git-tracked harness source `.sol` and
//! compares against the manifest recorded in `artifacts/manifest.json`, and
//! asserts each committed artifact is present with non-empty creation bytecode.
//! If a tracked harness source is edited without a rebuild+re-publish, this
//! test fails and tells the developer to re-run the harness build script.
//!
//! The AUTHORITATIVE compile-vs-use check (which also covers the gitignored
//! pinned vendored libs) is `tier3-oracle/verify-tier3-artifacts.sh` — it
//! recompiles each harness with the real toolchain and byte-compares against
//! the committed artifacts. It runs in the CI `tier3-oracle` job (has
//! solc/forge); this source-hash test needs no toolchain.

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

/// Every committed harness artifact must be present with creation bytecode, and
/// must match the sha256 of its git-tracked source recorded in the manifest.
#[test]
fn committed_tier3_harness_artifacts_match_tracked_sources() {
    let root = oracle_root();
    let manifest_path = root.join("artifacts/manifest.json");
    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("missing {}: {e}", manifest_path.display()));
    let manifest: Value = serde_json::from_str(&manifest_raw)
        .unwrap_or_else(|e| panic!("invalid manifest {}: {e}", manifest_path.display()));
    let map = manifest
        .as_object()
        .unwrap_or_else(|| panic!("manifest root must be an object"));

    assert!(
        !map.is_empty(),
        "manifest {} lists no artifacts — run tier3-oracle/write-harness-manifest.sh",
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
        //    value — a tracked harness edit without a rebuild fails here.
        let source_path = root.join(expected_src);
        let actual_hash = sha256_hex(&source_path);
        assert_eq!(
            actual_hash,
            expected_hash,
            "tier-3 harness source {} changed but artifacts/manifest.json was not refreshed. \
             Re-run tier3-oracle/<build-tier3-*-swap-harness.sh> (PUBLISH=1) and commit the \
             updated artifact + manifest.",
            source_path.display()
        );

        // 2. The committed artifact must exist and carry creation bytecode.
        let artifact_path = root.join("artifacts").join(artifact);
        let artifact_raw = std::fs::read_to_string(&artifact_path).unwrap_or_else(|e| {
            panic!(
                "missing committed artifact {}: {e}",
                artifact_path.display()
            )
        });
        let artifact: Value = serde_json::from_str(&artifact_raw)
            .unwrap_or_else(|e| panic!("invalid artifact {}: {e}", artifact_path.display()));
        let object = artifact["bytecode"]["object"]
            .as_str()
            .unwrap_or_else(|| panic!("{} missing bytecode.object", artifact_path.display()));
        assert!(
            object.len() > 2,
            "{} has empty object (creation) bytecode",
            artifact_path.display()
        );
    }
}
