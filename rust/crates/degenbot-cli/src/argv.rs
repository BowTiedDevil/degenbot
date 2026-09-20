//! The clap v4 argv tree and the argv -> [`Command`] mapping (ADR-051 D2).
//!
//! This is the ONE place argv is spelled. The mapping below translates clap
//! matches into the constructor-parsed values of `degenbot-cli-core` and never
//! re-encodes semantics: every arm, flag, prompt and exit code lives there.
//!
//! # Where clap's shape differs from the retired click tree
//!
//! - **`exchange` (ADR-051 D5, 34 click verbs -> one data-driven command).**
//!   Python declared one verb per `(chain, DEX)` pair (`base_aerodrome_v2`,
//!   `ethereum_uniswap_v3`, …). cli-core models that as
//!   `exchange activate|deactivate --chain <slug|id> --name <dex>`, resolving
//!   the pair against [`RETIRED_EXCHANGES`](degenbot_cli_core::RETIRED_EXCHANGES).
//! - **`aave activate|deactivate`.** Python spelled the market as a verb
//!   (`aave activate ethereum_aave_v3`); cli-core takes `chain_id` and resolves
//!   the deployment from [`AAVE_DEPLOYMENTS`](degenbot_cli_core::AAVE_DEPLOYMENTS),
//!   so the tree is `aave activate [--chain-id <id>]` with Python's Ethereum
//!   default (1) when no chain layer supplied a value.
//! - **`aave position show <ADDRESS>`.** The click shape is kept, except that
//!   its chain selector IS the global `--chain-id` (ADR-051 D8) rather than a
//!   second, position-local spelling of the same thing; cli-core's arm takes
//!   the resolved session chain id.
//! - **`--socket`.** cli-core resolves the operator socket through the cascade
//!   `--socket` > `DEGENBOT_OPERATOR_SOCKET` > `~/.config/degenbot/operator.sock`
//!   ([`resolve_socket`](degenbot_cli_core::resolve_socket)); click declared
//!   `--socket` required. The flag is therefore optional here - cli-core owns
//!   the cascade.
//! - **`database reset`.** Python marks it `hidden=True`. The command is
//!   present and visible here (cli-core carries the arm; hiding it would make
//!   the Rust tree the only place the arm is unreachable).
//! - **Flag help text** is the Python help text, so the two surfaces read the
//!   same. The per-flag click `envvar=` fallbacks (`DEGENBOT_CHUNK_SIZE`, …) are
//!   NOT re-added: cli-core models these inputs as argv only, and the only env
//!   vocabulary the console owns is the driver-domain set of ADR-051 D8.

use std::io::Write as _;

use clap::{ArgAction, Args, CommandFactory as _, Parser, Subcommand, ValueEnum};
use degenbot_cli_core::{
    AaveCommand, CliContext, CliError, Command, DatabaseCommand, ExchangeCommand, FleetCommand,
    PathCommand, PathDirection, PoolCommand, PoolFamily, PosturePatchEntry, StrategyCommand,
    StrategyFacet, DEFAULT_CHUNK_SIZE, DEFAULT_TO_BLOCK, DEFAULT_VERIFY_ALL_INTERVAL,
};
use degenbot_config::{EnvVars, ProcessEnv, DEFAULT_CHAIN_ID_ENV};

use crate::VERSION_LINE;

/// The degenbot console.
#[derive(Debug, Parser)]
#[command(
    name = "degenbot",
    about = "Perform cli.",
    version = VERSION_LINE,
    arg_required_else_help = true,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Path to the SQLite database (`--database` > `DEGENBOT_DB_PATH` >
    /// `<state_home>/degenbot/db/degenbot.db`).
    #[arg(long, global = true, value_name = "PATH")]
    pub database: Option<String>,

    /// Session chain id (`--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`).
    #[arg(long, global = true, value_name = "CHAIN_ID")]
    pub chain_id: Option<String>,

    /// HTTP RPC endpoint (`--node-http` > `DEGENBOT_RPC_HTTP_CHAINID_<id>`).
    #[arg(long, global = true, value_name = "URI")]
    pub node_http: Option<String>,

    /// WebSocket RPC endpoint (`--node-ws` > `DEGENBOT_RPC_WS_CHAINID_<id>`).
    #[arg(long, global = true, value_name = "URI")]
    pub node_ws: Option<String>,

    /// Typed config file the `strategy` verbs read and write (`--config` >
    /// `DEGENBOT_CONFIG` > the XDG config home).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<String>,
}

