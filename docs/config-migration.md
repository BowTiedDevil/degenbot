# The operator config.toml and the four-layer cascade

**ADR-062 — one operator file, four layers (0.6, pre-release). The file is the BASE layer: it is where driver-domain settings live, and every retired item is refused at boot, never translated.**

## The file is the base layer

The operator file `$XDG_CONFIG_HOME/degenbot/config.toml` (else `~/.config/degenbot/config.toml`; or its `DEGENBOT_CONFIG` override) is the **typed Rust file layer** of `degenbot-config`'s `BotConfigLoader`, and it is the lowest-precedence layer of the cascade the driver-domain resolvers read. Every top-level table must name a declared schema section, and every key must be a declared key — the loader fails closed, aggregating every problem before reporting.

| Precedence | Layer | Supplies |
|---|---|---|
| 1 | explicit CLI | `--node <uri>`, `--chain-id`, `--database` |
| 2 | environment | `DEGENBOT_RPC_{HTTP,WS,IPC}_CHAINID_<chain>`, `DEGENBOT_DEFAULT_CHAIN_ID`, `DEGENBOT_DB_PATH` |
| 3 (base) | `--config` file | the `[nodes.*]` endpoint tables, `session.chain_id`, `database.path`, and every other typed section |
| 4 | declared default | the schema default (`database.path` → the XDG state home) |

`[nodes]` — the per-chain `http`, `ws`, and `ipc` tables — is the base layer, and the `DEGENBOT_RPC_*` env family and `--node` override it: an env entry for one chain outranks that chain's file entry alone, and an explicit `--node` outranks both. The same shape holds for the session chain id (`[session] chain_id` below `DEGENBOT_DEFAULT_CHAIN_ID` below `--chain-id`) and the database path (`[database] path` below `DEGENBOT_DB_PATH` below `--database`).

The authoritative key reference is generated from the schema — regenerate with `REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config` (lands in [`rust-config-keys.md`](rust-config-keys.md)). Every assignment is recorded with its winning layer, so nothing resolves anonymously.

## The base layer, spelled out

```toml
[nodes]
http = { 1 = "http://localhost:8545" }
ws = { 1 = "ws://localhost:8546" }

# The nested table form is equivalent; `ipc` is a socket path or an ipc:// URL.
[nodes.ipc]
1 = "/tmp/anvil.ipc"

[session]
chain_id = 1

[database]
path = "./degenbot.db"
```

A machine-local or bind-mounted file holds these values; the environment is where a single value diverges for one run.

## Replacement table for the pre-0.6 file vocabulary

The pre-0.6 file vocabulary was Python-driver domain the typed schema never carried. The driver-domain items it covered are now DECLARED file keys; the two layout items nothing replaced stay retired. There is no shim: the old spellings are refused, not translated.

| Pre-0.6 file spelling | Replacement |
|---|---|
| `[rpc]` per-chain endpoints (`[rpc]\n1 = "http://…"`) | the declared `[nodes.http]` table (env `DEGENBOT_RPC_HTTP_CHAINID_<chain>`, or `--node <uri>`, still override it per chain) |
| `[ws]` per-chain endpoints | the declared `[nodes.ws]` table (env `DEGENBOT_RPC_WS_CHAINID_<chain>`, or `--node <uri>`) |
| — (no pre-0.6 spelling) | the new `[nodes.ipc]` table (env `DEGENBOT_RPC_IPC_CHAINID_<chain>`, or `--node ipc://…`) |
| `[database]` `filepath` | the declared `[database] path` (env `DEGENBOT_DB_PATH`, or `--database`, still override it) |
| `[otel]` `endpoint` / `enabled` | **retired** — the modern `telemetry` section: `telemetry.otel` (toggle) and `telemetry.jaeger_endpoint` (OTLP endpoint) |
| top-level `default_chain_id` | **retired** — `[session] chain_id` in the file, or the `DEGENBOT_DEFAULT_CHAIN_ID` env name / `--chain-id` |

Boot behavior: a surviving pre-0.6 spelling fails the load and the process exits 2 with a message naming what to write instead —

```
bot configuration invalid (1 problem(s)):
  - --config /home/you/.config/degenbot/config.toml: unknown section [rpc]
```

for a table the schema never declared, and

```
bot configuration invalid (1 problem(s)):
  - --config /home/you/.config/degenbot/config.toml: retired config-layout item [otel] is no longer supported — the [otel] table is retired: use the modern telemetry section (telemetry.otel, telemetry.jaeger_endpoint); see docs/config-migration.md
```

for the two names that DID have a replacement. A stale `[database] filepath` is refused the same way as any other undeclared key: `unknown key filepath in section [database]`.

## In-schema key retirements (typed migrations)

Later hard cutovers retired SCHEMA keys the same way: a surviving env var
or TOML key fails the load loudly with a pointed message, for one release.

