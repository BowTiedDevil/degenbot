//! argv -> `Command` round-trip tests (ADR-051 D2).
//!
//! Every command in the tree is parsed from argv and mapped to the
//! constructor-parsed `Command` cli-core owns; the assertions are the mapping
//! contract, not the rendering.

use std::collections::BTreeMap;

use clap::Parser as _;
use degenbot_cli::argv::{resolve_with_env, Cli};
use degenbot_cli_core::{
    AaveCommand, CliError, Command, ConfigCommand, DatabaseCommand, ExchangeCommand, FleetCommand,
    PathCommand, PathDirection, PoolCommand, PoolFamily, PosturePatchEntry, PosturePatchValue,
    StrategyCommand, StrategyFacet, DEFAULT_CHUNK_SIZE, DEFAULT_TO_BLOCK,
    DEFAULT_VERIFY_ALL_INTERVAL,
};
use degenbot_config::{MapEnv, NodeTransport};

#[expect(
    clippy::expect_used,
    reason = "test fixture: a malformed argv must fail the test loudly"
)]
fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(args).expect("argv parses")
}

fn empty_env() -> MapEnv {
    MapEnv::new(BTreeMap::new())
}

#[expect(
    clippy::expect_used,
    reason = "test fixture: a malformed argv must fail the test loudly"
)]
fn resolve(args: &[&str]) -> Command {
    let cli = parse(args);
    resolve_with_env(&cli, &empty_env()).expect("argv resolves")
}

#[test]
fn database_arms_round_trip() {
    assert_eq!(
        resolve(&["degenbot", "database", "backup"]),
        Command::Database(DatabaseCommand::Backup)
    );
    assert_eq!(
        resolve(&["degenbot", "database", "reset", "--force"]),
        Command::Database(DatabaseCommand::Reset { force: true })
    );
    assert_eq!(
        resolve(&["degenbot", "database", "reset"]),
        Command::Database(DatabaseCommand::Reset { force: false })
    );
    assert_eq!(
        resolve(&["degenbot", "database", "upgrade", "--force"]),
        Command::Database(DatabaseCommand::Upgrade { force: true })
    );
    assert_eq!(
        resolve(&["degenbot", "database", "compact"]),
        Command::Database(DatabaseCommand::Compact)
    );
    assert_eq!(
        resolve(&["degenbot", "database", "cutover", "--dry-run", "--force"]),
        Command::Database(DatabaseCommand::Cutover {
            dry_run: true,
            force: true
        })
    );
    assert_eq!(
        resolve(&["degenbot", "database", "heal"]),
        Command::Database(DatabaseCommand::Heal {
            dry_run: false,
            force: false
        })
    );
    assert_eq!(
        resolve(&["degenbot", "database", "inspect"]),
        Command::Database(DatabaseCommand::Inspect)
    );
}

#[test]
fn exchange_arms_round_trip() {
    assert_eq!(
        resolve(&[
            "degenbot",
            "exchange",
            "activate",
            "--chain",
            "base",
            "--name",
            "aerodrome_v2"
        ]),
        Command::Exchange(ExchangeCommand::Activate {
            chain: "base".to_string(),
            name: "aerodrome_v2".to_string(),
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "exchange",
            "deactivate",
            "--chain",
            "8453",
            "--name",
            "uniswap_v4"
        ]),
        Command::Exchange(ExchangeCommand::Deactivate {
            chain: "8453".to_string(),
            name: "uniswap_v4".to_string(),
        })
    );
    assert_eq!(
        resolve(&["degenbot", "exchange", "list"]),
        Command::Exchange(ExchangeCommand::List { chain: None })
    );
    assert_eq!(
        resolve(&["degenbot", "exchange", "list", "--chain", "ethereum"]),
        Command::Exchange(ExchangeCommand::List {
            chain: Some("ethereum".to_string()),
        })
    );
}

#[test]
fn pool_update_defaults_and_gates() {
    assert_eq!(
        resolve(&["degenbot", "pool", "update"]),
        Command::Pool(PoolCommand::Update {
            chunk_size: DEFAULT_CHUNK_SIZE,
            to_block: DEFAULT_TO_BLOCK.to_string(),
            verify_chunk: true,
            verify_all: false,
            verify_all_interval: DEFAULT_VERIFY_ALL_INTERVAL,
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "pool",
            "update",
            "--chunk",
            "5",
            "--to-block",
            "1000",
            "--no-verify-chunk",
            "--verify-all",
            "--verify-all-interval",
            "7"
        ]),
        Command::Pool(PoolCommand::Update {
            chunk_size: 5,
            to_block: "1000".to_string(),
            verify_chunk: false,
            verify_all: true,
            verify_all_interval: 7,
        })
    );
}

