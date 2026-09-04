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
uv sync    # or: pip install -e .
```

**Rust only** (no Python machinery in the build graph):

```bash
cargo add degenbot
```

## Five-minute tour

The `Bot` class is the central session object. It manages connections and registries, provides factory methods for pools and tokens, and enforces chain-id consistency between your RPC endpoints and configuration:

```python
import degenbot
from degenbot.config import DegenbotConfig

bot = degenbot.Bot(
    config=DegenbotConfig(
        default_chain_id=1,
        rpc={1: "https://your-archive-node"},
        database={"path": "~/.config/degenbot/degenbot.db"},
    )
)

# Bot constructs the RPC provider from config and checks its eth_chainId
# matches default_chain_id (fail-fast) — no manual provider registration.

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
