#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration test asserts on the known-valid example boot output"
)]
//! G6 telemetry acceptance for the standalone settlement-bot example
//! (ergo ZOBXVC, epic RGZG4S).
//!
//! Shells the built example against the frozen parity.db fixture and asserts
//! the pure-Rust driver now boots the same observability stack the Python
//! driver boots:
//!
//! - the telemetry prelude announces itself on stderr (console wiring, the
//!   worker-census boot table, and the Prometheus scrape endpoint),
//! - the machine-checkable stdout contract is unchanged (the RSP-8
//!   `grep '^parity-ledger row='` extraction still yields 20 rows),
//! - `warn_retired_env_names()` fires through the facade for a retired flag,
//! - an env-configured OTLP endpoint activates the span layer,
//! - the Prometheus endpoint serves real exposition text mid-run.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const DB_REL: &str = "../../crates/degenbot-db/tests/fixtures/parity.db";

fn db_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DB_REL)
}

/// The offline fixture boot command: no RPC, hermetic HOME, ephemeral metrics
/// port. Matches `boot_gate.rs`'s environment discipline so the two suites
/// cannot interfere.
fn base_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_degenbot-settlement-bot-example"));
    command.env_clear();
    command.env("HOME", std::env::temp_dir());
    command.env("PATH", std::env::var("PATH").unwrap_or_default());
    command.env("DEGENBOT_FIXTURE_DB", db_path());
    command.env("DEGENBOT_RPC_HTTP_CHAINID_1", "http://127.0.0.1:1");
    command.env("DEGENBOT_RPC_WS_CHAINID_1", "ws://127.0.0.1:1");
    command.env("DEGENBOT_DISCOVERY_CHAIN_ID", "8453");
    command.env("DEGENBOT_METRICS_ADDR", "127.0.0.1:0");
    command
}

fn run(extra_env: &[(&str, &str)]) -> (String, String) {
    let mut command = base_command();
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let output = command
        .arg("--smoke-offline")
        .output()
        .expect("spawn degenbot-settlement-bot-example");
    assert!(
        output.status.success(),
        "offline boot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("boot stdout is UTF-8"),
        String::from_utf8(output.stderr).expect("boot stderr is UTF-8"),
    )
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Poll `/metrics` until the endpoint answers, failing if the driver exits
/// first or the deadline passes.
fn scrape(addr: &str, child: &mut Child) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait on the driver child") {
            panic!("driver exited before serving /metrics: {status}");
        }
        if let Ok(mut stream) = TcpStream::connect(addr) {
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .write_all(b"GET /metrics HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .expect("write scrape request");
            let mut body = String::new();
            let _ = stream.read_to_string(&mut body);
            if body.starts_with("HTTP/1.1 200") || body.starts_with("HTTP/1.0 200") {
                return body;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "metrics endpoint never served a 200"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn offline_boot_announces_telemetry_and_keeps_the_parity_contract() {
    let (stdout, stderr) = run(&[]);

    // (a) The telemetry prelude announced itself.
    assert!(
        stderr.contains("telemetry boot complete"),
        "missing telemetry boot announcement: {stderr}"
    );
    // (b) The scrape-server thread line (mirrors the Python boot census).
    assert!(
        stderr.contains("Prometheus metrics endpoint active"),
        "missing scrape-endpoint announcement: {stderr}"
    );
    // (c) The ONE worker-census boot table (contains the metrics_scrape row).
    assert!(
        stderr.contains("boot table full") && stderr.contains("metrics_scrape"),
        "missing worker-census boot line: {stderr}"
    );

    // (d) The RSP-8 machine contract on stdout is byte-unchanged.
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("parity-ledger row="))
        .collect();
    assert_eq!(rows.len(), 20, "parity-ledger row count changed:\n{stdout}");
    assert!(stdout.contains("[boot] discovery enumerated "));
    assert!(stdout.contains("[g3] offline-dry pipeline: "));
}

#[test]
fn retired_env_name_warns_through_the_facade() {
    let (_, stderr) = run(&[("DEGENBOT_DRAIN_DBG", "1")]);
    assert!(
        stderr.contains("retired telemetry flag set"),
        "retired-env WARN missing: {stderr}"
    );
    assert!(
        stderr.contains("DEGENBOT_DRAIN_DBG"),
        "retired env name not named: {stderr}"
    );
}

#[test]
fn otlp_endpoint_env_activates_the_span_layer() {
    let (_, stderr) = run(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")]);
    assert!(
        stderr.contains("OTel OTLP span layer active"),
        "OTLP layer did not activate from the env endpoint: {stderr}"
    );
    assert!(
        !stderr.contains("telemetry boot complete") || stderr.contains("otel=active"),
        "boot announcement must report the active OTLP layer: {stderr}"
    );
}

#[test]
fn prometheus_endpoint_serves_real_metrics_mid_run() {
    let port = free_port();
    let socket = std::env::temp_dir().join(format!("settlement-telemetry-{port}.sock"));
    let mut child = base_command()
        .env("DEGENBOT_METRICS_ADDR", format!("127.0.0.1:{port}"))
        .args(["--operator-inert", "--operator-socket"])
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the inert operator driver");

    let body = scrape(&format!("127.0.0.1:{port}"), &mut child);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket);

    assert!(
        body.contains("degenbot_metric_series"),
        "scrape is missing the bot metric families:\n{body}"
    );
}
