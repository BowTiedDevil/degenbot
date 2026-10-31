#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! The production executor's AOT prep lane.
//!
//! Compiles the injected `cmd_executor` runtime bytecode once, into the
//! durable artifact store the process-shared JIT holder opens
//! (`<persistence.state_dir>/aot-artifacts`), and proves the artifact loads
//! back as `ProgramKind::Aot`. Every later process then executes the
//! executor's compiled program from its first simulated candidate instead
//! of paying the compile stall on the hottest contract in the system.
//!
//! The artifact is `contracts/cmd_executor_runtime_bytecode.txt` — the
//! PRODUCTION runtime with its five immutables baked in (the code the sim
//! injects verbatim), not the tier-3 oracle's test artifact. The code hash
//! the runner keys its lookup on is `keccak256(injected bytecode)`, so this
//! lane and the runtime agree on the key by construction.
//!
//! Idempotent: the store writes through a temporary sibling and a rename, so
//! a re-run replaces the artifact cleanly.
//!
//! ```sh
//! cargo run --release -p degenbot-simulation --example aot_prepare_executor \
//!   --features evm2-jit -- [artifact-path]
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{keccak256, Bytes};

use ::evm2_jit::runtime::{AotRequest, ArtifactStore, JitBackend, ProgramKind, RuntimeConfig};

use degenbot_simulation::sim::evm2::aot_store::{default_artifact_dir, DurableArtifactStore};
use degenbot_simulation::sim::evm2::handle::EVM2_PRODUCTION_SPEC;

/// The production executor runtime artifact, repo-relative.
const EXECUTOR_RUNTIME_ARTIFACT: &str = "contracts/cmd_executor_runtime_bytecode.txt";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

/// The artifact path: `argv[1]` when given, else the repo's production artifact.
fn artifact_path(override_path: Option<String>) -> PathBuf {
    override_path.map_or_else(
        || repo_root().join(EXECUTOR_RUNTIME_ARTIFACT),
        PathBuf::from,
    )
}

/// Read a hex-text artifact (`0x`-prefixed or bare).
fn load_hex(path: &Path) -> Vec<u8> {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let text = raw.trim();
    let text = text.strip_prefix("0x").unwrap_or(text);
    alloy::hex::decode(text).unwrap_or_else(|e| panic!("hex-decode {}: {e}", path.display()))
}

fn main() {
    let path = artifact_path(std::env::args().nth(1));
    let path = path.canonicalize().unwrap_or(path);
    let code = load_hex(&path);
    let code_hash = keccak256(&code);
    let store_dir = default_artifact_dir();

    let store = Arc::new(
        DurableArtifactStore::new(&store_dir)
            .unwrap_or_else(|e| panic!("open artifact store {}: {e}", store_dir.display())),
    );
    let backend = JitBackend::new(RuntimeConfig {
        enabled: true,
        store: Some(store.clone()),
        ..Default::default()
    })
    .expect("jit backend init");

    let preloaded = backend
        .get_compiled(code_hash, EVM2_PRODUCTION_SPEC)
        .is_some();

    let prep = Instant::now();
    backend
        .prepare_aot_sync(AotRequest {
            code_hash,
            code: Bytes::from(code.clone()),
            spec_id: EVM2_PRODUCTION_SPEC,
        })
        .expect("prepare_aot_sync");
    let prep_time = prep.elapsed();

    let program = backend
        .get_compiled(code_hash, EVM2_PRODUCTION_SPEC)
        .unwrap_or_else(|| panic!("no compiled program for {code_hash:x}"));
    let (artifact_key, artifact_len, object_path) = store
        .load_all()
        .expect("list the artifact store")
        .into_iter()
        .find(|(key, _)| {
            key.runtime.code_hash == code_hash && key.runtime.spec_id == EVM2_PRODUCTION_SPEC
        })
        .map_or_else(
            || panic!("the store holds no artifact for {code_hash:x}"),
            |(key, stored)| (key, stored.manifest.artifact_len, stored.object_path),
        );

    println!("aot-prepare-executor");
    println!("  artifact:     {}", path.display());
    println!("  bytecode:     {} bytes", code.len());
    println!("  code hash:    0x{code_hash:x}");
    println!("  spec:         {EVM2_PRODUCTION_SPEC:?}");
    println!("  store:        {}", store.dir().display());
    println!("  artifact key: {artifact_key:?}");
    println!(
        "  object:       {} ({} bytes)",
        object_path.display(),
        artifact_len
    );
    println!("  proof:        get_compiled -> {:?}", program.kind);
    println!(
        "  prep time:    {} (already persisted: {})",
        human_time(prep_time),
        if preloaded { "yes" } else { "no" }
    );

    if program.kind != ProgramKind::Aot {
        eprintln!(
            "FAILED: get_compiled returned {:?}, not ProgramKind::Aot",
            program.kind
        );
        std::process::exit(1);
    }
}

/// Milliseconds with one decimal, the unit a one-shot build cost reads in.
fn human_time(elapsed: Duration) -> String {
    format!("{:.1} ms", elapsed.as_secs_f64() * 1_000.0)
}
