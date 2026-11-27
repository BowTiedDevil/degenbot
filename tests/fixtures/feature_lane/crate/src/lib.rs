//! Fixture crate for the feature-lane inventory tests.

#[cfg(test)]
#[cfg(feature = "obs")]
mod gated {
    #[test]
    fn gated_item() {}

    #[test]
    #[ignore = "manual: fixture-only ignored item"]
    fn ignored_item() {}

    #[test]
    fn plain_item() {}

    #[test]
    fn timing_probe() {}
}

#[cfg(all(test, feature = "obs", not(target_arch = "wasm32")))]
mod nested;