/// The command groups.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Database commands.
    Database {
        /// The database command.
        #[command(subcommand)]
        command: DatabaseSub,
    },
    /// Exchange commands.
    Exchange {
        /// The exchange command.
        #[command(subcommand)]
        command: ExchangeSub,
    },
    /// Pool commands.
    Pool {
        /// The pool command.
        #[command(subcommand)]
        command: PoolSub,
    },
    /// Aave commands.
    Aave {
        /// The aave command.
        #[command(subcommand)]
        command: AaveSub,
    },
    /// Steer the live worker fleet over the operator command channel.
    Fleet {
        /// The fleet command.
        #[command(subcommand)]
        command: FleetSub,
    },
    /// Steer a live bot over the operator command channel.
    Path {
        /// The path command.
        #[command(subcommand)]
        command: PathSub,
    },
    /// Inspect the typed strategy facets (ADR-055).
    Strategy {
        /// The strategy command.
        #[command(subcommand)]
        command: StrategySub,
    },
}

/// The `database` command group.
#[derive(Debug, Subcommand)]
pub enum DatabaseSub {
    /// Back up the database.
    Backup,
    /// Remove and recreate the database.
    Reset {
        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,
    },
    /// Upgrade the database to the latest schema (RETIRED: the database
    /// upgrades itself at open; `database heal` is the explicit repair).
    Upgrade {
        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,
    },
    /// Compact the database.
    Compact,
    /// Flip an Alembic-stamped DB into Rust schema ownership (ADR-010).
    Cutover {
        /// Report the schema state + what cutover would do; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt and run the cutover.
        #[arg(long)]
        force: bool,
    },
    /// Rebuild an Alembic-stamped DB into Rust ownership via dump-and-restore
    /// (ADR-011).
    Heal {
        /// Report the schema state + what heal would do; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt and run the heal.
        #[arg(long)]
        force: bool,
    },
    /// Inspect the database schema state (read-only; never writes).
    Inspect,
}

/// The `exchange` command group.
#[derive(Debug, Subcommand)]
pub enum ExchangeSub {
    /// Activate the exchange. Liquidity pools for all activated exchanges are
    /// included when running "pool update".
    Activate {
        /// The chain selector: a chain slug (`base`, `ethereum`) or a numeric
        /// chain id.
        #[arg(long, value_name = "CHAIN")]
        chain: String,
        /// The DEX name slug stored in the database (`aerodrome_v2`,
        /// `uniswap_v3`, …).
        #[arg(long, value_name = "NAME")]
        name: String,
    },
    /// Deactivate the exchange. Liquidity pools for all deactivated exchanges
    /// are not included when running "pool update".
    Deactivate {
        /// The chain selector: a chain slug (`base`, `ethereum`) or a numeric
        /// chain id.
        #[arg(long, value_name = "CHAIN")]
        chain: String,
        /// The DEX name slug stored in the database.
        #[arg(long, value_name = "NAME")]
        name: String,
    },
    /// List the supported exchanges and their activation state.
    List {
        /// Restrict the list to one chain (a chain slug or numeric id).
        #[arg(long, value_name = "CHAIN")]
        chain: Option<String>,
    },
}