| Retired key | Cutover | Replacement |
|---|---|---|
| `solve.executor` / `DEGENBOT_SOLVE_EXECUTOR` | P6YXA6 | the worker fleet (per-bin hosting needs no executor selection) |
| `solve.lpt_partition` / `DEGENBOT_LPT_PARTITION` | P6YXA6 | bins are always LPT-pre-balanced |
| `fleet.stance` / `DEGENBOT_FLEET` | LW-T9 (ergo CQLMM2) | fleet is the only stance since LW-T9 — the worker fleet is the only behavior |
| `solve.solve_sim_inflight` / `DEGENBOT_SOLVE_SIM_INFLIGHT` | LW-T9 (ergo CQLMM2) | SimDriver capacity: `fleet.sim_slot_cap`; inline-sim sizing: `solve.inline_sim_workers` |

## What is NOT retired

`[failure_policy]` is deliberately **not** typed and **not** rejected: it is the ADR-040 D3 free-form per-bucket override table, read as a raw TOML table from the same file the loader selected (`BotConfigLoader::file_path()`). Files may keep it unchanged.

Known adjacent gap (same family as the retired Python-domain keys): the
`[deployments]` overlay table (Python deployment-registry overlay,
`src/degenbot/registry/deployment_loader.py`) is in neither the typed schema
nor the loader's free-form list, so a file carrying it is refused with the
generic "unknown section" error at the typed boot. It must join
`FREE_FORM_FILE_SECTIONS` (or the schema) before the overlay is usable in the
shared file.

## Example migration

Before (pre-0.6):

```toml
default_chain_id = 1

[rpc]
1 = "http://localhost:8545"

[database]
filepath = "./degenbot.db"

[otel]
endpoint = "http://localhost:4318"
```

After (0.6):

```toml
# The base layer now carries the endpoints, the session chain, and the
# database file — nothing has to be exported for them to take effect.
[nodes.http]
1 = "http://localhost:8545"

[session]
chain_id = 1

[database]
path = "./degenbot.db"

[telemetry]  # the [otel] table, renamed
otel = true
jaeger_endpoint = "http://localhost:4318"

[failure_policy]  # unchanged, still free-form
```

The Python driver's own cascade is unchanged: `BotConfig(rpc={1: "http://localhost:8545"}, default_chain_id=1)` still works, and its `resolve_rpc_uris` still reads the `DEGENBOT_RPC_*` names above the file (`src/degenbot/config.py`). Note the asymmetry for a SHARED file: the Python model still parses `[rpc]`/`[ws]`/`[database]`, but those spellings no longer boot the Rust CLI, so a file the Rust core accepts is read by Python for its Rust-domain sections only and delivers its endpoints through the env family above.

CAUTION (2026-09-10 incident): inside the degenbot devcontainer, do NOT export the
`DEGENBOT_RPC_*` names from a shell rc file (`.bashrc` etc.), and do not use
`localhost:8545` there. `devcontainer.json` `containerEnv` already bakes the
container-correct URIs — `http://host.containers.internal:8545` and
`ws://host.containers.internal:8546` — into the container environment, and the
environment outranks the bind-mounted host `config.toml`, so a later rc-file
export silently wins and points the bot at the container's own loopback, where
nothing listens (connection refused at the first `eth_chainId` call). Override
endpoints in-container via the CLI (`--node http://host.containers.internal:8545` /
`--node ws://host.containers.internal:8546`), which outranks the environment, or
by editing `devcontainer.json` and rebuilding.

### Credentials in the operator file

`config.toml` is a machine-local file, so treat it as a secret carrier: `chmod
600` it, keep keys out of it where a per-machine environment variable does the
job, and do not commit one. Interpolating an environment variable INTO the file
is a recorded follow-up (ergo MXFVVI's sibling decision, ADR-063) and is not
implemented — the file's values are read literally.

## Debugging a value that did not take effect

`degenbot config show --resolved` prints the full inventory the process resolves,
each key annotated with the layer that won it (`cli` > `env` > `file` > `default`),
with `(unresolved)` where no layer supplied it — a per-chain entry reads
`nodes.ws[8453] = … (env)` when an export shadowed the file entry, which is the
first place to look when a value seems to be ignored. `degenbot config show` is
the file layer alone, and `degenbot config path` prints which file the cascade
reads. All three are read-only; a mutating `config get|set|unset` surface is a
recorded follow-up (ergo MXFVVI) and does not exist yet.

## Database upgrades

A stale Alembic-marked database now heals **automatically at open** (ADR-052):
`ensure_schema` runs the ADR-011 out-of-place heal on any `alembic_version`
database — head-stamped or stale — preserves the old file as `*.bak`, and
proceeds Rust-owned. There is no `degenbot database upgrade` command: it renders
a pointed retirement error and exits non-zero, while `degenbot database heal`
remains the explicit repair entry point. Set `DEGENBOT_DB_AUTO_HEAL=0` to
disable heal-at-open for pinned environments.
