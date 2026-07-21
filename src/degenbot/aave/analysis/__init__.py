"""Aave V3 position analysis — thin Python driver over the Rust core.

The pure math (health-factor / LTV / eMode / isolation / scaled-balance calc)
lives in the ``degenbot-aave`` Rust crate (``analysis`` module). This package
is a thin I/O orchestration shell: ``orchestrator.py`` resolves the DB path,
fetches users / positions / prices via Rust-backed readers, and drives the
per-user ``analyze_aave_user_position`` PyO3 seam.
"""