/// The `pool` command group.
#[derive(Debug, Subcommand)]
pub enum PoolSub {
    /// Update liquidity pool information for activated exchanges.
    Update {
        /// The maximum number of blocks to process before committing changes to
        /// the database.
        #[arg(long = "chunk", value_name = "BLOCKS", default_value_t = DEFAULT_CHUNK_SIZE)]
        chunk_size: u64,
        /// The last block in the update range. Must be a valid block
        /// identifier: 'earliest', 'finalized', 'safe', 'latest', 'pending'.
        /// An identifier can be given with an optional offset, e.g. 'latest:-64'
        /// stops 64 blocks before the chain tip, 'safe:128' stops 128 blocks
        /// after the last 'safe' block.
        #[arg(long = "to-block", value_name = "BLOCK", default_value = DEFAULT_TO_BLOCK)]
        to_block: String,
        /// The pre-commit verification gates.
        #[command(flatten)]
        verify: VerifyFlags,
        /// Block interval for the `--verify-all` full-verification gate. A chunk
        /// that crosses or lands-on a multiple of this interval triggers a
        /// pre-commit market-wide verify. Ignored unless `--verify-all` is set.
        #[arg(
            long = "verify-all-interval",
            value_name = "BLOCKS",
            default_value_t = DEFAULT_VERIFY_ALL_INTERVAL
        )]
        verify_all_interval: u64,
    },
    /// Verify a pool's committed liquidity map against on-chain truth.
    Verify {
        /// The HTTP RPC endpoint to read on-chain truth from.
        #[arg(long = "rpc-url", value_name = "URL", required = true)]
        rpc_url: String,
        /// The chain id the pool lives on. The Rust field is named
        /// `pool_chain_id` so its clap arg id cannot collide with the global
        /// `--chain-id` driver flag; the argv spelling stays `--chain`.
        #[arg(long = "chain", value_name = "CHAIN_ID", required = true)]
        pool_chain_id: i64,
        /// The block number to read on-chain truth at.
        #[arg(long = "block", value_name = "BLOCK", required = true)]
        block_number: u64,
        #[arg(
            long = "pool",
            value_name = "POOL",
            required = true,
            help = "The pool to verify. V3: the pool contract address. V4: the PoolId (pool_hash, bytes32 hex 0x…)."
        )]
        pool: String,
        #[arg(
            long = "family",
            value_enum,
            required = true,
            help = "The pool family (selects ticks()/tickBitmap() vs PoolManager extsload)."
        )]
        family: FamilyArg,
        #[arg(
            long = "pool-manager",
            value_name = "ADDRESS",
            help = "(V4 only) The deployed V4 PoolManager singleton address (the V4 exchange's factory). Required for --family v4."
        )]
        pool_manager: Option<String>,
    },
}

/// The `--verify-chunk` / `--verify-all` gate pairs shared by both updaters.
#[derive(Debug, Clone, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the two on/off gate pairs are argv vocabulary, not state: each pair is two mutually-overriding flags"
)]
pub struct VerifyFlags {
    /// Run the pre-commit per-chunk on-chain-truth gate before each chunk's
    /// persist commits. A divergence rolls back the chunk + does NOT advance
    /// `last_update_block`.
    #[arg(long = "verify-chunk", action = ArgAction::SetTrue, overrides_with = "no_verify_chunk")]
    pub verify_chunk: bool,
    /// Disable the pre-commit per-chunk on-chain-truth gate.
    #[arg(long = "no-verify-chunk", action = ArgAction::SetTrue, overrides_with = "verify_chunk")]
    pub no_verify_chunk: bool,
    /// Run a pre-commit FULL verification at the block boundary set by
    /// `--verify-all-interval` AND when the run completes its last block. Off
    /// by default (operator opt-in).
    #[arg(long = "verify-all", action = ArgAction::SetTrue, overrides_with = "no_verify_all")]
    pub verify_all: bool,
    /// Disable the market-wide `--verify-all` gate.
    #[arg(long = "no-verify-all", action = ArgAction::SetTrue, overrides_with = "verify_all")]
    pub no_verify_all: bool,
}

impl VerifyFlags {
    /// The per-chunk gate value: default ON, `--no-verify-chunk` turns it off.
    #[must_use]
    pub const fn chunk_gate(&self) -> bool {
        self.verify_chunk || !self.no_verify_chunk
    }

