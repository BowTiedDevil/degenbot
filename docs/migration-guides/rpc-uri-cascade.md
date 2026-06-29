# RPC URI Configuration Cascade

> **Purpose.** Resolving an HTTP/WS RPC endpoint by chain id used to happen
> ad-hoc inside the backrun example (`examples/eth_backrun_helpers.py`), which
> composed a URI from a split `NODE_HOST_*`/`NODE_PORT_*` pair read **only** from
> `examples/mainnet.env`, with a hardcoded `http://localhost:8545` fallback. In
> a fresh devcontainer (no `mainnet.env`) that fallback silently produced an
> invalid `localhost` endpoint and crashed. This guide documents the standard
> cascade that replaced it: one shared resolver in the library, with a CLI
> flag, OS env, and config.toml all wired as real priority layers.

## The resolver

`degenbot.config.resolve_rpc_uris` is the single resolution path for an
HTTP/WS endpoint by chain id. Library code, the `degenbot` click CLI, and the
backrun example all delegate to it.

```python
from degenbot.config import resolve_rpc_uris, RpcNotConfiguredError

http, ws = resolve_rpc_uris(
    1,                       # chain id (Ethereum mainnet)
    cli_http="--node-http",  # optional, highest priority
    cli_ws="--node-ws",
    fallback_http=...,       # optional, below OS env
    fallback_ws=...,
)
```

## Resolution order (per URI, independently)

The two URIs (`http`, `ws`) each walk the cascade **independently** — one may
come from a CLI flag while the other comes from config.toml. There is **no
`localhost` default**: a chain with no configured endpoint in any layer raises
`RpcNotConfiguredError` (a `ValueError` subclass) so the misconfiguration
surfaces immediately.

| Priority | HTTP source | WS source |
|----------|-------------|-----------|
| 1 (highest) | `cli_http` arg | `cli_ws` arg |
| 2 | OS env `DEGENBOT_RPC_HTTP_CHAINID_{cid}` | OS env `DEGENBOT_RPC_WS_CHAINID_{cid}` |
| 3 | `fallback_http` (caller-supplied) | `fallback_ws` (caller-supplied) |
| 4 | config.toml `rpc[cid]` | config.toml `ws[cid]` |
| 5 (none) | raise `RpcNotConfiguredError` | raise `RpcNotConfiguredError` |

The chain-id discriminator in the envvar name (`..._CHAINID_1`,
`..._CHAINID_42161`, …) makes the chain binding explicit and surfaces a
missing/wrong chain as a clean error instead of a silent cross-chain
fallback (ADR-006 — one Bot per chain; the chain identity lives in
`config.default_chain_id` and is enforced at construction).

### Layer notes

- **CLI** — the backrun example exposes `--node-http` / `--node-ws`. The
  library `resolve_rpc_uris(chain_id, cli_http=…)` is the seam any caller uses.
- **OS env** — read via `os.environ`, so a plain `export` in the devcontainer
  (or a shell, or CI) takes effect. The `.env`-file dotenv dict is **not**
  consulted by the resolver; the example's `mainnet.env` only feeds the legacy
  layer (below).
- **Caller fallback** — the slot the backrun example uses to inject the
  deprecated `NODE_HOST_*` rebuilt URIs without leaking that naming into the
  library.
- **config.toml** — `~/.config/degenbot/config.toml`, parsed by
  `load_config_from_file`. URIs are carried as pydantic `HttpUrl`/
  `WebsocketUrl`; `str()` of those is returned (empty path normalizes to a
  trailing `/`, e.g. `http://host:8545/` — fine for `web3` providers).

## Quick start (devcontainer)

Pick **one** of the layers:

```bash
# 1. OS env (recommended for devcontainers / CI)
export DEGENBOT_RPC_HTTP_CHAINID_1=http://host.containers.internal:8545
export DEGENBOT_RPC_WS_CHAINID_1=ws://host.containers.internal:8546

# 2. CLI flag (ad-hoc, per-run)
uv run python examples/eth_backrun_v2_v3_v4_rust.py \
    --node-http http://host.containers.internal:8545 \
    --node-ws ws://host.containers.internal:8546

# 3. config.toml (~/.config/degenbot/config.toml)
[rpc]
1 = "http://host.containers.internal:8545"
[ws]
1 = "ws://host.containers.internal:8546"
```

When none of the layers provide an endpoint, the run fails fast:

```
RpcNotConfiguredError: No HTTP RPC endpoint configured for chain 1. Set
DEGENBOT_RPC_HTTP_CHAINID_1 in the environment, pass --node-http, supply a
fallback, or add an `rpc` chain-id entry to .../config.toml (config.toml layer).
```

## Deprecated: `NODE_HOST_*` / `NODE_PORT_*`

The split host+port form (`NODE_HOST_HTTP`, `NODE_PORT_HTTP`,
`NODE_HOST_WEBSOCKET`, `NODE_PORT_WEBSOCKET`) is retained in the backrun
example as a **deprecated** fallback: when a host is present, the example
rebuilds the full URI and passes it as the resolver `fallback_*` slot (priority
3 — below OS env, above config.toml), emitting `DeprecationWarning`.

To migrate, replace the split form with a full-URI OS envvar (or a config.toml
entry, or `--node-http`):

```diff
- NODE_HOST_HTTP=https://eth.example.com
- NODE_PORT_HTTP=8545
- NODE_HOST_WEBSOCKET=wss://ws.eth.example.com
- NODE_PORT_WEBSOCKET=8546
+ export DEGENBOT_RPC_HTTP_CHAINID_1=https://eth.example.com
+ export DEGENBOT_RPC_WS_CHAINID_1=wss://ws.eth.example.com
```

Because `pytest` runs with `filterwarnings = ["error", ...]`, any test path
exercising the deprecated layer must either use `pytest.warns(DeprecationWarning)`
or set the new envvar instead.

## Reference

- Resolver + error type: `src/degenbot/config.py` — `resolve_rpc_uris`,
  `RpcNotConfiguredError`.
- Example adoption: `examples/eth_backrun_helpers.py` — `BackrunConfig.from_env`
  (`chain_id`, `cli_http`, `cli_ws` kwargs; deprecated `NODE_HOST_*` fallback).
- CLI flags: `examples/eth_backrun_v2_v3_v4_rust.py` — `_build_arg_parser`
  (`--node-http` / `--node-ws`).
- ADR-006 (`docs/adr/0006-bot-as-per-chain-orchestrator.md`) — per-chain Bot
  orchestrator; the chain-id-discriminated envvar makes the chain binding
  explicit.