#[test]
fn pool_verify_round_trips() {
    assert_eq!(
        resolve(&[
            "degenbot",
            "pool",
            "verify",
            "--rpc-url",
            "http://localhost:8545",
            "--chain",
            "1",
            "--block",
            "123",
            "--pool",
            "0xabc",
            "--family",
            "v3"
        ]),
        Command::Pool(PoolCommand::Verify {
            rpc_url: "http://localhost:8545".to_string(),
            chain_id: 1,
            block_number: 123,
            pool: "0xabc".to_string(),
            family: PoolFamily::V3,
            pool_manager: None,
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "pool",
            "verify",
            "--rpc-url",
            "http://x",
            "--chain",
            "8453",
            "--block",
            "1",
            "--pool",
            "0xid",
            "--family",
            "v4",
            "--pool-manager",
            "0xman"
        ]),
        Command::Pool(PoolCommand::Verify {
            rpc_url: "http://x".to_string(),
            chain_id: 8453,
            block_number: 1,
            pool: "0xid".to_string(),
            family: PoolFamily::V4,
            pool_manager: Some("0xman".to_string()),
        })
    );
}

#[test]
fn aave_arms_round_trip() {
    assert_eq!(
        resolve(&["degenbot", "aave", "activate"]),
        Command::Aave(AaveCommand::Activate { chain_id: 1 })
    );
    assert_eq!(
        resolve(&["degenbot", "aave", "deactivate"]),
        Command::Aave(AaveCommand::Deactivate {
            chain_id: 1,
            market_name: "Aave Ethereum Market".to_string(),
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "aave",
            "update",
            "--one-chunk",
            "--dry-run",
            "--no-verify-chunk"
        ]),
        Command::Aave(AaveCommand::Update {
            chunk_size: DEFAULT_CHUNK_SIZE,
            to_block: DEFAULT_TO_BLOCK.to_string(),
            verify_chunk: false,
            verify_all: false,
            verify_all_interval: DEFAULT_VERIFY_ALL_INTERVAL,
            stop_after_one_chunk: true,
            dry_run: true,
            enable_backup: false,
        })
    );
    assert_eq!(
        resolve(&["degenbot", "aave", "reset", "--dry-run"]),
        Command::Aave(AaveCommand::Reset {
            chain_id: 1,
            market_name: Some("Aave Ethereum Market".to_string()),
            dry_run: true,
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "aave",
            "reset",
            "--name",
            "Other Market",
            "--chain-id",
            "8453"
        ]),
        Command::Aave(AaveCommand::Reset {
            chain_id: 8453,
            market_name: Some("Other Market".to_string()),
            dry_run: false,
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "aave",
            "position",
            "show",
            "0xABC",
            "--chain-id",
            "8453"
        ]),
        Command::Aave(AaveCommand::PositionShow {
            address: "0xABC".to_string(),
            market: "Aave Ethereum Market".to_string(),
            chain_id: 8453,
        })
    );
}

#[test]
fn aave_activate_honors_a_malformed_chain_layer() {
    let cli = parse(&["degenbot", "aave", "activate", "--chain-id", "not-a-number"]);
    assert!(matches!(
        resolve_with_env(&cli, &empty_env()),
        Err(CliError::Config(_))
    ));
}

#[test]
fn fleet_posture_round_trips() {
    assert_eq!(
        resolve(&[
            "degenbot",
            "fleet",
            "posture",
            "show",
            "--socket",
            "/tmp/op.sock"
        ]),
        Command::Fleet(FleetCommand::PostureShow {
            socket: Some("/tmp/op.sock".to_string()),
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "fleet",
            "posture",
            "set",
            "--cordon-enter-events",
            "2",
            "--cordon-duty-percent",
            "12.5",
            "--cordon-sim-intake-floor",
            "null"
        ]),
        Command::Fleet(FleetCommand::PostureSet {
            socket: None,
            patch: vec![
                PosturePatchEntry::int("cordon_enter_events", 2),
                PosturePatchEntry::float("cordon_duty_percent", 12.5),
                PosturePatchEntry::new("cordon_sim_intake_floor", PosturePatchValue::Null),
            ],
        })
    );
}