    /// The market-wide gate value: default OFF, `--verify-all` turns it on.
    #[must_use]
    pub const fn all_gate(&self) -> bool {
        self.verify_all && !self.no_verify_all
    }
}

/// The pool family selector.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FamilyArg {
    /// V3 (`ticks()`/`tickBitmap()`).
    V3,
    /// V4 (`PoolManager` `extsload`).
    V4,
}

/// The `aave` command group.
#[derive(Debug, Subcommand)]
pub enum AaveSub {
    /// Activate an Aave market. Positions for activated markets are included
    /// when running `degenbot aave position update`.
    Activate,
    /// Deactivate an Aave market. Positions for deactivated markets are not
    /// included when running `degenbot aave position update`.
    Deactivate {
        /// Market name to flip (default: Aave Ethereum Market).
        #[arg(
            long = "name",
            value_name = "MARKET",
            default_value = "Aave Ethereum Market"
        )]
        market_name: String,
    },
    /// Update positions for active Aave markets.
    Update {
        /// The maximum number of blocks to process before committing changes to
        /// the database.
        #[arg(long = "chunk", value_name = "BLOCKS", default_value_t = DEFAULT_CHUNK_SIZE)]
        chunk_size: u64,
        /// The last block in the update range. Must be a valid block
        /// identifier: 'earliest', 'finalized', 'safe', 'latest', 'pending'.
        /// An identifier can be given with an optional offset, e.g. 'latest:-64'
        /// stops 64 blocks before the chain tip, 'safe:128' stops 128 blocks
        /// after the last 'safe' block.
        #[arg(long = "to-block", value_name = "BLOCK", default_value = DEFAULT_TO_BLOCK)]
        to_block: String,
        /// The pre-commit verification gates.
        #[command(flatten)]
        verify: VerifyFlags,
        /// Block interval for the `--verify-all` full-verification gate. A chunk
        /// that crosses or lands-on a multiple of this interval triggers a
        /// pre-commit market-wide verify. Ignored unless `--verify-all` is set.
        #[arg(
            long = "verify-all-interval",
            value_name = "BLOCKS",
            default_value_t = DEFAULT_VERIFY_ALL_INTERVAL
        )]
        verify_all_interval: u64,
        /// Stop processing after the first chunk.
        #[arg(long = "one-chunk", action = ArgAction::SetTrue)]
        stop_after_one_chunk: bool,
        /// Preview changes without committing to the database.
        #[arg(long = "dry-run", action = ArgAction::SetTrue)]
        dry_run: bool,
        /// Create a database backup after the completion verification runs
        /// (once per market, at the end of the run).
        #[arg(long = "backup", action = ArgAction::SetTrue, overrides_with = "no_backup")]
        backup: bool,
        /// Disable the completion backup.
        #[arg(long = "no-backup", action = ArgAction::SetTrue, overrides_with = "backup")]
        no_backup: bool,
    },
    /// Position commands.
    Position {
        /// The position command.
        #[command(subcommand)]
        command: AavePositionSub,
    },
}

/// The `aave position` command group.
#[derive(Debug, Subcommand)]
pub enum AavePositionSub {
    /// Display current Aave positions for a user.
    Show {
        /// The user address.
        #[arg(value_name = "ADDRESS")]
        address: String,
        /// Market name to query (default: Aave Ethereum Market).
        #[arg(
            long = "market",
            value_name = "MARKET",
            default_value = "Aave Ethereum Market"
        )]
        market: String,
    },
}

/// The `fleet` command group.
#[derive(Debug, Subcommand)]
pub enum FleetSub {
    /// Inspect or re-tune the live cordon posture thresholds.
    Posture {
        /// The posture command.
        #[command(subcommand)]
        command: FleetPostureSub,
    },
}

