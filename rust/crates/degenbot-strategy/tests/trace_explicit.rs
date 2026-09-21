//! Typed trace override: a configured `logging.trace_jsonl` wins over the
//! run directory's `trace.jsonl` installed at boot. This test installs the
//! typed config, so it lives in its own binary (the holder is first-wins
//! process-global).

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail loudly on an unconstructible prerequisite"
)]

#[test]
fn typed_trace_override_wins_over_the_run_default() {
    let dir = std::env::temp_dir().join(format!("degenbot-trace-explicit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let run = degenbot_runs::RunDirectory::create_in(&dir, "engine").expect("run dir");

    assert!(
        degenbot_runs::set_trace_jsonl_default(run.trace_jsonl_path().to_path_buf()),
        "first install wins in this test process"
    );
    // Seed the session trace BEFORE the override is installed, so the
    // capture provably stops appending to it once the typed override lands.
    degenbot_strategy::frame_pipeline::trace_jsonl("run_default", serde_json::json!({"k": 0}));
    let run_before = std::fs::read_to_string(run.trace_jsonl_path()).expect("read run trace");

    let explicit = dir.join("explicit.jsonl");
    let mut cfg = degenbot_config::BotConfig::default();
    cfg.logging.trace_jsonl = Some(explicit.clone());
    assert!(
        degenbot_config::holder::install(std::sync::Arc::new(cfg)),
        "first install wins in this test process"
    );

    degenbot_strategy::frame_pipeline::trace_jsonl("test_kind", serde_json::json!({"k": 2}));
    let explicit_text = std::fs::read_to_string(&explicit).expect("read explicit trace");
    let explicit_line: serde_json::Value =
        serde_json::from_str(explicit_text.trim()).expect("trace line is JSON");
    assert_eq!(explicit_line["k"], 2, "typed capture wins");
    assert!(
        !explicit_text.contains("run_default"),
        "the typed sink is not appended to with pre-override records: {explicit_text}"
    );
    let run_text = std::fs::read_to_string(run.trace_jsonl_path()).expect("read run trace");
    assert_eq!(
        run_text, run_before,
        "the session trace is untouched once the typed override is installed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
