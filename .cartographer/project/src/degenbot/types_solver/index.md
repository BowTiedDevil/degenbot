# types_solver

> [← back](../index.md)

├── [README.md](/Volumes/PNY/dev-projects/mev-arbitrum/vendor/degenbot/src/degenbot/types_solver/README.md) — `solver/driver/types/` — Python canonical mirror of `IExecut
├── [codec.py](/Volumes/PNY/dev-projects/mev-arbitrum/vendor/degenbot/src/degenbot/types_solver/codec.py) — ABI calldata encoders for Executor strategy entry points.
├── [executor.py](/Volumes/PNY/dev-projects/mev-arbitrum/vendor/degenbot/src/degenbot/types_solver/executor.py) — Frozen dataclasses + enum ordinals mirroring `IExecutor.sol`
├── [test_fixtures_lock.py](/Volumes/PNY/dev-projects/mev-arbitrum/vendor/degenbot/src/degenbot/types_solver/test_fixtures_lock.py) — Cross-language ABI lock — Python encoders MUST match `fixtur
└── [wire.py](/Volumes/PNY/dev-projects/mev-arbitrum/vendor/degenbot/src/degenbot/types_solver/wire.py) — JSON wire-format deserializers for cross-language fixtures.