/// The `fleet posture` command group.
#[derive(Debug, Subcommand)]
pub enum FleetPostureSub {
    /// Show the LIVE cordon posture: thresholds + Nominal|Cordoned.
    Show {
        #[arg(
            long,
            value_name = "PATH",
            help = "Unix domain socket path of the running bot's OperatorServer."
        )]
        socket: Option<String>,
    },
    /// Re-tune the LIVE cordon thresholds (a partial patch).
    Set {
        #[arg(
            long,
            value_name = "PATH",
            help = "Unix domain socket path of the running bot's OperatorServer."
        )]
        socket: Option<String>,
        /// Throttle events within the enter window that cordon the fleet.
        #[arg(long = "cordon-enter-events", value_name = "COUNT")]
        cordon_enter_events: Option<u64>,
        /// Throttled-time duty percent over the duty window that cordons the
        /// fleet.
        #[arg(long = "cordon-duty-percent", value_name = "PERCENT")]
        cordon_duty_percent: Option<f64>,
        /// Rolling window (ms) for the throttle-event burst enter trigger.
        #[arg(long = "cordon-enter-window-ms", value_name = "MS")]
        cordon_enter_window_ms: Option<u64>,
        /// Trailing window (ms) over which throttled-time duty is evaluated.
        #[arg(long = "cordon-duty-window-ms", value_name = "MS")]
        cordon_duty_window_ms: Option<u64>,
        /// Clean-window hysteresis (ms) required before cordon exits.
        #[arg(long = "cordon-exit-clean-ms", value_name = "MS")]
        cordon_exit_clean_ms: Option<u64>,
        #[arg(
            long = "cordon-sim-intake-floor",
            value_name = "COUNT|null",
            help = "SimDriver new-lease cap while cordoned. The literal null restores half the slot cap."
        )]
        cordon_sim_intake_floor: Option<String>,
    },
}

/// The `path` command group.
#[derive(Debug, Subcommand)]
pub enum PathSub {
    /// Add ONE specific path to the live bot mid-run.
    Add {
        #[arg(
            long,
            value_name = "PATH",
            help = "Unix domain socket path of the running bot's OperatorServer."
        )]
        socket: Option<String>,
        /// A path hop as FAMILY:ADDRESS, or V4:ADDRESS:HASH (pool id). Repeat
        /// for each hop, in path order.
        #[arg(long = "hop", value_name = "HOP", required = true)]
        hops: Vec<String>,
        /// Applied to every hop when set: 'zfo' (zero-for-one, True for each
        /// hop) or 'ozf' (one-for-zero, False for each hop). Omit to let the bot
        /// auto-resolve directions.
        #[arg(long = "direction", value_enum)]
        direction: Option<DirectionArg>,
    },
    /// Trigger a bounded on-demand discovery sweep on the live bot.
    Discover {
        #[arg(
            long,
            value_name = "PATH",
            help = "Unix domain socket path of the running bot's OperatorServer."
        )]
        socket: Option<String>,
        /// Maximum number of paths to process in this discovery sweep.
        #[arg(long = "bound", value_name = "COUNT")]
        bound: Option<u64>,
    },
}

/// The `--direction` choice on `path add`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DirectionArg {
    /// Zero-for-one.
    Zfo,
    /// One-for-zero.
    Ozf,
}

