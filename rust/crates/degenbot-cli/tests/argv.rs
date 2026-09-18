//! argv -> `Command` round-trip tests (ADR-051 D2).
//!
//! Every command in the tree is parsed from argv and mapped to the
//! constructor-parsed `Command` cli-core owns; the assertions are the mapping
//! contract, not the rendering.

use std::collections::BTreeMap;

use clap::Parser as _;
use degenbot_cli::argv::{resolve_with_env, Cli};
use degenbot_cli_core::{
    AaveCommand, CliError, Command, DatabaseCommand, ExchangeCommand, FleetCommand, PathCommand,
    PathDirection, PoolCommand, PoolFamily, PosturePatchEntry, PosturePatchValue, StrategyCommand,
    StrategyFacet, DEFAULT_CHUNK_SIZE, DEFAULT_TO_BLOCK, DEFAULT_VERIFY_ALL_INTERVAL,
};
use degenbot_config::MapEnv;

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
        resolve(&["degenbot", "strategy", "show", "backrun"]),
        Command::Strategy(StrategyCommand::Show {
            facet: StrategyFacet::Backrun,
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
        resolve(&["degenbot", "strategy", "remove", "backrun", "enabled"]),
        Command::Strategy(StrategyCommand::Remove {
            facet: StrategyFacet::Backrun,
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
        "database", "exchange", "pool", "aave", "fleet", "path", "strategy",
    ] {
        assert!(help.contains(group), "missing {group} in:\n{help}");
    }
    for flag in [
        "--database",
        "--chain-id",
        "--node-http",
        "--node-ws",
        "--version",
    ] {
        assert!(help.contains(flag), "missing {flag} in:\n{help}");
    }
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
        ("fleet", vec!["posture"]),
        ("path", vec!["add", "discover"]),
        ("strategy", vec!["list", "show", "add", "set", "remove"]),
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
