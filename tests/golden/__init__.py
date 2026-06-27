"""Golden-value scaffold for on-chain parity tests.

See ``docs/architecture/golden-onchain-parity.md``.

Two layers:
- L2 (oracle truth) — :mod:`tests.golden.oracle` (record/replay ints).
- L1 (pool-state cassette) — reuse :class:`degenbot.provider.OfflineProvider`
  from pre-recorded ``tests/fixtures/chain_data`` files; a general recorder is
  a tracked follow-up, not part of this scaffold.
"""