/// The `strategy` command group (ADR-055 facets): the strategy activation
/// and parameter surface over the typed config.
#[derive(Debug, Subcommand)]
pub enum StrategySub {
    /// List the declared strategy facets.
    List,
    /// Show one strategy facet: declared keys, activation, and the settled
    /// endpoint posture.
    Show {
        /// The facet to show.
        #[arg(value_enum)]
        facet: FacetArg,
    },
    /// Activate a strategy and settle its endpoint posture. Exactly one of
    /// `--endpoints` / `--endpoints-default` unless the facet already carries
    /// a settled choice.
    Activate {
        /// The facet to activate.
        #[arg(value_enum)]
        facet: FacetArg,
        /// The explicit endpoint set (comma-separated URLs).
        #[arg(long = "endpoints", value_name = "URLS", conflicts_with = "endpoints_default")]
        endpoints: Option<String>,
        /// Adopt the documented default endpoint set.
        #[arg(long = "endpoints-default")]
        endpoints_default: bool,
    },
    /// Deactivate a strategy (its recorded endpoint choice is kept).
    Deactivate {
        /// The facet to deactivate.
        #[arg(value_enum)]
        facet: FacetArg,
    },
    /// Set one declared facet key.
    Set {
        /// The facet to mutate.
        #[arg(value_enum)]
        facet: FacetArg,
        /// The config key name.
        key: String,
        /// The raw value.
        value: String,
    },
    /// Drop one key's override so the declared default applies again
    /// ("set the default if you have no preference").
    Default {
        /// The facet to mutate.
        #[arg(value_enum)]
        facet: FacetArg,
        /// The config key name.
        key: String,
    },
    /// The `default` verb's traditional spelling.
    Remove {
        /// The facet to mutate.
        #[arg(value_enum)]
        facet: FacetArg,
        /// The config key name.
        key: String,
    },
}

/// The strategy facet selector.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FacetArg {
    /// The settled-block strategy.
    Settlement,
    /// The pending-transaction strategy.
    Backrun,
}

/// The [`CliContext`] the argv overrides describe (ADR-051 D8).
#[must_use]
pub fn context<'a>(cli: &Cli, env: &'a dyn EnvVars) -> CliContext<'a> {
    let mut ctx = CliContext::new(env);
    if let Some(database) = &cli.database {
        ctx = ctx.with_database(database.clone());
    }
    if let Some(chain_id) = &cli.chain_id {
        ctx = ctx.with_chain_id(chain_id.clone());
    }
    if let Some(node_http) = &cli.node_http {
        ctx = ctx.with_node_http(node_http.clone());
    }
    if let Some(node_ws) = &cli.node_ws {
        ctx = ctx.with_node_ws(node_ws.clone());
    }
    if let Some(config) = &cli.config {
        ctx = ctx.with_config(config.clone());
    }
    ctx
}

/// Map argv into a [`Command`], reading the driver-domain env through the real
/// process environment.
///
/// # Errors
///
/// [`CliError`] when an arm needs a driver-domain value no layer supplied, or a
/// malformed `--chain-id` / `cordon_sim_intake_floor`.
pub fn resolve(cli: &Cli) -> Result<Command, CliError> {
    resolve_with_env(cli, &ProcessEnv)
}

/// Map argv into a [`Command`] over an injectable env seam (tests never mutate
/// the process environment).
///
/// # Errors
///
/// As [`resolve`].
///
/// # Panics
///
/// Never: a missing subcommand is a typed [`CliError::InvalidArgument`].
pub fn resolve_with_env(cli: &Cli, env: &dyn EnvVars) -> Result<Command, CliError> {
    let ctx = context(cli, env);
    let Some(command) = &cli.command else {
        return Err(CliError::InvalidArgument(
            "a subcommand is required".to_string(),
        ));
    };
    match command {
        Commands::Database { command } => Ok(Command::Database(database(command))),
        Commands::Exchange { command } => Ok(Command::Exchange(exchange(command))),
        Commands::Pool { command } => Ok(Command::Pool(pool(command))),
        Commands::Aave { command } => Ok(Command::Aave(aave(command, cli, &ctx)?)),
        Commands::Fleet { command } => Ok(Command::Fleet(fleet(command)?)),
        Commands::Path { command } => Ok(Command::Path(path(command))),
        Commands::Strategy { command } => Ok(Command::Strategy(strategy(command))),
    }
}

/// Print clap's own usage block for the no-subcommand-but-args case.
pub fn write_missing_subcommand_error() {
    let mut command = Cli::command();
    let usage = command.render_usage().to_string();
    let _ = writeln!(
        std::io::stderr().lock(),
        "error: a subcommand is required\n\n{usage}"
    );
}

