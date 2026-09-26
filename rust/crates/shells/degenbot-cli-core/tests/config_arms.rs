//! Integration tests for the `config` command arms.
//!
//! The read-only surface: `config show` (the values the operator file
//! declares), `config show --resolved` (every layer's winning value with the
//! layer that supplied it), and `config path` (the file the mutating arms
//! write). The mutating `get|set|unset` arms land on the same verb.
#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use degenbot_cli_core::{
    run, CliContext, Command, CommandReport, ConfigCommand, ConfigReport, ExitCode, PromptPlan,
    Prompter,
};
use degenbot_config::{MapEnv, NodeTransport};

struct NoPrompt {
    calls: RefCell<usize>,
}

impl NoPrompt {
    fn new() -> Self {
        Self {
            calls: RefCell::new(0),
        }
    }
}

impl Prompter for NoPrompt {
    fn confirm(&self, _message: &str, _default: bool) -> bool {
        *self.calls.borrow_mut() += 1;
        false
    }
}

fn write_config(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("cli-config-{name}-{}", std::process::id()));
    std::fs::write(&path, body).unwrap();
    path
}

fn env(pairs: &[(&str, &str)]) -> MapEnv {
    MapEnv::new(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
    )
}

fn ctx<'e>(env: &'e MapEnv, file: Option<&Path>) -> CliContext<'e> {
    let mut ctx = CliContext::new(env);
    if let Some(file) = file {
        ctx = ctx.with_config(file.to_string_lossy().into_owned());
    }
    ctx
}

/// Run one read-only arm and return its rendered lines.
fn lines(env: &MapEnv, file: Option<&Path>, command: ConfigCommand) -> Vec<String> {
    let ctx = ctx(env, file);
    let prompter = NoPrompt::new();
    let outcome = run(&Command::Config(command), &ctx, &prompter);
    assert_eq!(
        outcome.exit_code,
        ExitCode::Success,
        "the arm must succeed: {:?}",
        outcome.error().map(ToString::to_string)
    );
    assert_eq!(
        *prompter.calls.borrow(),
        0,
        "a read-only config arm never prompts"
    );
    outcome
        .report()
        .expect("a successful arm reports")
        .render_lines()
}

