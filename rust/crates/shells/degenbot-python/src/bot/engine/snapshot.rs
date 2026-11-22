//! `PyO3` wrapper for the engine stage surface — snapshot `#[pymethods]` slice.
//!
//! `PyO3` allows multiple `#[pymethods] impl PyArbEngine { … }` blocks per
//! type, so each concern file contributes one slice. This slice carries no
//! methods: the snapshot-seed surface (`snapshot_seed_block` /
//! `set_snapshot_seed_block`) lives on the `super` slice in `mod.rs`, and
//! the DB snapshot arm is the crawl slice's `PySnapshotArm`.
