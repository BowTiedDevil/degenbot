# ADR-062: One operator file, four layers — node endpoints, chain id, and database path resolve through the typed config on every entry path

**Status: accepted** (2026-09-25; revised the same day after a design review —
three transports, capability-scoped resolution, and a deletion of the Python
config model; ratified after a citation-by-citation verification pass against the
tree). Amends **ADR-051 D8** (the driver-domain resolvers keep their
`Source` tagging and provenance; its "these resolvers deliberately do not re-add
file vocabulary" clause is superseded below). Reverses the pre-0.6
file-vocabulary retirement of `[rpc]`, `[ws]`, `[database]`, and
`default_chain_id` recorded in `docs/config-migration.md`; `[otel]` stays
retired. Companion: **ADR-063** (secrets stay in the environment). Successor:
**ADR-065** (the verdict is the single configuration authority) records the
enforcement of the cascade this decision defines. Predecessors:
ADR-051 (the console owns the driver domain), ADR-052 (the database heals itself
at open, so its path is configuration, not migration), ADR-053 (FFI stubs are
generated from Rust), ADR-006 D5 (one `Bot` per chain), ADR-040 D3
(`[failure_policy]` is a free-form table read from the same file).

## Context

The operator file `$XDG_CONFIG_HOME/degenbot/config.toml` (or its
`DEGENBOT_CONFIG` override) should be the one place an operator writes a
deployment down. Today it is one file with three authorities over it, and the
disagreement is invisible until a bot refuses to start.

| Concern | Rust (`degenbot-config`) | Python (`degenbot.config`) |
|---|---|---|
| File location | `DEGENBOT_CONFIG` else XDG config home (`loader.rs:standard_file_path_with`) | `XDG_CONFIG_HOME`/`HOME` only — **`DEGENBOT_CONFIG` is ignored** (`config.py:33-69`) |
| Typed keys | `config_schema!` → `BotConfig`, installed process-wide by the FFI module init (`degenbot-python/src/lib.rs:280-295`) | a second pydantic model re-parsing the same file (`config.py:126-215`, `_init_config`) |
| Node endpoints | `--node-*` > `DEGENBOT_RPC_{HTTP,WS}_CHAINID_<id>` > error; "no file-table layer" (`resolvers.rs:280-385`) | CLI > env > `fallback_*` > `config.toml rpc[id]`/`ws[id]` > error (`config.py:327-463`) |
| Chain id | `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`; the file key is "deliberately not consulted" | env only |
| Database path | `--database` > `DEGENBOT_DB_PATH` > state-home default | `DatabaseSettings.path` from its own model |

The `[rpc]`/`[ws]`/`[database]`/`default_chain_id` retirement (Option B, the
JLFE2F cutover) was coherent when the typed loader was the only consumer: the
Python model kept reading them, so the file vocabulary was never lost. It stops
being coherent the moment a *file* carrying those tables is booted by the Rust
loader, which refuses it and exits 2 (`RETIRED_LAYOUT_ITEMS`, `loader.rs:80-100`,
`loader.rs:385-395`). The Python cascade's file layer is therefore unreachable in
any real bot, and the cost of the split shows up as workarounds:

- `runner/bot_runner.py:79-115` `_make_arbitrage_config` resolves a URI, then
  **rebuilds** a `DegenbotConfig` with `rpc={1: node_http}` to inject a value the
  same process already read from a file.
- `rust/examples/settlement_bot/src/main.rs:300-372` hand-rolls
  `read_config_toml` + `cascade_rpc_uri` + `resolve_db_path` that read
  `[rpc]`/`[ws]`/`database.path` raw — the desired behavior exists in-tree as
  duplicated example code the typed loader forbids.
- `degenbot-strategy/src/backrun_driver/driver_boot.rs:227` resolves a node join
  from the process environment with no CLI argument and no file.
- `docs/architecture/rust-settlement-bot-parity.md` still records the cascade as
  driver-side policy: S15 classifies it `KEEP-DRIVER` ("the typed Rust loader does
  not own these arbitration keys… no ADR assigns Rust a shared env-file
  contract"), and rows 2–4 scope the Rust cascade to its CLI and env layers only
  (`DRIVER-POLICY` / `REACHABLE`, "the file layer is retired, not reimplemented").
  No row reaches a shared file contract, so every consumer re-implements the
  file half.

The report that prompted this decision: an operator with their endpoints already
in `config.toml` has to export environment variables just to start a bot.

Two properties make a single endpoint slot too small, and both are settled:

**A local node is a first-class deployment.** Operators running a node beside the
bot should get the fastest transport available, and IPC is it. It is not a
second-class citizen in the transport layer: `alloy-transport-ipc` drives the
same `alloy_pubsub::ConnectionInterface` as the WebSocket transport over a Unix
socket or Windows named pipe, so it serves one-shot requests *and* subscriptions
identically. Our own `AlloyProvider` already branches on it (`is_ipc_path`,
`IpcConnect`, `connect_ipc_with_retries` in `degenbot-rpc/src/provider.rs:365`,
`:413`, `:979-993`), and the pump's subscription path
(`WsIngestor::connect` → `AlloyProvider::new`,
`degenbot-ingestion/src/ingestor.rs:62`) is already IPC-capable despite its
`node_ws` parameter name. What does *not* work yet is two hardcoded HTTP clients
(`driver_boot.rs:235`, `driver_loop.rs:1164`) — and we have no end-to-end IPC
test at all.

**Credentials are per-machine; the file is not.** A production endpoint carries an
API key, while the file is copied between machines and bind-mounted into the
devcontainer. ADR-063 owns that half (expansion of `${env:NAME}`); this ADR owns
the reporting half (redaction).

The ADR-051 D8 rationale was that file vocabulary must not silently re-enter a
fail-closed typed loader. That concern is answered here by *typing* the
vocabulary rather than forbidding it: a declared key with a validated kind cannot
be a silent fail-open, and the free-form escape stays explicitly enumerated (D11).

## Decision

**D1 — One file, four layers, on every entry path.** Node endpoints, the session
chain id, and the database path resolve identically for the Rust console, a
pure-Rust consumer, and a Python-launched bot:

| Rank | Layer | Node HTTP / WS / IPC | Chain id | Database |
|---|---|---|---|---|
| 1 | explicit override | `--node <uri>` or an in-process argument | `--chain-id` | `--database` |
| 2 | environment | `DEGENBOT_RPC_{HTTP,WS,IPC}_CHAINID_<id>` | `DEGENBOT_DEFAULT_CHAIN_ID` | `DEGENBOT_DB_PATH` |
| 3 | file | `[nodes]` tables in the operator file | `session.chain_id` | `database.path` |
| 4 | fail loud | `ConfigError` / `RpcNotConfiguredError` naming every layer consulted | | |

There is deliberately no `localhost` default. Every resolved value carries the
`Source` that supplied it, per entry. The rank order is the industry norm for
layered configuration (Django, Spring Boot, systemd, Compose, Kubernetes all put
an explicitly passed value above the ambient environment) and it is the only
ordering under which an in-process argument is not silently overruled by a stray
export.

**D2 — The node tables are typed schema vocabulary, not a free-form table.**
`degenbot-config` gains a `str_map` key kind (`BaseKind::StrMap`,
`BTreeMap<String, String>`, env encoding a comma-separated `key=value` list — the
shape `telemetry.diag` already uses) and `KeyDecl` gains an `env_prefix` for keys
whose env layer is a *family* of names. Four declared keys:

```toml
[nodes]
http = { 1 = "https://eth.example/rpc", 8453 = "https://base.example/rpc" }
ws   = { 1 = "wss://eth.example/rpc" }
ipc  = { 1 = "/run/user/1000/anvil.ipc" }

[session]
chain_id = 1

[database]
path = "~/.local/state/degenbot/db/degenbot.db"   # the declared state-home default
```

Both the nested-table form (`[nodes.http]` with `1 = "…"`) and the flat string form
(`http = "1=https://…,8453=https://…"`) are accepted. Per-entry provenance lands
in `LoadedConfig::entry_provenance`; the aggregate `provenance` entry records the
highest-ranked contributing layer. Validation is fail-closed: a chain key must
parse as `u64`; an `http` entry must be `http`/`https`; a `ws` entry `ws`/`wss`;
an `ipc` entry an `ipc://` URL or a filesystem path; `session.chain_id` a positive
integer.

*Rejected:* a free-form `[nodes]` table skipped by the loader and read by a
hand-rolled parser (the `settlement_bot` shape). It is the smallest diff and the
wrong one — the keys then miss doc generation, `readiness.rs`, and the writer, so
an operator can read endpoints from the file but never write them from the CLI,
and the next retirement lands in the same place. *Also rejected:* restoring the
pre-0.6 `[rpc]`/`[ws]` spelling, which would be a third name for the same concept
and still could not hold a third transport.

**D3 — Capability scopes decide which endpoint a consumer gets; layers outrank
transport preference.** Each transport key resolves through D1's four layers
independently, and each consumer declares the capability it needs:

| Scope | Transports it accepts | Preference |
|---|---|---|
| request (pool IO, reads, `eth_callMany`, tx submission) | ipc, ws, http | **ipc > ws > http** |
| subscription (the pump, head/logs streams) | ipc, ws | **ipc > ws** |

Two ordering rules follow, and the second is the one that is easy to get wrong:

1. **A value's capability is intrinsic.** An `ipc` entry can serve a request; an
   `http` entry never serves a subscription. The consumer's scope, not the
   operator's intent at the console, picks the key.
2. **Layers are resolved before transport preference is applied.** If *any*
   transport has a layer-1 or layer-2 value, the highest-preference transport
   *among those* wins; only when no explicit value exists anywhere do the file
   entries compete by preference. Without this rule a file carrying
   `ipc = { 1 = "…" }` would silently defeat an operator's
   `DEGENBOT_RPC_HTTP_CHAINID_1` export — the precise surprise this decision
   exists to remove, and the devcontainer depends on the rule holding.

**D4 — Chain id and database path join the same cascade, as ordinary typed
keys.** `session.chain_id` (`opt u64`) and `database.path` (`path`, keeping the
`$XDG_STATE_HOME` default the resolver already implements) are declared schema
keys with the existing env names, so the file layer is the same code path for all
three. Splitting them would recreate the split at a smaller scale — the parity
example duplicates all three, and an operator who can set the endpoint in the
file but not the chain id still exports a variable. The pre-0.6 top-level
`default_chain_id` stays **refused** with a pointed error naming
`session.chain_id`, and `[otel]` stays refused naming the `telemetry` section: a
rename is a cutover, not a compatibility shim.

**D5 — An in-process override is the `Source::Cli` layer.** `fallback_http` /
`fallback_ws` retire; the replacement is an explicit value that ranks exactly
where a command-line flag ranks. This reverses the documented Python precedence
(where `fallback_*` sat *below* env) and is a hard cutover: the old keyword names
are refused with a pointed error.

**D6 — One scheme-classified `--node` flag replaces `--node-http`/`--node-ws`.**
The flag is repeatable and the value classifies itself: `wss://` fills the `ws`
key, `http://` the `http` key, `ipc://` or a path the `ipc` key. A flag cannot
disagree with its own value, one rule covers a transport we have not thought of
yet, and no alias is kept for the old flags.

**D7 — One loader, one resolver, no re-derivation.** `degenbot-config` owns the
cascade end to end. The capability-scoped resolvers read the loaded maps and tag
the winner; they do not consult the environment themselves (the loader is the only
environment-reading crate). `CliContext` loads once and threads the result into
every resolver. The two modules that hardcode an HTTP client
(`driver_boot.rs:235`, `driver_loop.rs:1164`) take an **injected capability-scoped
provider at construction** instead — the same seam `WsIngestor::with_provider`
already offers — so "this module needs requests" becomes a constructor argument
rather than a hidden transport. The pure-Rust facade, the settlement-bot example,
and Python all call the same functions; the parity ledger rows reclassify to
`REACHABLE` with evidence links.

**D8 — Chain identity is a Rust invariant at endpoint binding.** The `eth_chainId`
check moves out of `provider/factory.py` into the core, so the console, the
pure-Rust consumer, and Python all get the fail-fast guard on a misconfigured
endpoint. It costs one round-trip per binding and a new error type across the FFI
boundary; it removes an ADR-006 D5 invariant that is currently enforced on only
one of the two consumers.

**D9 — The database is opened by the core, not created by Python.** The resolved
path is the core's to open; ADR-052 already gives Rust ownership of open-time
schema ensure and heal. Python's single production call to
`db_create_new_database` (today `config.py:530`, already a thin FFI passthrough)
is deleted, and the "Rust creates the database on first run" behavior gains its
own tests. Tests that assumed Python owns creation are expected to break and are
rewritten against the new owner.

**D10 — Python is a driver, not a second config authority.** `DegenbotConfig`,
`DatabaseSettings`, `OtelSettings`, `load_config_from_file`, and `_init_config` are
**deleted**; `otel` and `failure_policy` have no live Python reader (the failure
policy is read from the file by `degenbot-python` at module init). `Bot` and
`AsyncBot` take optional keyword overrides — `chain_id`, `node`, `database` —
each one an explicit override in the D5 slot, with resolution from the installed
typed config when absent. There is no module-level `degenbot.configure()`:
process-wide mutable boot state is untestable in a process that builds two bots,
and the repo's holder pattern exists precisely to keep process-wide state typed
and installed once. `CONFIG_DIR`/`CONFIG_FILE`/`_xdg_config_home` go with them,
which fixes Python's silent ignoring of `DEGENBOT_CONFIG`. The public-API churn is
accepted: `README.md` (five call sites), `docs/getting-started.md`,
`bot/_bot.py`, `provider/factory.py`, `runner/bot_runner.py`, and the config tests
are rewritten in the same change.

**D11 — The free-form list is closed and explicit.** `[failure_policy]`
(ADR-040 D3) stays free-form, and `[deployments]` joins it — the Python
deployment-registry overlay (`src/degenbot/registry/deployment_loader.py`) is
currently in neither the schema nor the sanctioned list, so a real deployment
file is refused at the typed boot today. Both are read as raw tables from the file
the loader selected. Every other section is typed or refused.

**D12 — Operators can write what they can read.** `degenbot config
get|set|unset nodes.http.1` (and the `session`/`database` siblings) write through
`degenbot-config`'s writer, which validates before touching the file and reports
a per-entry `Shadowed { env: DEGENBOT_RPC_HTTP_CHAINID_<id> }` when the
environment will win — the same "tell me what I just lost" behavior the rest of
the config stack already has. `degenbot config show --resolved` prints every
resolved value with its winning `Source`, and **never** prints credentials:
userinfo and credential-bearing query parameters are redacted in that output, in
error text, and in log lines, because a diagnostic's output is what ends up in a
scrollback, an issue, or a CI log. The file on disk keeps what the operator
wrote; `chmod 600` remains the storage-side advice. A container bind-mounting a
host operator file is out of scope for guards — it is a developer-environment
concern, not an end-user one.

**D13 — The cutover is hard and loud in both directions.** `[rpc]`, `[ws]`, and
`[database]` leave `RETIRED_LAYOUT_ITEMS`; `default_chain_id` and `[otel]` stay
with replacement text naming this ADR's keys. No shim translates old spellings.
`docs/config-migration.md`, the README configuration section, the devcontainer
note, and the parity ledger change in the same change as the code. A Python↔Rust
parity test resolves the same file and environment through both surfaces and
asserts identical URIs and identical winning layers, so the two authorities
cannot drift apart silently again.

**D14 — Gates.** `just test-rust` (full workspace), `just check-rust-consumer`,
`just check-rust-binding-default`, `just check-rust-extension-release`,
`just test-standalone`, `just dev && just verify-build-fresh`, and the targeted
pytest set. Three behavior tests are new gates rather than refactors: an
end-to-end **IPC** boot (today nothing in the suite dials a socket end to end),
"**Rust creates the database on first run**", and the Python↔Rust **parity**
assertion.

## Consequences

An operator writes one file. `config.toml` with `[nodes]`, `session.chain_id`,
and `database.path` boots the Rust console, a pure-Rust consumer, and a
Python-launched bot with no environment variables; `--node` or an in-process
argument still wins; the resolved value and its winning layer are printable
without leaking a key. A devcontainer keeps one file and overrides the endpoints
it must reach through the Podman host gateway, because layer 2 outranks layer 3
and a socket in the file no longer defeats an export.

`degenbot-config` grows a `str_map` kind, a prefix-enumerating `EnvVars` method,
and per-entry provenance. That is the price of making dynamic per-chain keys
first-class rather than special-cased in a resolver.

Python loses a public config type and its database-creation role. That is the
largest single break in this decision and it is deliberate: the alternative is
three answers to "where does an endpoint come from", which is the problem this
record exists to end.

Rust gains two responsibilities it did not have — chain-identity enforcement and
database creation — and one seam it should have had from the start: a
capability-scoped provider injected where a transport is needed.

What this does not do: it does not give the file a second writer, it does not
provide per-chain endpoint *lists* or failover (one endpoint per transport per
chain; a ladder is separate work), it does not relax the fail-loud posture when a
chain has no endpoint in any layer, and it does not add secret storage (ADR-063
covers portability, not a secret store).


## Amendment (2026-09-27): resolution as a value — the hypothetical door

`ResolvedConfig` holds `verdict: &'static Verdict` from a `OnceLock`, so the
install-once contract is enforced by the compiler: re-installing a global would
require the `OnceLock` to become an `RwLock` and the borrow an `Arc` across the
whole FFI surface. That convenience is not taken. A test that needs a second
cascade constructs one instead.

`degenbot._ffi.resolve_hypothetical(env, file)` is that constructor: a pure
function of its inputs
(`BotConfigLoader::new().with_env(...).with_config_path(file).load()`), projected
exactly as the verdict projects. It installs nothing and returns a
`HypotheticalConfig`; it cannot return a `ResolvedConfig`, which holds
`&'static`. The argument-taking cascade methods get the same treatment as
standalone siblings (`resolve_hypothetical_node_uri`, `..._chain_id`,
`..._database_path`) — envy's `from_env` / `from_iter` split.

Two doors follow, and using the wrong one is a tautology:

- `resolve_hypothetical(env, file)` for claims about HOW the cascade resolves
  inputs.
- `resolved_config()` for claims about WHAT this process installed.

The hypothetical lives on the raw FFI seam (ADR-013) and is deliberately not
re-exported from `degenbot.config`. The driver-domain leaves consume resolved
values threaded from the construction boundary — the Python companion to the
engine's instance-scoped `SolveRuntimeConfig` — never the verdict.

The install-once contract this amendment makes type-enforced has its first home
in [ADR-065](ADR-065-verdict-single-configuration-authority.md).