#[test]
fn set_writes_a_scalar_key_and_get_reads_the_resolved_value() {
    let file = write_config("set-scalar", "[session]\nchain_id = 1\n");
    let env = env(&[]);
    let context = ctx(&env, Some(&file));
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Set {
            key: "session.chain_id".to_string(),
            value: "8453".to_string(),
            force: true,
        }),
        &context,
        &prompter,
    );
    assert_eq!(
        outcome.exit_code,
        ExitCode::Success,
        "{:?}",
        outcome.error().map(ToString::to_string)
    );
    assert_eq!(*prompter.calls.borrow(), 0, "--force skips the prompt");

    let read_ctx = ctx(&env, Some(&file));
    let read_prompter = NoPrompt::new();
    let get = run(
        &Command::Config(ConfigCommand::Get {
            key: "session.chain_id".to_string(),
        }),
        &read_ctx,
        &read_prompter,
    );
    assert_eq!(
        get.exit_code,
        ExitCode::Success,
        "{:?}",
        get.error().map(ToString::to_string)
    );
    assert_eq!(*read_prompter.calls.borrow(), 0, "get never prompts");
    let lines = get.report().unwrap().render_lines();
    assert_eq!(
        line_for(&lines, "session.chain_id"),
        "session.chain_id = 8453 (file)"
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn set_without_force_prompts_and_a_decline_aborts() {
    let file = write_config("set-decline", "[session]\nchain_id = 1\n");
    let env = env(&[]);
    let context = ctx(&env, Some(&file));
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Set {
            key: "session.chain_id".to_string(),
            value: "8453".to_string(),
            force: false,
        }),
        &context,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    assert!(
        matches!(outcome.error(), Some(degenbot_cli_core::CliError::Aborted)),
        "a declined prompt is the Abort arm: {:?}",
        outcome.error().map(ToString::to_string)
    );
    assert_eq!(*prompter.calls.borrow(), 1, "the arm asked once");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("chain_id = 1"),
        "a declined write leaves the file alone: {text}"
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn unset_removes_a_scalar_override() {
    let file = write_config("unset-scalar", "[session]\nchain_id = 8453\n");
    let env = env(&[]);
    let context = ctx(&env, Some(&file));
    let outcome = run(
        &Command::Config(ConfigCommand::Unset {
            key: "session.chain_id".to_string(),
            force: true,
        }),
        &context,
        &NoPrompt::new(),
    );
    assert_eq!(
        outcome.exit_code,
        ExitCode::Success,
        "{:?}",
        outcome.error().map(ToString::to_string)
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains("chain_id"),
        "the override is gone so the default applies: {text}"
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn set_writes_a_node_entry_and_get_returns_it() {
    let file = write_config("set-entry", "[nodes]\nhttp = { 1 = \"http://a:8545\" }\n");
    let env = env(&[]);
    let context = ctx(&env, Some(&file));
    let outcome = run(
        &Command::Config(ConfigCommand::Set {
            key: "nodes.http.2".to_string(),
            value: "http://b:9999".to_string(),
            force: true,
        }),
        &context,
        &NoPrompt::new(),
    );
    assert_eq!(
        outcome.exit_code,
        ExitCode::Success,
        "{:?}",
        outcome.error().map(ToString::to_string)
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("http://b:9999"), "the entry landed: {text}");
    assert!(text.contains("http://a:8545"), "the sibling stays: {text}");

    let read_ctx = ctx(&env, Some(&file));
    let get = run(
        &Command::Config(ConfigCommand::Get {
            key: "nodes.http.2".to_string(),
        }),
        &read_ctx,
        &NoPrompt::new(),
    );
    let lines = get.report().unwrap().render_lines();
    assert_eq!(
        line_for(&lines, "nodes.http.2"),
        "nodes.http.2 = http://b:9999 (file)"
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn set_reports_the_shadowing_entry_env() {
    use degenbot_cli_core::MutationOutcome;
    let file = write_config(
        "set-entry-shadow",
        "[nodes]\nhttp = { 1 = \"http://a:8545\" }\n",
    );
    let env = env(&[("DEGENBOT_RPC_HTTP_CHAINID_1", "http://shadow:8545")]);
    let context = ctx(&env, Some(&file));
    let outcome = run(
        &Command::Config(ConfigCommand::Set {
            key: "nodes.http.1".to_string(),
            value: "http://b:9999".to_string(),
            force: true,
        }),
        &context,
        &NoPrompt::new(),
    );
    assert_eq!(
        outcome.exit_code,
        ExitCode::Success,
        "{:?}",
        outcome.error().map(ToString::to_string)
    );
    let Some(CommandReport::Config(ConfigReport::Set { outcome, .. })) = outcome.report() else {
        panic!("a set reports the set variant");
    };
    assert_eq!(
        *outcome,
        MutationOutcome::Shadowed {
            env: "DEGENBOT_RPC_HTTP_CHAINID_1".to_string()
        }
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn mutating_config_arms_prompt_unless_forced() {
    for command in [
        ConfigCommand::Set {
            key: "session.chain_id".to_string(),
            value: "1".to_string(),
            force: false,
        },
        ConfigCommand::Unset {
            key: "session.chain_id".to_string(),
            force: false,
        },
    ] {
        assert_eq!(
            command.prompt_plan(&ctx(&env(&[]), None)),
            PromptPlan::UnlessForce
        );
    }
    assert_eq!(
        ConfigCommand::Get {
            key: "session.chain_id".to_string()
        }
        .prompt_plan(&ctx(&env(&[]), None)),
        PromptPlan::None
    );
}

#[test]
fn set_redacts_in_render_but_the_file_keeps_the_credential() {
    let file = write_config("redact-set", "[nodes]\nhttp = { 1 = \"http://a:8545\" }\n");
    let env = env(&[]);
    let context = ctx(&env, Some(&file));
    let outcome = run(
        &Command::Config(ConfigCommand::Set {
            key: "nodes.http.1".to_string(),
            value: "https://user:secret@host/x?api_key=abc".to_string(),
            force: true,
        }),
        &context,
        &NoPrompt::new(),
    );
    assert_eq!(
        outcome.exit_code,
        ExitCode::Success,
        "{:?}",
        outcome.error().map(ToString::to_string)
    );

    // The file on disk keeps exactly what the operator wrote.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("user:secret@host"),
        "the secret stays on disk: {text}"
    );
    assert!(
        text.contains("api_key=abc"),
        "the secret stays on disk: {text}"
    );
    for line in outcome.report().unwrap().render_lines() {
        assert!(
            !line.contains("secret"),
            "the set report must not leak: {line}"
        );
        assert!(
            !line.contains("abc"),
            "the set report must not leak: {line}"
        );
    }

    // `config show --resolved` renders the redacted form.
    let read_ctx = ctx(&env, Some(&file));
    let show = run(
        &Command::Config(ConfigCommand::Show { resolved: true }),
        &read_ctx,
        &NoPrompt::new(),
    );
    let lines = show.report().unwrap().render_lines();
    assert_eq!(
        line_for(&lines, "nodes.http[1]"),
        "nodes.http[1] = https://host/x?api_key=REDACTED (file)"
    );

    // `config get` uses the same render contract.
    let get_ctx = ctx(&env, Some(&file));
    let get = run(
        &Command::Config(ConfigCommand::Get {
            key: "nodes.http.1".to_string(),
        }),
        &get_ctx,
        &NoPrompt::new(),
    );
    let get_lines = get.report().unwrap().render_lines();
    assert_eq!(
        line_for(&get_lines, "nodes.http.1"),
        "nodes.http.1 = https://host/x?api_key=REDACTED (file)"
    );
    let _ = std::fs::remove_file(&file);
}

fn line_for<'a>(lines: &'a [String], key: &str) -> &'a str {
    let matches: Vec<&String> = lines.iter().filter(|line| line.starts_with(key)).collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one {key} line in:\n{}",
        lines.join("\n")
    );
    matches[0]
}

#[test]
fn resolved_show_names_the_winning_layer_of_each_endpoint() {
    let file = write_config(
        "resolved",
        "[nodes]\nhttp = { 1 = \"http://127.0.0.1:8545\" }\n",
    );
    let env = env(&[("DEGENBOT_RPC_WS_CHAINID_1", "ws://127.0.0.1:8546")]);
    let lines = lines(&env, Some(&file), ConfigCommand::Show { resolved: true });
    assert_eq!(
        line_for(&lines, "nodes.http[1]"),
        "nodes.http[1] = http://127.0.0.1:8545 (file)"
    );
    assert_eq!(
        line_for(&lines, "nodes.ws[1]"),
        "nodes.ws[1] = ws://127.0.0.1:8546 (env)"
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn an_explicit_node_reports_the_cli_layer_and_only_that_layer() {
    let env = env(&[]);
    let ctx = ctx(&env, None).with_node("ws://x", NodeTransport::Ws);
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Show { resolved: true }),
        &ctx,
        &prompter,
    );
    let lines = outcome.report().unwrap().render_lines();
    assert_eq!(line_for(&lines, "nodes.ws"), "nodes.ws = ws://x (cli)");
    let cli_lines: Vec<&String> = lines.iter().filter(|line| line.contains("(cli)")).collect();
    assert_eq!(cli_lines.len(), 1, "only the ws slot: {}", lines.join("\n"));
}

#[test]
fn a_socket_path_node_classifies_as_ipc() {
    let env = env(&[]);
    let ctx = ctx(&env, None).with_node("/tmp/anvil.ipc", NodeTransport::Ipc);
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Show { resolved: true }),
        &ctx,
        &prompter,
    );
    let lines = outcome.report().unwrap().render_lines();
    assert_eq!(
        line_for(&lines, "nodes.ipc"),
        "nodes.ipc = /tmp/anvil.ipc (cli)"
    );
}

#[test]
fn every_occurrence_fills_its_own_transport_slot() {
    let env = env(&[]);
    let mut overrides = degenbot_config::NodeOverrides::new();
    overrides = overrides.with_transport(NodeTransport::Http, "http://x");
    overrides = overrides.with_transport(NodeTransport::Ws, "ws://y");
    let ctx = CliContext::new(&env).with_node_overrides(overrides);
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Show { resolved: true }),
        &ctx,
        &prompter,
    );
    let lines = outcome.report().unwrap().render_lines();
    assert_eq!(
        line_for(&lines, "nodes.http"),
        "nodes.http = http://x (cli)"
    );
    assert_eq!(line_for(&lines, "nodes.ws"), "nodes.ws = ws://y (cli)");
}

