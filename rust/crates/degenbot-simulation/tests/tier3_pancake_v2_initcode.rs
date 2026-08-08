//! Toolchain-free provenance guard for the PINNED PancakeSwap V2 pair bytecode
//! (Tier-3 oracle, `tier3-oracle/artifacts/PancakeV2Pair/PancakeV2Pair.json`).
//!
//! The oracle deploys the pair from this committed on-chain creation bytecode
//! (raw `create`, see `tier3_pancake_v2_swap_vs_revm.rs`). Two invariants must
//! hold for that pin to be the genuine deployed PancakeSwap V2 pair:
//!
//! 1. **Init-code-pair hash.** `INIT_CODE_PAIR_HASH` (degenbot's
//!    `PANCAKESWAP_V2` CREATE2 identity) is `keccak256` of the pair CREATION
//!    code. This test recomputes it from the pinned creation bytecode and
//!    asserts it equals the canonical `0x57224589…` — making the init-code
//!    hash a machine-checked invariant instead of a committed constant. The
//!    source is cross-checked by the AUTHORITATIVE toolchain step in
//!    `tier3-oracle/verify-tier3-artifacts.sh`, which recompiles
//!    `sources/contracts/PancakeFactory.sol` at the pinned settings and
//!    byte-compares the creation code to this artifact (then `keccak` here
//!    closes the loop to `0x57224589…`).
//!
//! 2. **Embedded metadata (bzzr1) hash.** The 32-byte swarm metadata hash that
//!    a local CLI recompile could never reproduce is embedded in the bytecode
//!    tail (immediately before the `dsolc` marker). Asserting it equals the
//!    canonical on-chain value `0x361d24ef…` pins down that previously
//!    irreducible 32-byte metadata — so a source/build drift that changes it
//!    fails loudly.

#![allow(clippy::doc_markdown)]
#![expect(clippy::expect_used, clippy::panic)]
use std::path::PathBuf;

use alloy::hex;
use alloy::primitives::{keccak256, B256};
use serde_json::Value;

/// Canonical PancakeSwap V2 `INIT_CODE_PAIR_HASH` — `keccak256` of the pair
/// creation code, the value degenbot's `DexVariant::PancakeswapV2` derives
/// CREATE2 addresses from (factory `0x1097053F…`, mainnet).
const INIT_CODE_PAIR_HASH: &str =
    "0x57224589c67f3f30a6b0d7a1b54cf3153ab84563bc609ef41dfb34f8b2974d2d";
/// Canonical 32-byte swarm/bzzr1 metadata hash embedded in the deployed
/// runtime tail (before the `dsolc` version marker) — the value that a CLI
/// recompile at otherwise-correct settings could not reproduce.
const EMBEDDED_METADATA_HASH: &str =
    "0x361d24efe9bc43b2f8ddececbe5ea7e170556d840eab342be622f3e57ca7317a";
/// Hex of the trailing solc metadata-version marker (`dsolc`) that delimits
/// the appended CBOR metadata in both creation and deployed bytecode.
const DSOLC_MARKER: &str = "64736f6c63";

/// `tier3-oracle/` under the repo root (this crate + three up).
fn oracle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("tier3-oracle")
        .canonicalize()
        .expect("canonicalize tier3-oracle")
}

/// Load the pinned pair artifact JSON.
fn load_artifact() -> Value {
    let path = oracle_root().join("artifacts/PancakeV2Pair/PancakeV2Pair.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing pinned pair artifact {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("valid pinned pair JSON")
}

/// `keccak256(pinned pair creation code) == the canonical INIT_CODE_PAIR_HASH`.
///
/// The pinned `bytecode.object` is the deployable init code the oracle
/// raw-`create`s; hashing it must equal `0x57224589…` (this is what makes
/// CREATE2-derived addresses on mainnet reproducible).
#[test]
fn pinned_pair_creation_code_hits_init_code_pair_hash() {
    let v = load_artifact();
    let obj = v["bytecode"]["object"]
        .as_str()
        .expect("creation bytecode.object");
    let bytes = hex::decode(obj.trim_start_matches("0x")).expect("hex creation bytecode");
    let got = keccak256(&bytes);
    let expected: B256 = INIT_CODE_PAIR_HASH
        .parse()
        .expect("const init-code hash hex");
    assert_eq!(
        got, expected,
        "keccak256(pinned pair creation code) must equal the PancakeSwap V2 INIT_CODE_PAIR_HASH 0x57224589…"
    );

    // The artifact's own provenance must agree (self-describing pin).
    let prov = v["provenance"]["initCodeHash"]
        .as_str()
        .expect("provenance.initCodeHash");
    assert_eq!(
        prov.to_lowercase(),
        INIT_CODE_PAIR_HASH,
        "artifact provenance.initCodeHash must match the canonical value"
    );
}

/// The deployed runtime embeds the canonical 32-byte bzzr1 metadata hash.
///
/// Asserts the 32 bytes immediately before the last `dsolc` (`0x64736f6c63`)
/// marker in the pinned runtime bytecode equal the canonical on-chain metadata
/// hash — pinning the previously-irreproducible metadata value so any
/// source/settings drift on a rebuild is caught.
#[test]
fn pinned_pair_runtime_embeds_canonical_metadata_hash() {
    let v = load_artifact();
    let deployed = v["deployedBytecode"]["object"]
        .as_str()
        .expect("runtime bytecode.object");
    let hex_s = deployed.trim_start_matches("0x").to_lowercase();
    let idx = hex_s
        .rfind(DSOLC_MARKER)
        .expect("deployed runtime must carry the solc metadata marker (dsolc)");
    assert!(idx >= 64, "bzzr1 hash must precede the dsolc marker");
    let meta_hex = &hex_s[idx - 64..idx];
    let expected: B256 = EMBEDDED_METADATA_HASH
        .parse()
        .expect("const metadata hash hex");
    assert_eq!(
        meta_hex,
        hex::encode(expected),
        "embedded bzzr1 metadata hash must equal the canonical on-chain value 0x361d24ef…"
    );
}
