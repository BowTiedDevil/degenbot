//! The stateless-swap-simulation capability: the per-family simulators, the
//! `PoolEntry` dispatch row and its registry, the seam traits whose
//! implementations live one layer up (the `std::io::Read` precedent), and the
//! registration-refusal vocabulary (ADR-084 W5b).

pub mod curve_data_provider;

pub mod curve_dy_io;

pub mod curve_strategies;

pub mod pool;

pub mod rate_provider;

pub mod registration_refusal;

pub mod registry;

pub mod simulate_swap;

pub mod tick_fetch;
