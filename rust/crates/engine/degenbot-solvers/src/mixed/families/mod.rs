//! Per-family modules (ADR-071 D1): one module per pool family owning
//! everything the family contributes to a solve and its bound.

pub(crate) mod balancer_stable;
pub(crate) mod balancer_weighted;
pub(crate) mod cl;
pub(crate) mod curve_stableswap;
pub(crate) mod solidly;
