"""Retired snapshot converters (DADWUP / XEANMB).

This module is intentionally empty; it is kept as an import target for
``tests/rust/test_per_pool_snapshot_ingestion_removed.py``.

History:

- DADWUP retired the per-pool SQLAlchemy ``yield_per`` ingestion loops
  (``stream_*_snapshot_to_engine`` / ``insert_*_pool_snapshot``).
- XEANMB retired the whole-dict ``load_*_from_py`` engine surface and, with
  the in-memory ``SnapshotStore``, the ``_v3_snapshot_to_py_dict`` /
  ``_v4_snapshot_to_py_dict`` converters that fed it.

Snapshot ingestion is now Rust-owned: the DB path loads inside
``Bot::load_snapshot_from_db``; the non-DB path reads per-pool tick data from
the held-tx DB arm or the chain arm (RPC) at registration.
"""
