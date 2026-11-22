//! Turns a conflated ATP+OSM feature pair -- one that
//! `crate::pipeline::conflate` has already determined refer to the same
//! real-world object -- into a suggested OpenStreetMap edit.
//!
//! Deliberately separate from `crate::matchers::Matcher`, even though
//! both are dispatched by category: matching needs different signals
//! per category to decide *whether* two features are the same object
//! (e.g. brand for shops; a restaurant or museum matcher would likely
//! need different signals again). But once a match is settled, deciding
//! *what to change* -- e.g. diffing `opening_hours` -- doesn't care how
//! the match was made, and that logic can be shared across categories
//! that matching itself needs to tell apart. So `EditSuggester`
//! implementations don't need to mirror `Matcher` implementations
//! one-for-one; there may end up being far fewer of them.

mod poi;
mod post_partner;

/// Suggests a tag-level edit for an OSM feature already matched to an
/// AllThePlaces feature. Operates on plain tag lists -- not `Place`/
/// `Feature` -- since by this point matching is done and only the tags
/// on both sides matter.
pub trait EditSuggester {
    /// Returns the changed/added tags to propose, or `None` if there's
    /// nothing worth suggesting. `position` is where the feature lies,
    /// as (longitude, latitude) in degrees, for reading OSM phone
    /// numbers in national format.
    fn suggest_edit(
        &self,
        atp_tags: &[(String, String)],
        osm_tags: &[(String, String)],
        position: Option<(f64, f64)>,
    ) -> Option<Vec<(String, String)>>;
}

/// Picks an [EditSuggester] for an AllThePlaces feature of `spider`
/// with `atp_tags`, or `None` if we don't (yet) generate edits for this
/// kind of feature. `relation` is the row's `match.relation`: only a
/// postal partner gets edits for the host it is `located_in`, since
/// OSM puts the partner's tags on its host.
pub fn create_edit_suggester(
    spider: &str,
    atp_tags: &[(String, String)],
    relation: &str,
) -> Option<Box<dyn EditSuggester>> {
    let tags = || atp_tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
    if crate::matchers::post_partner::is_post_partner(tags()) {
        return matches!(relation, "same" | "located_in").then(|| {
            Box::new(post_partner::PostPartnerEditSuggester::new(
                spider, atp_tags,
            )) as _
        });
    }
    if relation != "same" {
        return None;
    }
    let mut mask = crate::matchers::MatchMask::default();
    for (key, value) in atp_tags {
        mask.add_tag(key, value);
    }
    if mask.intersects(&crate::matchers::MatchMask::SHOP) {
        Some(Box::new(poi::PoiEditSuggester))
    } else {
        // TODO: Trees, infrastructure, ... once matching supports them.
        None
    }
}