#[test]
fn show_without_resolved_reports_only_the_file_layer() {
    let file = write_config(
        "file-only",
        "[session]\nchain_id = 8453\n\n[nodes]\nhttp = { 1 = \"http://127.0.0.1:8545\" }\n",
    );
    let env = env(&[("DEGENBOT_RPC_WS_CHAINID_1", "ws://127.0.0.1:8546")]);
    let lines = lines(&env, Some(&file), ConfigCommand::Show { resolved: false });
    assert_eq!(
        line_for(&lines, "session.chain_id"),
        "session.chain_id = 8453"
    );
    assert_eq!(
        line_for(&lines, "nodes.http[1]"),
        "nodes.http[1] = http://127.0.0.1:8545"
    );
    assert!(
        lines.iter().all(|line| !line.ends_with(')')),
        "the file view carries no layer column:\n{}",
        lines.join("\n")
    );
    assert!(
        lines.iter().all(|line| !line.starts_with("nodes.ws[1]")),
        "an env-layer value is not a file value:\n{}",
        lines.join("\n")
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn the_show_header_says_whether_the_file_exists_yet() {
    // A home with no config file yet: the listing names the file the mutating
    // arms would create rather than pretending a file supplied the values.
    let absent_home = format!("/tmp/adr062-absent-home-{}", std::process::id());
    let seam = env(&[("HOME", absent_home.as_str())]);
    let expected = format!("{absent_home}/.config/degenbot/config.toml");
    let header = lines(&seam, None, ConfigCommand::Show { resolved: false });
    assert_eq!(
        line_for(&header, "config"),
        format!("config = {expected} (not yet created)")
    );
    assert_eq!(lines(&seam, None, ConfigCommand::Path), vec![expected]);
    let file = write_config("header", "[session]\nchain_id = 1\n");
    let env = env(&[]);
    let header = lines(&env, Some(&file), ConfigCommand::Show { resolved: false });
    assert_eq!(
        line_for(&header, "config"),
        format!("config = {}", file.display())
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn path_prints_the_file_the_mutating_arms_write() {
    let file = write_config("path", "[session]\nchain_id = 1\n");
    let env = env(&[]);
    let lines = lines(&env, Some(&file), ConfigCommand::Path);
    assert_eq!(lines, vec![file.to_string_lossy().into_owned()]);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn path_falls_back_to_the_config_home() {
    let env = env(&[("DEGENBOT_CONFIG", "/tmp/cli-config-env-named.toml")]);
    let lines = lines(&env, None, ConfigCommand::Path);
    assert_eq!(lines, vec!["/tmp/cli-config-env-named.toml".to_string()]);
}

#[test]
fn neither_config_arm_prompts() {
    for command in [
        ConfigCommand::Show { resolved: true },
        ConfigCommand::Show { resolved: false },
        ConfigCommand::Path,
    ] {
        assert_eq!(command.prompt_plan(&ctx(&env(&[]), None)), PromptPlan::None);
    }
}

#[test]
fn the_report_carries_the_file_and_the_values() {
    let file = write_config("typed", "[session]\nchain_id = 1\n");
    let env = env(&[]);
    let ctx = ctx(&env, Some(&file));
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Show { resolved: true }),
        &ctx,
        &prompter,
    );
    let Some(CommandReport::Config(ConfigReport::Shown {
        file: reported,
        values,
        resolved,
    })) = outcome.report()
    else {
        panic!("a config show reports the shown variant");
    };
    assert_eq!(reported.as_deref(), Some(file.as_path()));
    assert!(resolved, "the arm was asked for the resolved view");
    assert!(
        values
            .iter()
            .any(|value| value.key == "session.chain_id" && value.value == "1"),
        "the typed chain id is one of the values: {values:?}"
    );
    let _ = std::fs::remove_file(&file);
}

#[test]
fn an_unresolved_value_names_itself_rather_than_guessing() {
    let env = env(&[]);
    let lines = lines(&env, None, ConfigCommand::Show { resolved: true });
    assert_eq!(
        line_for(&lines, "session.chain_id"),
        "session.chain_id = (unresolved)"
    );
}

#[test]
fn a_broken_config_file_is_a_typed_refusal() {
    let file = write_config("broken", "[nodes]\nhttp = { 1 = \"ftp://nope\" }\n");
    let env = env(&[]);
    let ctx = ctx(&env, Some(&file));
    let prompter = NoPrompt::new();
    let outcome = run(
        &Command::Config(ConfigCommand::Show { resolved: true }),
        &ctx,
        &prompter,
    );
    assert_eq!(outcome.exit_code, ExitCode::Failure);
    let message = outcome.error().expect("the load is refused").to_string();
    assert!(
        message.contains("nodes.http"),
        "the refusal names the offending key: {message}"
    );
    let _ = std::fs::remove_file(&file);
}
