# Migrating the operator config.toml to the typed BotConfig layout

**Ergo JLFE2F — Option B hard cutover (0.6, pre-release). No shim layer: retired items are refused at boot, not translated.**

## What changed

The operator file `$XDG_CONFIG_HOME/degenbot/config.toml` (else `~/.config/degenbot/config.toml`; or its `DEGENBOT_CONFIG` override) is now the **typed Rust file layer** of `degenbot-config`'s `BotConfigLoader`. Every top-level table must name a declared schema section, and every key must be a declared key — the loader fails closed, aggregating every problem before reporting.

The pre-0.6 file vocabulary (`[rpc]`, `[ws]`, `[database]`, `[otel]`, top-level `default_chain_id`) was Python-driver domain the typed schema never carried. Those items are **retired**: a file containing them is refused at boot with a pointed error naming its replacement.

## Replacement table

| Retired item | Replacement |
|---|---|
| `[rpc]` per-chain endpoints (`[rpc]\n1 = "http://…"`) | `DEGENBOT_RPC_HTTP_CHAINID_<chain>` env names (or the Python config cascade, `src/degenbot/config.py`) |
| `[ws]` per-chain endpoints | `DEGENBOT_RPC_WS_CHAINID_<chain>` env names (or the Python config cascade) |
| `[database]` `filepath` | Python config cascade (`src/degenbot/config.py`, `DatabaseSettings`) |
| `[otel]` `endpoint` / `enabled` | the modern `telemetry` section: `telemetry.otel` (toggle) and `telemetry.jaeger_endpoint` (OTLP endpoint) |
| top-level `default_chain_id` | `DEGENBOT_DEFAULT_CHAIN_ID` env — the Python config cascade (`src/degenbot/config.py`) |

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

## The new layout

Every schema section doubles as a config-file table. The authoritative key reference is generated from the schema — regenerate with `REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config` (lands in [`rust-config-keys.md`](rust-config-keys.md)). Layer precedence: CLI override > `DEGENBOT_*` env > config file > declared default, with every assignment recorded in provenance.

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
[telemetry]
otel = true
jaeger_endpoint = "http://localhost:4318"

[failure_policy]  # unchanged, still free-form
```

with the Python-domain settings supplied through the environment (`DEGENBOT_RPC_HTTP_CHAINID_1=http://localhost:8545` — a HOST-machine example; in the devcontainer, see the CAUTION below) or the Python config cascade (`BotConfig(database=…, rpc={1: "http://localhost:8545"}, default_chain_id=1)`). The `degenbot` CLI reads the session chain id from `DEGENBOT_DEFAULT_CHAIN_ID` (e.g. `DEGENBOT_DEFAULT_CHAIN_ID=1` for mainnet — a HOST-machine example like the RPC names above).

CAUTION (2026-09-10 incident): inside the degenbot devcontainer, do NOT export these
`DEGENBOT_RPC_*` names from a shell rc file (`.bashrc` etc.), and do not use
`localhost:8545` there. `devcontainer.json` `containerEnv` already bakes the
container-correct URIs — `http://host.containers.internal:8545` and
`ws://host.containers.internal:8546` — into the container environment, and
`resolve_rpc_uris` reads `os.environ`, so a later rc-file export silently wins and
points the bot at the container's own loopback, where nothing listens (connection
refused at the first `eth_chainId` call). Override endpoints in-container via the
CLI (`--node-http` / `--node-ws`) or by editing `devcontainer.json` and rebuilding.

Boot behavior: a surviving retired item fails the load and the process exits 2 with a message like

```
bot configuration invalid (1 problem(s)):
  - --config /home/you/.config/degenbot/config.toml: retired config-layout item [rpc] is no longer supported — move per-chain RPC endpoints to the DEGENBOT_RPC_HTTP_CHAINID_<chain> env names (or the Python config.py cascade); see docs/config-migration.md
```

## Database upgrades

A stale Alembic-marked database now heals **automatically at open** (ADR-052):
`ensure_schema` runs the ADR-011 out-of-place heal on any `alembic_version`
database — head-stamped or stale — preserves the old file as `*.bak`, and
proceeds Rust-owned. There is no `degenbot database upgrade` command: it renders
a pointed retirement error and exits non-zero, while `degenbot database heal`
remains the explicit repair entry point. Set `DEGENBOT_DB_AUTO_HEAL=0` to
disable heal-at-open for pinned environments.
