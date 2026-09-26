# Getting started

## Requirements

- Python 3.12+
- A package manager (`uv` preferred; `pip` works)
- A funded RPC endpoint (archive node recommended; live mainnet data is used at construction time)
- (Rust consumers only) a Rust toolchain

## Install

**From PyPI** — the Python driver:

```bash
pip install degenbot
```

**From source:**

```bash
git clone https://github.com/BowTiedDevil/degenbot.git
cd degenbot
just bootstrap    # or: pip install -e . for release-equivalent defaults
```

**Rust only** (no Python machinery in the build graph):

```bash
cargo add degenbot
```

## Configure once, override per run

The operator file `$XDG_CONFIG_HOME/degenbot/config.toml` (else
`~/.config/degenbot/config.toml`, or the `DEGENBOT_CONFIG` override) is the base
layer of the four-layer cascade (`cli` > `env` > `file` > `default`). Declare
the endpoints, the session chain, and the database path there once:

```toml
[nodes]
http = { 1 = "https://your-archive-node" }
ws = { 1 = "wss://your-archive-node/ws" }

[session]
chain_id = 1

[database]
path = "~/.local/state/degenbot/db/degenbot.db"
```

Explicit `Bot(...)` keywords are the override layer and beat the file; the
`DEGENBOT_RPC_{HTTP,WS,IPC}_CHAINID_<chain>` and `DEGENBOT_DB_PATH` environment
names sit between them, so an env entry overrides one chain or key at a time.
The `degenbot` console spells the same node override `--node <uri>`, which is
self-classifying (`http://`, `ws://`, or `ipc://`); `degenbot config show
--resolved` prints the winning layer for every value.

## Five-minute tour

The `Bot` class is the central session object. It manages connections and registries, provides factory methods for pools and tokens, and enforces chain-id consistency between your RPC endpoints and configuration:

```python
import degenbot

bot = degenbot.Bot(
    chain_id=1,
    node="https://your-archive-node",
    database="~/.local/state/degenbot/db/degenbot.db",
)

# Bot constructs the RPC provider from the node endpoint and checks its
# eth_chainId matches chain_id (fail-fast) — no manual provider registration.

# Create pools and tokens through Bot (I/O-free where possible)
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
token = bot.build_erc20token("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")  # WETH

# Pools are I/O-free: all state is injected at construction, so swap
# calculations run with no network calls.
amount_out = pool.calculate_tokens_out_from_tokens_in(
    token_in=pool.token0,
    token_in_quantity=10**18,
)
```

## The rule that surprises people

Pool classes are Python companions over **Rust-owned pool state**. Direct construction is impossible — any constructor call raises `TypeError` — because a pool only comes into being by registering with a `Bot`'s Rust state:

```python
# This ALWAYS raises TypeError:
degenbot.UniswapV3Pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")

# Do this instead:
pool = bot.build_pool("0x8ad599c3A0ff1De082011EFDDc58f1908EB6e6D8")
```

## Where to go next

- **Architecture** — {doc}`/architecture/io-free-pools` (the foundation), {doc}`/architecture/block-state-machine`, {doc}`/architecture/rust-owned-bot`
- **Design rationale** — {doc}`/adr/index` (start with ADR-003 and ADR-005)
- **CLI reference** — {doc}`/cli/pool`, {doc}`/cli/database`, {doc}`/cli/aave`
- **Rust API** — [docs.rs/degenbot](https://docs.rs/degenbot)
- **Python docstrings** — every public class/method is documented in the compiled module (`help(degenbot.Bot.build_pool)` in a REPL); the API reference page on this site is a follow-up built from the type stubs
