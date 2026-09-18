//! Trace-helper default: with `SIDECAR_TRACE_JSONL` absent, the offline-review
//! capture lands in the run directory's `trace.jsonl` installed at boot; an
//! explicit override still wins. One test body keeps the process-global
//! default and the process env mutation serialized.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

#[test]
fn trace_capture_prefers_the_explicit_override_then_the_run_default() {
    let dir = std::env::temp_dir().join(format!("degenbot-trace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let run = degenbot_runs::RunDirectory::create_in(&dir, "engine").expect("run dir");

    // Absent override: the session trace is the default.
    std::env::remove_var("SIDECAR_TRACE_JSONL");
    assert!(
        degenbot_runs::set_trace_jsonl_default(run.trace_jsonl_path().to_path_buf()),
        "first install wins in this test process"
    );
    degenbot_submission::frame_pipeline::trace_jsonl("test_kind", serde_json::json!({"k": 1}));
    let default_text = std::fs::read_to_string(run.trace_jsonl_path()).expect("read trace");
    assert!(
        default_text.contains("\"kind\":\"test_kind\""),
        "record landed in the session trace: {default_text}"
    );
    assert!(
        default_text.contains("\"k\":1"),
        "payload merged: {default_text}"
    );

    // Explicit override: it wins and the session trace stays untouched.
    let explicit = dir.join("explicit.jsonl");
    std::env::set_var("SIDECAR_TRACE_JSONL", &explicit);
    degenbot_submission::frame_pipeline::trace_jsonl("test_kind", serde_json::json!({"k": 2}));
    let explicit_text = std::fs::read_to_string(&explicit).expect("read explicit trace");
    assert!(
        explicit_text.contains("\"k\":2"),
        "explicit capture wins: {explicit_text}"
    );
    assert!(
        !explicit_text.contains("\"k\":1"),
        "override is not appended to: {explicit_text}"
    );
    let run_text = std::fs::read_to_string(run.trace_jsonl_path()).expect("read run trace");
    assert_eq!(
        run_text, default_text,
        "the session trace is untouched once an explicit path is set"
    );

    std::env::remove_var("SIDECAR_TRACE_JSONL");
    let _ = std::fs::remove_dir_all(&dir);
}
