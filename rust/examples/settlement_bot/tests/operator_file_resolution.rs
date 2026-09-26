#![expect(
    clippy::expect_used,
    reason = "the ADR-062 acceptance probe must fail loudly when the binary or the operator file cannot be prepared"
)]
//! ADR-062 acceptance: the pure-Rust example resolves its endpoints and its
//! database path from ONE operator file, with no RPC environment variables.
//!
//! The example's former hand-rolled cascade read `[rpc]`/`[ws]`/`database.path`
//! raw; this asserts the file's typed `[nodes]`/`[database]` tables drive the
//! shared `degenbot::config` resolvers instead, including an `ipc` entry naming
//! a local socket.

use std::path::PathBuf;
use std::process::Command;

const DB_REL: &str = "../../crates/foundation/degenbot-db/tests/fixtures/parity.db";

fn db_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DB_REL)
}

#[test]
fn endpoints_and_db_resolve_from_the_operator_file_ipc_entry() {
    let dir = std::env::temp_dir().join(format!(
        "settlement-operator-file-{}-{}",
        std::process::id(),
        db_path()
            .file_name()
            .expect("fixture file name")
            .to_string_lossy()
    ));
    std::fs::create_dir_all(&dir).expect("create temp operator dir");
    let socket = dir.join("anvil.ipc");
    let operator = dir.join("config.toml");
    // `{:?}` renders a quoted, escaped TOML string for each path.
    std::fs::write(
        &operator,
        format!(
            "[database]\npath = {:?}\n\n[nodes]\nipc = {{ 1 = {:?} }}\n",
            db_path().display().to_string(),
            socket.display().to_string(),
        ),
    )
    .expect("write operator file");

    let mut command = Command::new(env!("CARGO_BIN_EXE_degenbot-settlement-bot-example"));
    command.env_clear();
    command.env("HOME", std::env::temp_dir());
    command.env("PATH", std::env::var("PATH").unwrap_or_default());
    // The file is the endpoint + database source: no DEGENBOT_RPC_* /
    // DEGENBOT_DB_PATH exports. DEGENBOT_CONFIG only POINTS at the file.
    command.env("DEGENBOT_CONFIG", &operator);
    // The committed chain-8453 fixture is Alembic-head-stamped; pin the heal
    // killswitch so this read-only probe never rewrites the shared file.
    command.env("DEGENBOT_DB_AUTO_HEAL", "0");
    command.env("DEGENBOT_DISCOVERY_CHAIN_ID", "8453");
    command.env("DEGENBOT_STRATEGY_SETTLEMENT_ACTIVE", "1");
    command.env("DEGENBOT_METRICS_ADDR", "127.0.0.1:0");
    command.arg("--smoke-offline");
    let output = command.output().expect("spawn the settlement-bot example");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "offline boot failed; stdout={stdout} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let socket = socket.display().to_string();
    assert!(
        stdout.contains(&format!("http={socket}")),
        "request endpoint did not resolve from the file ipc entry: {stdout}"
    );
    assert!(
        stdout.contains(&format!("ws={socket}")),
        "subscription endpoint did not resolve from the file ipc entry: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