fn database(command: &DatabaseSub) -> DatabaseCommand {
    match command {
        DatabaseSub::Backup => DatabaseCommand::Backup,
        DatabaseSub::Reset { force } => DatabaseCommand::Reset { force: *force },
        DatabaseSub::Upgrade { force } => DatabaseCommand::Upgrade { force: *force },
        DatabaseSub::Compact => DatabaseCommand::Compact,
        DatabaseSub::Cutover { dry_run, force } => DatabaseCommand::Cutover {
            dry_run: *dry_run,
            force: *force,
        },
        DatabaseSub::Heal { dry_run, force } => DatabaseCommand::Heal {
            dry_run: *dry_run,
            force: *force,
        },
        DatabaseSub::Inspect => DatabaseCommand::Inspect,
    }
}

fn exchange(command: &ExchangeSub) -> ExchangeCommand {
    match command {
        ExchangeSub::Activate { chain, name } => ExchangeCommand::Activate {
            chain: chain.clone(),
            name: name.clone(),
        },
        ExchangeSub::Deactivate { chain, name } => ExchangeCommand::Deactivate {
            chain: chain.clone(),
            name: name.clone(),
        },
        ExchangeSub::List { chain } => ExchangeCommand::List {
            chain: chain.clone(),
        },
    }
}

fn pool(command: &PoolSub) -> PoolCommand {
    match command {
        PoolSub::Update {
            chunk_size,
            to_block,
            verify,
            verify_all_interval,
        } => PoolCommand::Update {
            chunk_size: *chunk_size,
            to_block: to_block.clone(),
            verify_chunk: verify.chunk_gate(),
            verify_all: verify.all_gate(),
            verify_all_interval: *verify_all_interval,
        },
        PoolSub::Verify {
            rpc_url,
            pool_chain_id,
            block_number,
            pool,
            family,
            pool_manager,
        } => PoolCommand::Verify {
            rpc_url: rpc_url.clone(),
            chain_id: *pool_chain_id,
            block_number: *block_number,
            pool: pool.clone(),
            family: match family {
                FamilyArg::V3 => PoolFamily::V3,
                FamilyArg::V4 => PoolFamily::V4,
            },
            pool_manager: pool_manager.clone(),
        },
    }
}

fn aave(command: &AaveSub, cli: &Cli, ctx: &CliContext<'_>) -> Result<AaveCommand, CliError> {
    match command {
        AaveSub::Activate => Ok(AaveCommand::Activate {
            chain_id: chain_or_default(cli, ctx, 1)?,
        }),
        AaveSub::Deactivate { market_name } => Ok(AaveCommand::Deactivate {
            chain_id: chain_or_default(cli, ctx, 1)?,
            market_name: market_name.clone(),
        }),
        AaveSub::Update {
            chunk_size,
            to_block,
            verify,
            verify_all_interval,
            stop_after_one_chunk,
            dry_run,
            backup,
            no_backup,
        } => Ok(AaveCommand::Update {
            chunk_size: *chunk_size,
            to_block: to_block.clone(),
            verify_chunk: verify.chunk_gate(),
            verify_all: verify.all_gate(),
            verify_all_interval: *verify_all_interval,
            stop_after_one_chunk: *stop_after_one_chunk,
            dry_run: *dry_run,
            enable_backup: *backup && !*no_backup,
        }),
        AaveSub::Position { command } => match command {
            AavePositionSub::Show { address, market } => Ok(AaveCommand::PositionShow {
                address: address.clone(),
                market: market.clone(),
                chain_id: chain_or_default(cli, ctx, 1)?,
            }),
        },
    }
}

