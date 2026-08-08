//! The brute-force [`ScanTopK`] baseline implementation of [`OrderIndex`].
//!
//! Kept as the reference/redundant path: it re-ranks all results per query, so
//! it is correct but O(N) per `top_k`/`best`. Used to (a) give an independent
//! implementation for differential testing against the envelope, and (b) serve
//! as the fallback when the `envelope` Cargo feature is off.

use alloy_primitives::{I256, U256};
use std::fmt::Debug;

use crate::order_index::{clamp_gas, clamp_gross, net_of, IdKey, OrderIndex};

/// A straightforward, correct order index: store results and re-rank on query.
#[derive(Clone, Debug, Default)]
pub struct ScanTopK<Id> {
    points: Vec<(Id, U256, U256)>, // (id, gas, gross)
}

impl<Id: IdKey> OrderIndex<Id> for ScanTopK<Id> {
    fn insert(&mut self, id: Id, gas: U256, gross: U256) {
        if let Some(p) = self.points.iter_mut().find(|(i, ..)| *i == id) {
            p.1 = clamp_gas(gas);
            p.2 = clamp_gross(gross);
        } else {
            self.points.push((id, clamp_gas(gas), clamp_gross(gross)));
        }
    }

    fn remove(&mut self, id: &Id) -> bool {
        if let Some(pos) = self.points.iter().position(|(i, ..)| i == id) {
            self.points.swap_remove(pos);
            true
        } else {
            false
        }
    }

    fn update(&mut self, id: Id, gas: U256, gross: U256) -> bool {
        if let Some(p) = self.points.iter_mut().find(|(i, ..)| *i == id) {
            p.1 = clamp_gas(gas);
            p.2 = clamp_gross(gross);
            true
        } else {
            false
        }
    }

    fn best(&self, x: U256) -> Option<Id> {
        // `rank` orders (net desc, id asc); the first element is the best, so
        // take the minimum under `rank`.
        self.points
            .iter()
            .map(|(id, gas, gross)| (net_of(*gross, *gas, x), *id))
            .min_by(|a, b| rank(a, b))
            .map(|(_, id)| id)
    }

    fn top_k(&self, x: U256, k: usize) -> Vec<Id> {
        if k == 0 {
            return Vec::new();
        }
        let mut ranked: Vec<(I256, Id)> = self
            .points
            .iter()
            .map(|(id, gas, gross)| (net_of(*gross, *gas, x), *id))
            .collect();
        ranked.sort_by(|a, b| rank(a, b));
        ranked.truncate(k);
        ranked.into_iter().map(|(_, id)| id).collect()
    }

    fn len(&self) -> usize {
        self.points.len()
    }

    fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// Order two `(net, id)` rows: net descending, then id ascending (total order).
fn rank<Id: Ord>(a: &(I256, Id), b: &(I256, Id)) -> core::cmp::Ordering {
    b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
}