#[test]
fn path_arms_round_trip() {
    assert_eq!(
        resolve(&[
            "degenbot",
            "path",
            "add",
            "--hop",
            "V3:0xabc",
            "--hop",
            "V4:0xdef:0x1234",
            "--direction",
            "zfo",
            "--socket",
            "/tmp/op.sock"
        ]),
        Command::Path(PathCommand::Add {
            socket: Some("/tmp/op.sock".to_string()),
            hops: vec!["V3:0xabc".to_string(), "V4:0xdef:0x1234".to_string()],
            direction: Some(PathDirection::Zfo),
        })
    );
    assert_eq!(
        resolve(&["degenbot", "path", "discover", "--bound", "5"]),
        Command::Path(PathCommand::Discover {
            socket: None,
            bound: Some(5),
        })
    );
}

#[test]
fn strategy_arms_round_trip() {
    assert_eq!(
        resolve(&["degenbot", "strategy", "list"]),
        Command::Strategy(StrategyCommand::List)
    );
    assert_eq!(
        resolve(&["degenbot", "strategy", "show", "mevblocker_backrun"]),
        Command::Strategy(StrategyCommand::Show {
            facet: StrategyFacet::MevblockerBackrun,
        })
    );
    assert_eq!(
        resolve(&["degenbot", "strategy", "show", "txpool_backrun"]),
        Command::Strategy(StrategyCommand::Show {
            facet: StrategyFacet::TxpoolBackrun,
        })
    );
    assert_eq!(
        resolve(&["degenbot", "strategy", "set", "settlement", "enabled", "1"]),
        Command::Strategy(StrategyCommand::Set {
            facet: StrategyFacet::Settlement,
            key: "enabled".to_string(),
            value: "1".to_string(),
        })
    );
    assert_eq!(
        resolve(&[
            "degenbot",
            "strategy",
            "remove",
            "mevblocker_backrun",
            "enabled"
        ]),
        Command::Strategy(StrategyCommand::Remove {
            facet: StrategyFacet::MevblockerBackrun,
            key: "enabled".to_string(),
        })
    );
}

#[test]
fn root_help_lists_every_group() {
    let help = <Cli as clap::CommandFactory>::command()
        .render_help()
        .to_string();
    for group in [
        "database", "exchange", "pool", "aave", "fleet", "path", "strategy", "config",
    ] {
        assert!(help.contains(group), "missing {group} in:\n{help}");
    }
    for flag in ["--database", "--chain-id", "--node", "--version"] {
        assert!(help.contains(flag), "missing {flag} in:\n{help}");
    }
    // ADR-062 D6 is a hard cutover: the retired per-transport spellings are
    // gone from the tree, with no alias left behind.
    for retired in ["--node-http", "--node-ws"] {
        assert!(
            !help.contains(retired),
            "the retired {retired} must not survive in:\n{help}"
        );
    }
}

#[test]
fn one_repeatable_node_flag_classifies_each_occurrence() {
    let cli = parse(&[
        "degenbot",
        "--node",
        "http://127.0.0.1:8545",
        "--node",
        "wss://eth.example.com/ws",
        "--node",
        "/tmp/anvil.ipc",
    ]);
    let transports: Vec<NodeTransport> = cli.node.iter().map(|node| node.transport).collect();
    assert_eq!(
        transports,
        vec![NodeTransport::Http, NodeTransport::Ws, NodeTransport::Ipc]
    );
    let env = empty_env();
    let ctx = degenbot_cli::argv::context(&cli, &env);
    assert_eq!(
        ctx.node_overrides().http.as_deref(),
        Some("http://127.0.0.1:8545")
    );
    assert_eq!(
        ctx.node_overrides().ws.as_deref(),
        Some("wss://eth.example.com/ws")
    );
    assert_eq!(ctx.node_overrides().ipc.as_deref(), Some("/tmp/anvil.ipc"));
}

#[test]
fn a_node_value_no_transport_serves_is_a_usage_error() {
    for bad in ["ftp://x", "localhost:8545", ""] {
        let (code, rendered) = match Cli::try_parse_from(["degenbot", "--node", bad]) {
            Ok(_) => (0, String::new()),
            Err(error) => (error.exit_code(), error.render().to_string()),
        };
        assert_eq!(code, 2, "rendered: {rendered:?}");
        for form in ["wss://", "http://", "ipc://"] {
            assert!(
                rendered.contains(form),
                "the refusal names the {form} form: {rendered:?}"
            );
        }
    }
}

