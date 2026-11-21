//! Spiders whose data is known to be wrong in a way that would mislead
//! mappers, left out when `import_atp` reads the AllThePlaces dump
//! until AllThePlaces fixes them. `spider-stats` counts their features
//! as `held_back`. Remove a spider here once its issue is resolved.

/// Each held-back spider, with the AllThePlaces issue about its data.
const HELD_BACK: &[(&str, &str)] = &[
    // 89% of its devices are other operators' chargers on ChargePoint's
    // roaming map, but all of them say `brand=ChargePoint`.
    (
        "chargepoint",
        "https://github.com/alltheplaces/alltheplaces/issues/20717",
    ),
];

/// Whether `spider` is held back.
pub(crate) fn is_held_back(spider: &str) -> bool {
    HELD_BACK.iter().any(|(s, _)| *s == spider)
}

/// Logs the held-back spiders, so that the pipeline log says why
/// their features are missing.
pub(crate) fn log() {
    for (spider, issue) in HELD_BACK {
        log::info!(spider = *spider, issue = *issue; "import_atp: held back spider");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_is_held_back() {
        assert!(super::is_held_back("chargepoint"));
        assert!(!super::is_held_back("electrify_america_us"));
    }
}
