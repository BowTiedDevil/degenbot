"""Retired snapshot converters.

This module is intentionally empty; the retired converter names it once
held are pinned absent by ``tests/ffi/test_ffi_registration_surface.py``.

Snapshot ingestion is now Rust-owned: the DB path loads inside
``Bot::load_snapshot_from_db``; the non-DB path reads per-pool tick data from
the held-tx DB arm or the chain arm (RPC) at registration.
"""
