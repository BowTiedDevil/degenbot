# Architecture
- Domain packages: `src/degenbot/uniswap/`, `src/degenbot/curve/`, `src/degenbot/aave/`
  - Read the `AGENTS.md` files in these directories for module-specific direction
- Core utilities: `src/degenbot/exceptions/`, `src/degenbot/logging.py`
- Types: Domain-specific `types.py` files
- Database: `src/degenbot/database/models/`, `src/degenbot/migrations/`

# Organization
- Group related functionality in packages by domain (`uniswap`, `curve`, `aave`)
- Keep library functions separate from stateful classes
- Use type alias modules (`types.py`, `v2_types.py`, `v3_types.py`) for shared types
- Place specific logic in domain packages, cross-cutting concerns in core (`src/degenbot/`)