#[test]
fn config_arms_round_trip() {
    assert_eq!(
        resolve(&["degenbot", "config", "show"]),
        Command::Config(ConfigCommand::Show { resolved: false })
    );
    assert_eq!(
        resolve(&["degenbot", "config", "show", "--resolved"]),
        Command::Config(ConfigCommand::Show { resolved: true })
    );
    assert_eq!(
        resolve(&["degenbot", "config", "path"]),
        Command::Config(ConfigCommand::Path)
    );
}

#[expect(
    clippy::expect_used,
    reason = "test fixture: a missing group must fail the test loudly"
)]
#[test]
fn group_help_renders_the_leaf_commands() {
    let command = <Cli as clap::CommandFactory>::command();
    for (group, leaves) in [
        (
            "database",
            vec![
                "backup", "reset", "upgrade", "compact", "cutover", "heal", "inspect",
            ],
        ),
        ("exchange", vec!["activate", "deactivate"]),
        ("pool", vec!["update", "verify"]),
        ("aave", vec!["activate", "deactivate", "update", "position"]),
        ("config", vec!["show", "path"]),
        ("fleet", vec!["posture"]),
        ("path", vec!["add", "discover"]),
        (
            "strategy",
            vec![
                "list",
                "show",
                "activate",
                "deactivate",
                "set",
                "default",
                "remove",
            ],
        ),
    ] {
        let sub = command.find_subcommand(group).expect("group present");
        let help = sub.clone().render_help().to_string();
        for leaf in leaves {
            assert!(
                help.contains(leaf),
                "missing {leaf} in {group} help:\n{help}"
            );
        }
    }
}

#[test]
fn version_reports_the_workspace_version_and_the_receipt_fingerprint() {
    let rendered = match Cli::try_parse_from(["degenbot", "--version"]) {
        Ok(_) => String::new(),
        Err(error) => error.to_string(),
    };
    assert!(
        rendered.contains(env!("CARGO_PKG_VERSION")),
        "version line: {rendered:?}"
    );
    assert!(rendered.contains("(build "), "version line: {rendered:?}");
    assert!(
        rendered.contains(env!("DEGENBOT_CLI_BUILD_FINGERPRINT")),
        "version line: {rendered:?}"
    );
}

#[test]
fn unknown_root_subcommand_exits_two_with_usage() {
    let (code, rendered) = match Cli::try_parse_from(["degenbot", "definitely-not-a-command"]) {
        Ok(_) => (0, String::new()),
        Err(error) => (error.exit_code(), error.render().to_string()),
    };
    assert_eq!(code, 2, "rendered: {rendered:?}");
    assert!(rendered.contains("Usage"), "rendered: {rendered:?}");
}

#[test]
fn missing_subcommand_is_a_typed_refusal() {
    let parsed = Cli::try_parse_from(["degenbot", "--database", "/tmp/degenbot.db"]);
    assert!(parsed.is_ok(), "the global-only form parses: {parsed:?}");
    let Ok(cli) = parsed else { return };
    assert!(matches!(
        resolve_with_env(&cli, &empty_env()),
        Err(CliError::InvalidArgument(_))
    ));
}

#[test]
fn run_args_pins_the_console_exit_codes() {
    // clap owns --help/--version (exit 0) and usage diagnostics (exit 2);
    // the facade returns the code rather than exiting the process, so both
    // entry surfaces share one contract.
    assert_eq!(degenbot_cli::run_args(&["--help".to_string()]), 0);
    assert_eq!(degenbot_cli::run_args(&["--version".to_string()]), 0);
    assert_eq!(
        degenbot_cli::run_args(&["definitely-not-a-command".to_string()]),
        2
    );
}

#[cfg(unix)]
#[test]
fn run_args_tolerates_non_utf8_argv() {
    // The binary parses `std::env::args_os()`, so argv bytes outside UTF-8
    // reach the composition root intact; clap's UnknownArgument usage code
    // is the correct refusal, never a panic.
    use std::os::unix::ffi::OsStringExt as _;
    let invalid = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
    assert_eq!(degenbot_cli::run_args(&[invalid]), 2);
}