fn fleet(command: &FleetSub) -> Result<FleetCommand, CliError> {
    match command {
        FleetSub::Posture { command } => match command {
            FleetPostureSub::Show { socket } => Ok(FleetCommand::PostureShow {
                socket: socket.clone(),
            }),
            FleetPostureSub::Set {
                socket,
                cordon_enter_events,
                cordon_duty_percent,
                cordon_enter_window_ms,
                cordon_duty_window_ms,
                cordon_exit_clean_ms,
                cordon_sim_intake_floor,
            } => {
                let mut patch = Vec::new();
                if let Some(value) = cordon_enter_events {
                    patch.push(PosturePatchEntry::int("cordon_enter_events", *value));
                }
                if let Some(value) = cordon_duty_percent {
                    patch.push(PosturePatchEntry::float("cordon_duty_percent", *value));
                }
                if let Some(value) = cordon_enter_window_ms {
                    patch.push(PosturePatchEntry::int("cordon_enter_window_ms", *value));
                }
                if let Some(value) = cordon_duty_window_ms {
                    patch.push(PosturePatchEntry::int("cordon_duty_window_ms", *value));
                }
                if let Some(value) = cordon_exit_clean_ms {
                    patch.push(PosturePatchEntry::int("cordon_exit_clean_ms", *value));
                }
                if let Some(value) = cordon_sim_intake_floor {
                    patch.push(PosturePatchEntry::new(
                        "cordon_sim_intake_floor",
                        degenbot_cli_core::parse_sim_intake_floor(value)?,
                    ));
                }
                Ok(FleetCommand::PostureSet {
                    socket: socket.clone(),
                    patch,
                })
            }
        },
    }
}

fn path(command: &PathSub) -> PathCommand {
    match command {
        PathSub::Add {
            socket,
            hops,
            direction,
        } => PathCommand::Add {
            socket: socket.clone(),
            hops: hops.clone(),
            direction: direction.map(|direction| match direction {
                DirectionArg::Zfo => PathDirection::Zfo,
                DirectionArg::Ozf => PathDirection::Ozf,
            }),
        },
        PathSub::Discover { socket, bound } => PathCommand::Discover {
            socket: socket.clone(),
            bound: *bound,
        },
    }
}

fn strategy(command: &StrategySub) -> StrategyCommand {
    match command {
        StrategySub::List => StrategyCommand::List,
        StrategySub::Show { facet } => StrategyCommand::Show {
            facet: facet_of(*facet),
        },
        StrategySub::Activate {
            facet,
            endpoints,
            endpoints_default,
        } => StrategyCommand::Activate {
            facet: facet_of(*facet),
            endpoints: endpoints.clone(),
            endpoints_default: *endpoints_default,
        },
        StrategySub::Deactivate { facet } => StrategyCommand::Deactivate {
            facet: facet_of(*facet),
        },
        StrategySub::Set { facet, key, value } => StrategyCommand::Set {
            facet: facet_of(*facet),
            key: key.clone(),
            value: value.clone(),
        },
        StrategySub::Default { facet, key } => StrategyCommand::Default {
            facet: facet_of(*facet),
            key: key.clone(),
        },
        StrategySub::Remove { facet, key } => StrategyCommand::Remove {
            facet: facet_of(*facet),
            key: key.clone(),
        },
    }
}

fn facet_of(arg: FacetArg) -> StrategyFacet {
    match arg {
        FacetArg::Settlement => StrategyFacet::Settlement,
        FacetArg::Backrun => StrategyFacet::Backrun,
    }
}

/// The session chain id for the aave arms, which carry Python's Ethereum
/// default when NO chain layer (`--chain-id` or `DEGENBOT_DEFAULT_CHAIN_ID`)
/// supplied a value. A layer that IS present and malformed stays an error.
fn chain_or_default(cli: &Cli, ctx: &CliContext<'_>, default: u64) -> Result<u64, CliError> {
    let cli_layer = cli
        .chain_id
        .as_deref()
        .is_some_and(|value| !value.is_empty());
    let env_layer = ctx
        .env()
        .get(DEFAULT_CHAIN_ID_ENV)
        .is_some_and(|value| !value.is_empty());
    if cli_layer || env_layer {
        return ctx
            .chain_id()
            .map(|resolved| resolved.value)
            .map_err(CliError::from);
    }
    Ok(default)
}
