//! Trace-helper default: with no typed `logging.trace_jsonl` override, the
//! offline-review capture lands in the run directory's `trace.jsonl`
//! installed at boot. One test body keeps the process-global default
//! serialized.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

#[test]
fn trace_capture_lands_in_the_run_default() {
    let dir = std::env::temp_dir().join(format!("degenbot-trace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let run = degenbot_runs::RunDirectory::create_in(&dir, "engine").expect("run dir");

    // No typed override: the session trace is the default.
    assert!(
        degenbot_runs::set_trace_jsonl_default(run.trace_jsonl_path().to_path_buf()),
        "first install wins in this test process"
    );
    degenbot_submission::frame_pipeline::trace_jsonl("test_kind", serde_json::json!({"k": 1}));
    let text = std::fs::read_to_string(run.trace_jsonl_path()).expect("read trace");
    let line: serde_json::Value = serde_json::from_str(text.trim()).expect("trace line is JSON");
    assert_eq!(
        line["kind"], "test_kind",
        "record landed in the session trace"
    );
    assert_eq!(line["k"], 1, "payload merged");

    let _ = std::fs::remove_dir_all(&dir);
}
