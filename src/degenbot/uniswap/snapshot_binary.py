"""Retired snapshot converters.

This module is intentionally empty; it is kept as an import target for
``tests/rust/test_per_pool_snapshot_ingestion_removed.py``.

Snapshot ingestion is now Rust-owned: the DB path loads inside
``Bot::load_snapshot_from_db``; the non-DB path reads per-pool tick data from
the held-tx DB arm or the chain arm (RPC) at registration.
"""
