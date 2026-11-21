use super::brand_domains::BrandDomains;
use super::brand_names::{self, NsiNames};
use super::category::{category_factor, main_category};
use super::contact::{AtpContacts, CONTACT_FACTOR, Nondescript};
use super::former_brands;
use super::official_id::{self, IdEvidence};
use super::{
    MatchMask, Matcher, OsmCandidate, Rules, address, geo_point_distance, geo_point_distance_score,
    meters_to_chord_angle, normalize_name, parse_wikidata_ids, split_values, street,
};
use crate::places::Place;
use crate::utils::countries;
use geo::{Centroid, Closest, ClosestPoint};
use s2::{cell::Cell, cellid::CellID, latlng::LatLng, point::Point, s1::ChordAngle};

/// Finds the first `brand:wikidata` tag in `tags` and parses its
/// value, which may list several `;`-separated IDs -- e.g. a car
/// dealer tagged `brand=BMW;Mini`, `brand:wikidata=Q26678;Q116232`.
/// Shared by `for_place` and `score_osm_candidate` -- same lookup, two
/// different tag representations (`Place.tags` and
/// `OsmCandidate::tags()`).
fn find_brand_wikidata<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> impl Iterator<Item = u64> + 'a {
    tags.into_iter()
        .find(|&(k, _)| k == "brand:wikidata")
        .into_iter()
        .flat_map(|(_, v)| parse_wikidata_ids(v))
}

/// The [normalize_name]d parts of the first `brand` tag in `tags`,
/// which may list several `;`-separated brands.
fn find_brand_names<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)>,
) -> impl Iterator<Item = String> + 'a {
    tags.into_iter()
        .find(|&(k, _)| k == "brand")
        .into_iter()
        .flat_map(|(_, v)| split_values(v).map(normalize_name))
}

/// The [normalize_name]d brands of an OSM feature: those in its
/// `brand` tag, or if it has none, those in its `operator` tag. Many
/// OSM fuel stations name their brand only as `operator`, such as
/// `operator=Tamoil`.
fn find_osm_brand_names<'a>(
    tags: impl Iterator<Item = (&'a str, &'a str)> + Clone,
) -> impl Iterator<Item = String> + 'a {
    let key = if tags.clone().any(|(k, _)| k == "brand") {
        "brand"
    } else {
        "operator"
    };
    tags.into_iter()
        .find(move |&(k, _)| k == key)
        .into_iter()
        .flat_map(|(_, v)| split_values(v).map(normalize_name))
}

/// The Wikidata IDs that an ATP feature's brand goes by, for
/// [BrandDomains]: those in its `brand:wikidata` tag, or if it has none,
/// those in `operator:wikidata`, or else in `network:wikidata`, which
/// is how [crate::matchers::brand_domains::brand_id] reads them too.
fn find_domain_brand_ids<'a>(tags: impl Iterator<Item = (&'a str, &'a str)> + Clone) -> Vec<u64> {
    for key in ["brand:wikidata", "operator:wikidata", "network:wikidata"] {
        if let Some((_, v)) = tags.clone().find(|(k, _)| *k == key) {
            return parse_wikidata_ids(v).collect();
        }
    }
    Vec::new()
}

/// The evidence factor for an OSM feature without `brand:wikidata`
/// whose website is on a domain of the ATP feature's brand, see
/// [BrandDomains]. The same as [BRAND_NAME_FACTOR]: both say which
/// brand a feature is, not which branch.
const BRAND_DOMAIN_FACTOR: f64 = 0.96;

/// The evidence factor for an equal `brand` name, when the two sides
/// do not both have `brand:wikidata`. Below [CONTACT_FACTOR], because
/// a brand name says less than a shared phone number.
const BRAND_NAME_FACTOR: f64 = 0.96;

/// The evidence factor for an OSM feature whose `brand:wikidata` is a
/// former brand of the ATP feature's in its country, see
/// [former_brands]. The same as [BRAND_NAME_FACTOR]: it says which
/// brand a feature is, not which branch, and the old brand's stations
/// may also have closed rather than changed brand.
const FORMER_BRAND_FACTOR: f64 = 0.96;

/// The evidence factor for an OSM feature without a brand of its own
/// that has our street address, a compatible category and a fitting
/// name, see [PoiMatcher::address_evidence]. Below the other kinds of
/// evidence: an address is shared by every business in a building.
const ADDRESS_FACTOR: f64 = 0.9;

/// Address evidence counts only up to this distance.
const ADDRESS_MAX_METERS: f64 = 150.0;

/// Address evidence for an OSM shop without a name counts only up to
/// this distance, because in a mall many shops share one address.
const ADDRESS_UNNAMED_SHOP_MAX_METERS: f64 = 30.0;

/// Words that say nothing about which business a name is, for
/// [PoiMatcher::name_fits]. Lowercase and without accents, as
/// [street::words] gives them.
const GENERIC_NAME_WORDS: &[&str] = &[
    "and",
    "apotheke",
    "auto",
    "automobile",
    "automobiles",
    "banco",
    "bank",
    "banque",
    "cafe",
    "center",
    "centre",
    "coffee",
    "company",
    "corp",
    "das",
    "del",
    "der",
    "des",
    "die",
    "farmacia",
    "fitness",
    "garage",
    "gmbh",
    "group",
    "hotel",
    "inc",
    "international",
    "kids",
    "las",
    "les",
    "llc",
    "los",
    "ltd",
    "ltda",
    "mall",
    "market",
    "markt",
    "motors",
    "pharmacie",
    "pharmacy",
    "pizza",
    "plaza",
    "posto",
    "restaurant",
    "salon",
    "sarl",
    "service",
    "services",
    "shop",
    "shopping",
    "station",
    "store",
    "studio",
    "the",
    "und",
];

/// AllThePlaces features of these kinds get a [PoiMatcher].
const POI_MASK: MatchMask = MatchMask(
    MatchMask::SHOP.0
        | MatchMask::RESTAURANT.0
        | MatchMask::LODGING.0
        | MatchMask::SCHOOL.0
        | MatchMask::FUEL.0,
);

/// Beyond this distance, a contact or brand name match needs the OSM
/// street address to appear in the ATP address, see
/// [address::osm_address_in_atp_text].
/// On the planet, only a third of the far contact matches through a
/// category pair were right without this check.
const CONTACT_ADDRESS_CHECK_BEYOND_METERS: f64 = 150.0;

/// How close two AllThePlaces features of one spider must be to share
/// an OSM feature, see [Matcher::colocated_radius]. Departments of one
/// store, such as a car dealer's sales and service, usually come at the
/// same point; different branches of a chain are rarely this close.
const COLOCATED_METERS: f64 = 25.0;

/// Matches points of interest, such as shops, restaurants, hotels and
/// schools, by evidence:
///
/// - The same official ID, such as a French school's UAI: at least
///   [official_id::SAME_ID_MIN_SCORE], by the distance to the OSM
///   shape; see [official_id::same_id_score].
/// - Both sides have an official ID of the same kind, but share none:
///   no match, whatever else agrees. A collège and its SEGPA section
///   can share an address, a name and a phone number.
/// - Equal `brand:wikidata`: the distance score.
/// - Both sides have `brand:wikidata`, but share no ID: no match,
///   whatever else agrees, unless the OSM brand is a former brand of
///   ours in this country, such as Idemitsu for apollostation in
///   Japan ([former_brands]). Then [FORMER_BRAND_FACTOR] times
///   [category_factor] times the distance score.
/// - Otherwise, the first of four kinds of evidence that agrees, times
///   [category_factor] times the distance score:
///   - a shared descriptive phone number, email address or website, with
///     [CONTACT_FACTOR];
///   - an equal brand name, with [BRAND_NAME_FACTOR]. Many OSM
///     features name their brand but lack `brand:wikidata`. On the
///     OSM side, see [find_osm_brand_names]; on ours, the names of
///     our brand, operator and network (see [brand_names]), compared
///     by [brand_names::name_key], so without regard to case, accents,
///     spacing and punctuation;
///   - an OSM feature without a brand of its own (no `brand` or
///     `brand:wikidata`) whose `name` begins with one of those names,
///     such as `ローソン能美寺井店` for Lawson or `Rubis Majaoni` for
///     Rubis, with [BRAND_NAME_FACTOR]; see
///     [brand_names::starts_with_brand];
///   - an OSM feature without `brand:wikidata` whose website is on a
///     domain of the ATP feature's brand, with [BRAND_DOMAIN_FACTOR];
///     see [BrandDomains] and [find_domain_brand_ids];
///   - an OSM feature without a brand of its own at the ATP feature's
///     street address, with a name that fits, with [ADDRESS_FACTOR];
///     see [PoiMatcher::address_evidence]. Only within
///     [ADDRESS_MAX_METERS], or [ADDRESS_UNNAMED_SHOP_MAX_METERS] for
///     a shop without a name, as in a mall many shops share an address.
///
///   Beyond [CONTACT_ADDRESS_CHECK_BEYOND_METERS], the street address
///   must agree too.
///
/// Matches are one to one within a spider ([Matcher::is_exclusive]),
/// so that two branches of a chain don't both take the same OSM shop
/// when OSM lacks one of them. Features within [COLOCATED_METERS] of
/// each other may share one ([Matcher::colocated_radius]), and so may
/// features with a shared contact or official ID ([Matcher::may_share]).
///
/// Contacts that differ never lower a score: about one in six
/// right `brand:wikidata` matches has different numbers on the two
/// sides.
pub struct PoiMatcher<'a> {
    center: Point,
    /// Usually a single ID; see [find_brand_wikidata].
    brand_wikidata: Vec<u64>,
    /// The brands that ours replaced in the ATP feature's country,
    /// see [former_brands]; usually empty.
    former_brand_wikidata: Vec<u64>,
    /// Usually a single name; see [find_brand_names].
    brand_names: Vec<String>,
    /// The [brand_names::name_key]s of our brand, operator and
    /// network: from our own tags, and from the name-suggestion-index
    /// by Wikidata ID, for our position and category.
    name_keys: Vec<String>,
    contacts: AtpContacts,
    /// See [find_domain_brand_ids], but only the brands that have a
    /// domain in `brand_domains`.
    domain_brand_ids: Vec<u64>,
    brand_domains: &'a BrandDomains,
    /// Main category as `key=value`, for contact and name matches.
    category: Option<String>,
    /// The ATP feature's `addr:*` tags, for far contact and name
    /// matches and for [Self::address_evidence].
    address: Vec<(String, String)>,
    /// The ATP feature's `name` and `brand`, for [Self::name_fits].
    name: Option<String>,
    brand: Option<String>,
    /// See [official_id::atp_ids].
    official_ids: Vec<(&'static str, String)>,
}

impl<'a> PoiMatcher<'a> {
    // TODO(#953): We should also look at names, by computing the token sort
    // ratio, but we should do this in a way that works for CJK. So we need
    // a decent segmenter that works for CJK languages. Maybe Lindera?
    pub fn for_place(
        place: &Place,
        nondescript: &Nondescript,
        brand_domains: &'a BrandDomains,
        nsi: &NsiNames,
    ) -> Option<PoiMatcher<'a>> {
        if !place.mask.intersects(&POI_MASK) {
            return None;
        }
        let tags = || place.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let brand_wikidata: Vec<u64> = find_brand_wikidata(tags()).collect();
        let brand_names: Vec<String> = find_brand_names(tags()).collect();
        let ll = LatLng::from(CellID(place.s2_cell_id));
        let position = Some((ll.lng.deg(), ll.lat.deg()));
        let former_brand_wikidata = if former_brands::has_former_brands(&brand_wikidata) {
            let country = countries::country(ll.lng.deg(), ll.lat.deg());
            former_brands::former_brands(&brand_wikidata, country)
        } else {
            Vec::new()
        };
        let contacts = AtpContacts::from_tags(tags(), position, nondescript);
        let mut domain_brand_ids = if brand_domains.is_empty() {
            Vec::new()
        } else {
            find_domain_brand_ids(tags())
        };
        domain_brand_ids.retain(|id| brand_domains.has_brand(*id));
        let address: Vec<(String, String)> = tags()
            .filter(|(k, _)| k.starts_with("addr:"))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        // Only an address with a house number can say which place
        // this is. A playground with just a street name can't.
        let has_street_address = address.iter().any(|(k, v)| {
            matches!(
                k.as_str(),
                "addr:street" | "addr:street_address" | "addr:full"
            ) && (v.chars().any(|c| c.is_ascii_digit())
                || address.iter().any(|(k, _)| k == "addr:housenumber"))
        });
        let official_ids = official_id::atp_ids(tags());
        let category = main_category(tags());
        let mut name_keys = brand_names::atp_name_keys(tags());
        nsi.add_names(
            &brand_names::wikidata_ids(tags()),
            category.as_deref(),
            ll.lng.deg(),
            ll.lat.deg(),
            &mut name_keys,
        );
        name_keys.sort();
        name_keys.dedup();
        if brand_wikidata.is_empty()
            && official_ids.is_empty()
            && name_keys.is_empty()
            && contacts.is_empty()
            && domain_brand_ids.is_empty()
            && !has_street_address
        {
            return None;
        }
        let find = |key: &str| tags().find(|(k, _)| *k == key).map(|(_, v)| v.to_string());
        Some(PoiMatcher {
            center: Cell::from(CellID(place.s2_cell_id)).center(),
            brand_wikidata,
            former_brand_wikidata,
            brand_names,
            name_keys,
            contacts,
            domain_brand_ids,
            brand_domains,
            category,
            address,
            name: find("name"),
            brand: find("brand"),
            official_ids,
        })
    }

    /// The evidence factor for `candidate`, before distance, and the
    /// rule that gave it, for [Matcher::rule]: 1.0 for equal
    /// `brand:wikidata`, below 1.0 for a former brand, contact, brand
    /// name, brand domain or address match, 0.0 for no match.
    fn factor_and_rule(&self, candidate: &OsmCandidate) -> (f64, &'static str) {
        match official_id::compare(&self.official_ids, candidate.tags()) {
            IdEvidence::Same => return (1.0, "official_id"),
            IdEvidence::Different => return (0.0, "official_id"),
            IdEvidence::Unknown => {}
        }
        let mut osm_brand = find_brand_wikidata(candidate.tags()).peekable();
        let osm_has_brand = osm_brand.peek().is_some();
        let osm_category = || main_category(candidate.tags());
        if !self.brand_wikidata.is_empty() && osm_has_brand {
            let osm_brand: Vec<u64> = osm_brand.collect();
            if osm_brand.iter().any(|id| self.brand_wikidata.contains(id)) {
                return (1.0, "brand_wikidata");
            }
            if osm_brand
                .iter()
                .any(|id| self.former_brand_wikidata.contains(id))
            {
                let category = category_factor(self.category.as_deref(), osm_category().as_deref());
                return (FORMER_BRAND_FACTOR * category, "former_brand");
            }
            return (0.0, "brand_wikidata");
        }
        let contact = !self.contacts.is_empty() && self.contacts.agrees(candidate.tags());
        let (evidence, rule) = if contact {
            (CONTACT_FACTOR, "contact")
        } else if self.brand_name_agrees(candidate) {
            (BRAND_NAME_FACTOR, "brand_name")
        } else if !osm_has_brand && self.brand_in_name(candidate) {
            (BRAND_NAME_FACTOR, "brand_in_name")
        } else if self.brand_domain_agrees(candidate, osm_has_brand) {
            (BRAND_DOMAIN_FACTOR, "brand_domain")
        } else if self.address_evidence(candidate, osm_has_brand) {
            (ADDRESS_FACTOR, "address")
        } else {
            return (0.0, "none");
        };
        (
            evidence * category_factor(self.category.as_deref(), osm_category().as_deref()),
            rule,
        )
    }

    /// Every rule of [Self::factor_and_rule] whose evidence agrees for
    /// `candidate`, whichever of them decided: the same checks, but
    /// without stopping at the first that fires. Like
    /// [Self::factor_and_rule], this leaves out the distance limits
    /// that [Matcher::score_osm_candidate] puts on `address` and on
    /// far matches.
    fn agreeing_rules(&self, candidate: &OsmCandidate) -> Rules {
        let mut rules = Rules::default();
        if official_id::compare(&self.official_ids, candidate.tags()) == IdEvidence::Same {
            rules.insert("official_id");
        }
        let osm_brand: Vec<u64> = find_brand_wikidata(candidate.tags()).collect();
        let osm_has_brand = !osm_brand.is_empty();
        if osm_brand.iter().any(|id| self.brand_wikidata.contains(id)) {
            rules.insert("brand_wikidata");
        }
        if !self.brand_wikidata.is_empty()
            && osm_brand
                .iter()
                .any(|id| self.former_brand_wikidata.contains(id))
        {
            rules.insert("former_brand");
        }
        if !self.contacts.is_empty() && self.contacts.agrees(candidate.tags()) {
            rules.insert("contact");
        }
        if self.brand_name_agrees(candidate) {
            rules.insert("brand_name");
        }
        if !osm_has_brand && self.brand_in_name(candidate) {
            rules.insert("brand_in_name");
        }
        if self.brand_domain_agrees(candidate, osm_has_brand) {
            rules.insert("brand_domain");
        }
        if self.address_evidence(candidate, osm_has_brand) {
            rules.insert("address");
        }
        rules
    }

    /// Whether one of the OSM brand names ([find_osm_brand_names]) is
    /// one of [Self::name_keys].
    fn brand_name_agrees(&self, candidate: &OsmCandidate) -> bool {
        !self.name_keys.is_empty()
            && find_osm_brand_names(candidate.tags())
                .any(|n| self.name_keys.contains(&brand_names::name_key(&n)))
    }

    /// Whether `candidate` has no `brand` tag, and a `name` that
    /// begins with one of [Self::name_keys]. The caller checks that it
    /// has no `brand:wikidata`.
    fn brand_in_name(&self, candidate: &OsmCandidate) -> bool {
        if self.name_keys.is_empty() || candidate.tags().any(|(k, _)| k == "brand") {
            return false;
        }
        let Some(name) = candidate.tags().find(|(k, _)| *k == "name").map(|(_, v)| v) else {
            return false;
        };
        let words: Vec<String> = street::words(name).collect();
        self.name_keys
            .iter()
            .any(|key| brand_names::starts_with_brand(&words, key))
    }

    /// Whether `candidate` is of our brand by [find_domain_brand_ids],
    /// such as an OSM post office with the same `operator:wikidata` as
    /// ours, or has no `brand:wikidata` of its own but a website on a
    /// domain of our brand.
    fn brand_domain_agrees(&self, candidate: &OsmCandidate, osm_has_brand: bool) -> bool {
        if self.domain_brand_ids.is_empty() {
            return false;
        }
        let ours = |id: &u64| self.domain_brand_ids.contains(id);
        find_domain_brand_ids(candidate.tags()).iter().any(ours)
            || (!osm_has_brand && self.brand_domains.brands(candidate.tags()).iter().any(ours))
    }

    /// Whether `candidate` has our street address and a name that fits
    /// ours ([Self::name_fits]), and no brand of its own: no
    /// `brand:wikidata`, `brand` or `operator`. A brand of its own that
    /// differs from ours means another business at the same address.
    /// The category and distance are checked by the caller.
    fn address_evidence(&self, candidate: &OsmCandidate, osm_has_brand: bool) -> bool {
        !osm_has_brand
            && find_osm_brand_names(candidate.tags()).next().is_none()
            && self.name_fits(candidate)
            && self.address_agrees(candidate)
    }

    /// Whether the OSM name, if there is one, can be the name of our
    /// feature. Words of either address, such as the city in `Dunkin'
    /// Berlin`, don't count. When our name is our brand, the OSM name
    /// must share a word with it (`Domino's Pizza` for `Domino's`).
    /// Otherwise it must share a word of our name that isn't part of
    /// our brand (`Ira Ford` for `Ira Ford Auburn`, but not `DCH Toyota`
    /// for `Imperial Toyota`), or be our brand alone (`Rossmann` for
    /// `Rossmann Drogerie-Markt Klosterstr. 34`). Words shorter than
    /// four letters and [GENERIC_NAME_WORDS] count only as part of a
    /// name that is the same as ours as a whole.
    fn name_fits(&self, candidate: &OsmCandidate) -> bool {
        let Some(osm_name) = candidate.tags().find(|(k, _)| *k == "name").map(|(_, v)| v) else {
            return true;
        };
        let place_words: Vec<String> = self
            .address
            .iter()
            .map(|(_, v)| v.as_str())
            .chain(
                candidate
                    .tags()
                    .filter(|(k, _)| k.starts_with("addr:"))
                    .map(|(_, v)| v),
            )
            .flat_map(street::words)
            .collect();
        let all_words = |s: &str| -> Vec<String> {
            street::words(s)
                .filter(|w| !place_words.contains(w))
                .collect()
        };
        let telling = |words: Vec<String>| -> Vec<String> {
            words
                .into_iter()
                .filter(|w| w.chars().count() >= 4 && !GENERIC_NAME_WORDS.contains(&w.as_str()))
                .collect()
        };
        let osm_all = all_words(osm_name);
        let osm = telling(osm_all.clone());
        if osm.is_empty() {
            // Compare all words, without accents or punctuation, so
            // that `École primaire publique Eugène Pottier` on Boulevard
            // Eugène Pottier fits `Ecole primaire publique Eugène Pottier`.
            let osm_name: Vec<String> = street::words(osm_name).collect();
            return [&self.name, &self.brand]
                .into_iter()
                .flatten()
                .any(|n| street::words(n).eq(osm_name.iter().cloned()));
        }
        let brand_all: Vec<String> = self.brand.as_deref().map(all_words).unwrap_or_default();
        let brand = telling(brand_all.clone());
        let name = telling(self.name.as_deref().map(all_words).unwrap_or_default());
        let distinct: Vec<&String> = name.iter().filter(|w| !brand.contains(w)).collect();
        if distinct.is_empty() {
            return osm.iter().any(|w| brand.contains(w));
        }
        osm.iter().any(|w| distinct.contains(&w))
            || (!brand_all.is_empty() && osm_all.iter().all(|w| brand_all.contains(w)))
    }

    fn address_agrees(&self, candidate: &OsmCandidate) -> bool {
        let atp: Vec<(&str, &str)> = self
            .address
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let osm: Vec<(&str, &str)> = candidate.tags().collect();
        address::osm_address_in_atp_text(&atp, &osm)
    }
}

impl Matcher for PoiMatcher<'_> {
    /// `official_id`, `brand_wikidata`, `former_brand`, `contact`
    /// (phone or email), `brand_name`, `brand_in_name`, `brand_domain`
    /// or `address`, in
    /// the order [PoiMatcher::factor_and_rule] tries them.
    fn rule(&self, candidate: &OsmCandidate) -> &'static str {
        self.factor_and_rule(candidate).1
    }

    fn rules(&self, candidate: &OsmCandidate) -> Rules {
        self.agreeing_rules(candidate)
    }

    /// False if the ATP feature has nothing but an `operator:wikidata`
    /// or `network:wikidata` with [BrandDomains]. Such features had no
    /// matcher before brand domains existed, and many brands of this
    /// kind are only partly in ATP: the Swiss Post spider lists post
    /// offices, but not the post agencies in village shops that OSM
    /// tags with the same `operator:wikidata`.
    ///
    /// A website alone doesn't count either, for the same reason: the
    /// Swiss Post spider gives each post office a page of its own.
    /// Only the street address can't say that a place is missing,
    /// and neither can an official ID: OSM often has a school without
    /// its UAI.
    fn reports_missing(&self) -> bool {
        !self.brand_wikidata.is_empty()
            || !self.brand_names.is_empty()
            || !self.contacts.is_empty()
            || !self.domain_brand_ids.is_empty()
    }

    fn counts_for_coverage(&self) -> bool {
        !self.brand_wikidata.is_empty()
            || !self.brand_names.is_empty()
            || self.contacts.has_phone_or_email()
    }

    /// On the 2026-11-02 planet run, 36k ATP features took an OSM shop
    /// that a closer branch of the same spider also took, a median
    /// 246 m away; their phones agreed with the OSM shop's in 16% of
    /// cases, against 83-87% for other matches beyond 150 m.
    fn is_exclusive(&self) -> bool {
        true
    }

    /// A shared descriptive phone number, email address or website
    /// says that `candidate` is our place, even if a closer feature of
    /// our spider took it. Spiders sometimes list one store several
    /// times, such as Staples with a feature for each of its print
    /// services, a few dozen meters from the store.
    ///
    /// The same holds for an official ID: OSM sometimes maps a lycée
    /// polyvalent and its lycée professionnel as one school, tagged
    /// with the IDs of both.
    fn may_share(&self, candidate: &OsmCandidate) -> bool {
        (!self.contacts.is_empty() && self.contacts.agrees(candidate.tags()))
            || official_id::compare(&self.official_ids, candidate.tags()) == IdEvidence::Same
    }

    fn colocated_radius(&self) -> Option<ChordAngle> {
        Some(meters_to_chord_angle(COLOCATED_METERS))
    }

    fn score_osm_candidate(&self, candidate: &OsmCandidate) -> f64 {
        // Cheap check first: only decode the (potentially large) geometry
        // for candidates that have some evidence.
        let (factor, rule) = self.factor_and_rule(candidate);
        if factor == 0.0 {
            return 0.0;
        }
        let Result::Ok(geometry) = candidate.geometry() else {
            return 0.0;
        };
        if rule == "official_id" {
            return official_id::same_id_score(&self.center, &geometry);
        }
        let Some(centroid) = geometry.centroid() else {
            return 0.0;
        };
        let distance = geo_point_distance(&self.center, &centroid);
        if rule == "address" {
            let unnamed_shop = candidate.tags().any(|(k, _)| k == "shop")
                && !candidate.tags().any(|(k, _)| k == "name");
            let max = if unnamed_shop {
                ADDRESS_UNNAMED_SHOP_MAX_METERS
            } else {
                ADDRESS_MAX_METERS
            };
            // Measure to the nearest point of the shape, as a car
            // dealer's lot can be large but has one address.
            let ll = LatLng::from(self.center);
            let atp = geo::Point::new(ll.lng.deg(), ll.lat.deg());
            let nearest = match geometry.closest_point(&atp) {
                Closest::Intersection(_) => 0.0,
                Closest::SinglePoint(p) => geo_point_distance(&self.center, &p),
                Closest::Indeterminate => distance,
            };
            if nearest > max {
                return 0.0;
            }
        }
        if factor < 1.0
            && distance > CONTACT_ADDRESS_CHECK_BEYOND_METERS
            && !self.address_agrees(candidate)
        {
            return 0.0;
        }
        factor * geo_point_distance_score(&self.center, &centroid, 400.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::UtcTimestamp;
    use std::sync::LazyLock;

    static NO_DOMAINS: LazyLock<BrandDomains> = LazyLock::new(BrandDomains::none);
    static NO_NSI: LazyLock<NsiNames> = LazyLock::new(NsiNames::default);

    fn test_timestamp() -> UtcTimestamp {
        UtcTimestamp(time::UtcDateTime::from_unix_timestamp(1_770_000_000).unwrap())
    }

    /// A placeholder point shape for fixtures below that only care about
    /// `s2_cell_id`/`mask`/`tags`, not the real geometry -- reconstructs
    /// a point at the cell's own center, so it's at least consistent
    /// with `s2_cell_id` rather than an arbitrary unrelated coordinate.
    fn test_shape(s2_cell_id: u64) -> Vec<u8> {
        let cell = s2::cellid::CellID(s2_cell_id);
        let lat_lng = s2::latlng::LatLng::from(cell);
        crate::geometry::encode_wkb(&geo::Geometry::from(geo::Point::new(
            lat_lng.lng.deg(),
            lat_lng.lat.deg(),
        )))
    }

    static CH_CLOTHES_ATP: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637664633565895,
        spider: String::from("newyorker"),
        mask: MatchMask::SHOP,
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637664633565895),
        tags: tags(&[
            ("addr:city", "Rapperswil"),
            ("addr:country", "CH"),
            ("addr:full", "Zürcherstrasse 4, 8640 Rapperswil"),
            ("addr:postcode", "8640"),
            ("brand", "New Yorker"),
            ("brand:wikidata", "Q706421"),
            ("name", "NEW YORKER | EKZ Sonnenhof"),
            ("opening_hours", "Mo-Fr 09:00-20:00; Sa 08:00-18:00"),
            ("phone", "+41 55 210 63 10"),
            ("ref", "5b3cb3fac00458481b4375e1"),
            ("shop", "clothes"),
        ]),
    });

    static CH_CLOTHES_OSM: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637664662121729,
        spider: String::from("osm"),
        mask: MatchMask(1),
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637664662121729),
        tags: tags(&[
            ("branch", "Rapperswil Sonnenhof"),
            ("brand", "New Yorker"),
            ("brand:wikidata", "Q706421"),
            ("level", "0"),
            ("name", "New Yorker"),
            ("opening_hours", "Mo-Fr 09:00-20:00; Sa 08:00-18:00"),
            ("shop", "clothes"),
            ("website", "https://www.newyorker.de/ch/"),
        ]),
    });

    static CH_KIOSK_ATP: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637400739491865,
        spider: String::from("valora"),
        mask: MatchMask::SHOP,
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637400739491865),
        tags: tags(&[
            ("addr:city", "Rapperswil"),
            ("addr:country", "CH"),
            ("addr:postcode", "8640"),
            ("addr:street_address", "Unterführung Bahnhof 1"),
            ("brand", "k kiosk"),
            ("brand:wikidata", "Q60381703"),
            ("name", "kkiosk Rapperswil BHF"),
            ("opening_hours", "Mo-Fr 05:30-20:00; Sa-Su 07:00-20:00"),
            ("ref", "730"),
            ("shop", "newsagent"),
        ]),
    });

    static CH_KIOSK_OSM: LazyLock<Place> = LazyLock::new(|| Place {
        s2_cell_id: 5159637400743919515,
        spider: String::from("osm"),
        mask: MatchMask::SHOP,
        fetched: test_timestamp(),
        shape_wkb: test_shape(5159637400743919515),
        tags: tags(&[
            ("brand", "k kiosk"),
            ("brand:wikidata", "Q60381703"),
            ("brand:wikipedia", "it:K Kiosk"),
            ("cash_withdrawal", "yes"),
            ("cash_withdrawal:fee", "no"),
            ("cash_withdrawal:operator", "sonect"),
            ("cash_withdrawal:purchase_required", "no"),
            ("cash_withdrawal:type", "checkout"),
            ("level", "-1"),
            ("name", "k kiosk"),
            (
                "opening_hours",
                "Mo-Fr 05:45-19:00; Sa 08:00-18:00; Su 09:00-18:00",
            ),
            ("shop", "kiosk"),
            ("wheelchair", "yes"),
        ]),
    });

    fn tags(t: &[(&str, &str)]) -> Vec<(String, String)> {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Exercises `score_osm_candidate` for two ATP shops (New Yorker,
    /// k kiosk) against real-world OSM positions (reused from
    /// `CH_CLOTHES_OSM`/`CH_KIOSK_OSM`'s `s2_cell_id`s, so this exercises
    /// the WKB-decode-then-centroid distance computation against genuine
    /// coordinates, not synthetic ones), with the OSM side as a
    /// `Feature` + `StringPool` instead of a `Place`.
    #[test]
    fn test_poi_matcher_osm_candidate() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        const WKB_OPTS: WriteOptions = WriteOptions {
            endianness: wkb::Endianness::LittleEndian,
        };

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "brand:wikidata",
            "Q706421",
            "shop",
            "clothes",
            "Q60381703",
            "kiosk",
            "Q706421; Q60381703",
            "Q1;Q2",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");

        let make_feature = |tags: &[(&str, &str)], s2_cell_id: u64| -> Feature {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WKB_OPTS,
            )
            .expect("wkb encode");
            Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            }
        };

        let clothes_feature = make_feature(
            &[("brand:wikidata", "Q706421"), ("shop", "clothes")],
            CH_CLOTHES_OSM.s2_cell_id,
        );
        let kiosk_feature = make_feature(
            &[("brand:wikidata", "Q60381703"), ("shop", "kiosk")],
            CH_KIOSK_OSM.s2_cell_id,
        );
        let clothes_candidate = OsmCandidate {
            feature: &clothes_feature,
            strings: &pool,
        };
        let kiosk_candidate = OsmCandidate {
            feature: &kiosk_feature,
            strings: &pool,
        };

        let ch_clothes_matcher =
            PoiMatcher::for_place(&CH_CLOTHES_ATP, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
                .expect("should create matcher");
        let ch_kiosk_matcher =
            PoiMatcher::for_place(&CH_KIOSK_ATP, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
                .expect("should create matcher");
        assert!(ch_clothes_matcher.score_osm_candidate(&clothes_candidate) > 0.5);
        assert!(ch_clothes_matcher.score_osm_candidate(&kiosk_candidate) == 0.0);
        assert!(ch_kiosk_matcher.score_osm_candidate(&clothes_candidate) == 0.0);
        assert!(ch_kiosk_matcher.score_osm_candidate(&kiosk_candidate) > 0.5);

        // A multi-brand OSM feature matches each of its brands.
        for (matcher, s2_cell_id) in [
            (&ch_clothes_matcher, CH_CLOTHES_OSM.s2_cell_id),
            (&ch_kiosk_matcher, CH_KIOSK_OSM.s2_cell_id),
        ] {
            let multi_feature = make_feature(
                &[
                    ("brand:wikidata", "Q706421; Q60381703"),
                    ("shop", "clothes"),
                ],
                s2_cell_id,
            );
            let multi_candidate = OsmCandidate {
                feature: &multi_feature,
                strings: &pool,
            };
            assert!(matcher.score_osm_candidate(&multi_candidate) > 0.5);
        }
        let other_feature = make_feature(
            &[("brand:wikidata", "Q1;Q2"), ("shop", "clothes")],
            CH_KIOSK_OSM.s2_cell_id,
        );
        let other_candidate = OsmCandidate {
            feature: &other_feature,
            strings: &pool,
        };
        assert_eq!(ch_kiosk_matcher.score_osm_candidate(&other_candidate), 0.0);
    }

    #[test]
    fn test_poi_matcher_is_exclusive() {
        let m = PoiMatcher::for_place(&CH_CLOTHES_ATP, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        assert!(m.is_exclusive());
        let radius = m.colocated_radius().expect("colocated radius");
        assert!(radius > meters_to_chord_angle(COLOCATED_METERS - 1.0));
        assert!(radius < meters_to_chord_angle(COLOCATED_METERS + 1.0));
    }

    /// Phone evidence for an ATP feature without `brand:wikidata`: a
    /// shared number matches when the categories agree, never against a
    /// `brand:wikidata` conflict, and never with a different category.
    #[test]
    fn test_poi_matcher_contact() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "amenity",
            "brand:wikidata",
            "car",
            "car_repair",
            "contact:phone",
            "florist",
            "Q1",
            "Q2",
            "shop",
            "supermarket",
            "055 210 63 10",
            "+41 55 210 63 10",
            "+41 55 210 63 11",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let make_feature = |tags: &[(&str, &str)]| -> Feature {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            }
        };
        let score = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let feature = make_feature(tags);
            matcher.score_osm_candidate(&OsmCandidate {
                feature: &feature,
                strings: &pool,
            })
        };
        let atp = |tags: &[(&str, &str)]| Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::SHOP,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: self::tags(tags),
        };

        let dealer = atp(&[
            ("addr:country", "CH"),
            ("phone", "+41 55 210 63 10"),
            ("shop", "car"),
        ]);
        let m = PoiMatcher::for_place(&dealer, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let same = score(
            &m,
            &[("shop", "car"), ("contact:phone", "+41 55 210 63 10")],
        );
        assert!(same > 0.9 && same < 1.0, "{same}");
        let national = score(&m, &[("shop", "car"), ("contact:phone", "055 210 63 10")]);
        assert_eq!(national, same);
        let may_share = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let feature = make_feature(tags);
            matcher.may_share(&OsmCandidate {
                feature: &feature,
                strings: &pool,
            })
        };
        assert!(may_share(
            &m,
            &[("shop", "car"), ("contact:phone", "+41 55 210 63 10")]
        ));
        assert!(!may_share(
            &m,
            &[("shop", "car"), ("contact:phone", "+41 55 210 63 11")]
        ));
        let pair = score(
            &m,
            &[
                ("shop", "car_repair"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert!(pair > 0.9 && pair < same, "{pair}");
        let other_category = score(
            &m,
            &[
                ("shop", "supermarket"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert_eq!(other_category, 0.0);
        let other_phone = score(
            &m,
            &[("shop", "car"), ("contact:phone", "+41 55 210 63 11")],
        );
        assert_eq!(other_phone, 0.0);
        // OSM has brand:wikidata, ATP does not: no conflict.
        let osm_brand = score(
            &m,
            &[
                ("brand:wikidata", "Q1"),
                ("shop", "car"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert_eq!(osm_brand, same);

        // Both sides have brand:wikidata, and they differ: no match.
        let branded = atp(&[
            ("brand:wikidata", "Q2"),
            ("phone", "+41 55 210 63 10"),
            ("shop", "car"),
        ]);
        let m = PoiMatcher::for_place(&branded, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let vetoed = score(
            &m,
            &[
                ("brand:wikidata", "Q1"),
                ("shop", "car"),
                ("contact:phone", "+41 55 210 63 10"),
            ],
        );
        assert_eq!(vetoed, 0.0);
        let brand = score(&m, &[("brand:wikidata", "Q2"), ("shop", "florist")]);
        assert!(brand > same, "{brand}");

        // No brand:wikidata and no contacts: no matcher at all.
        let bare = atp(&[("shop", "florist")]);
        assert!(PoiMatcher::for_place(&bare, &Nondescript::none(), &NO_DOMAINS, &NO_NSI).is_none());

        // A street without a house number doesn't make a matcher.
        let street_only = atp(&[("leisure", "playground"), ("addr:street", "Bahnhofstrasse")]);
        assert!(
            PoiMatcher::for_place(&street_only, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
                .is_none()
        );
    }

    /// Brand name evidence: an equal `brand` name matches when the
    /// categories agree, without regard to case and spacing, and
    /// never against a `brand:wikidata` conflict.
    #[test]
    fn test_poi_matcher_brand_name() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "amenity",
            "brand",
            "brand:wikidata",
            "fuel",
            "shop",
            "supermarket",
            "Avia",
            "AVIA",
            "Avia;Shell",
            "Shell",
            "Q1",
            "Q2",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let score = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            let feature = Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            };
            matcher.score_osm_candidate(&OsmCandidate {
                feature: &feature,
                strings: &pool,
            })
        };
        let atp = |tags: &[(&str, &str)]| Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::FUEL,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: self::tags(tags),
        };

        // ATP without brand:wikidata still gets a matcher.
        let avia = atp(&[("amenity", "fuel"), ("brand", "Avia")]);
        let m = PoiMatcher::for_place(&avia, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let same = score(&m, &[("amenity", "fuel"), ("brand", "AVIA")]);
        assert!(same > 0.9 && same < BRAND_NAME_FACTOR + 1e-9, "{same}");
        assert_eq!(
            score(&m, &[("amenity", "fuel"), ("brand", "Avia;Shell")]),
            same
        );
        assert_eq!(score(&m, &[("amenity", "fuel"), ("brand", "Shell")]), 0.0);
        assert_eq!(
            score(&m, &[("shop", "supermarket"), ("brand", "Avia")]),
            0.0
        );

        // ATP has brand:wikidata, OSM only the brand name.
        let branded = atp(&[
            ("amenity", "fuel"),
            ("brand", "Avia"),
            ("brand:wikidata", "Q1"),
        ]);
        let m = PoiMatcher::for_place(&branded, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        assert_eq!(score(&m, &[("amenity", "fuel"), ("brand", "Avia")]), same);
        // Both sides have brand:wikidata, and they differ: no match.
        let vetoed = score(
            &m,
            &[
                ("amenity", "fuel"),
                ("brand", "Avia"),
                ("brand:wikidata", "Q2"),
            ],
        );
        assert_eq!(vetoed, 0.0);
    }

    /// Our brand at the start of an OSM name, from our own tags or from
    /// the name-suggestion-index, such as its `matchNames`.
    #[test]
    fn test_poi_matcher_rules() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "amenity",
            "brand",
            "brand:wikidata",
            "fuel",
            "name",
            "phone",
            "Avia",
            "Avia Tankstelle Baar",
            "+41 41 760 00 00",
            "+41 41 760 99 99",
            "Q1",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let rules = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            let feature = Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            };
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &pool,
            };
            assert!(matcher.score_osm_candidate(&candidate) > 0.0);
            (
                matcher.rule(&candidate),
                matcher.rules(&candidate).names().collect::<Vec<_>>(),
            )
        };
        let place = Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::FUEL,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: tags(&[
                ("amenity", "fuel"),
                ("brand", "Avia"),
                ("brand:wikidata", "Q1"),
                ("phone", "+41 41 760 00 00"),
            ]),
        };
        let m = PoiMatcher::for_place(&place, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let phone = ("phone", "+41 41 760 00 00");
        let other_phone = ("phone", "+41 41 760 99 99");

        // The contact decides, but the brand name agrees too.
        assert_eq!(
            rules(&m, &[("amenity", "fuel"), ("brand", "Avia"), phone]),
            ("contact", vec!["contact", "brand_name"])
        );
        assert_eq!(
            rules(&m, &[("amenity", "fuel"), ("brand", "Avia"), other_phone]),
            ("brand_name", vec!["brand_name"])
        );
        assert_eq!(
            rules(
                &m,
                &[("amenity", "fuel"), ("name", "Avia Tankstelle Baar"), phone]
            ),
            ("contact", vec!["contact", "brand_in_name"])
        );
        // Rules after `brand_wikidata` are checked too.
        assert_eq!(
            rules(
                &m,
                &[
                    ("amenity", "fuel"),
                    ("brand", "Avia"),
                    ("brand:wikidata", "Q1"),
                    phone
                ]
            ),
            (
                "brand_wikidata",
                vec!["brand_wikidata", "contact", "brand_name"]
            )
        );
    }

    #[test]
    fn test_poi_matcher_brand_in_name() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "amenity",
            "brand",
            "brand:wikidata",
            "fuel",
            "name",
            "operator",
            "shop",
            "supermarket",
            "Avia Tankstelle Baar",
            "Aviatik Garage",
            "Agip Zug",
            "AGIP",
            "Avia",
            "Shell",
            "Q2",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let score = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            let feature = Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            };
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &pool,
            };
            (
                matcher.score_osm_candidate(&candidate),
                matcher.rule(&candidate),
            )
        };
        let nsi = NsiNames::from_json(&serde_json::json!({"nsi": {
            "brands/amenity/fuel": {"items": [{
                "id": "avia",
                "locationSet": {"include": ["ch"]},
                "matchNames": ["agip"],
                "tags": {"brand": "Avia", "brand:wikidata": "Q1"}
            }]}
        }}));
        let place = Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::FUEL,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: tags(&[
                ("amenity", "fuel"),
                ("brand", "Avia"),
                ("brand:wikidata", "Q1"),
            ]),
        };
        let m = PoiMatcher::for_place(&place, &Nondescript::none(), &NO_DOMAINS, &nsi)
            .expect("matcher");
        let (same, rule) = score(&m, &[("amenity", "fuel"), ("name", "Avia Tankstelle Baar")]);
        assert!(same > 0.9 && same < BRAND_NAME_FACTOR + 1e-9, "{same}");
        assert_eq!(rule, "brand_in_name");
        // Only at a word boundary.
        assert_eq!(
            score(&m, &[("amenity", "fuel"), ("name", "Aviatik Garage")]).0,
            0.0
        );
        // A name from NSI's matchNames.
        assert_eq!(
            score(&m, &[("amenity", "fuel"), ("name", "Agip Zug")]),
            (same, "brand_in_name")
        );
        // ...also as OSM operator, which counts as a brand name.
        assert_eq!(
            score(&m, &[("amenity", "fuel"), ("operator", "AGIP")]),
            (same, "brand_name")
        );
        // A brand of its own: the name doesn't count.
        assert_eq!(
            score(
                &m,
                &[
                    ("amenity", "fuel"),
                    ("brand", "Shell"),
                    ("name", "Avia Tankstelle Baar")
                ]
            )
            .0,
            0.0
        );
        // Another brand:wikidata vetoes the match.
        assert_eq!(
            score(
                &m,
                &[
                    ("amenity", "fuel"),
                    ("brand:wikidata", "Q2"),
                    ("name", "Avia Tankstelle Baar")
                ]
            )
            .0,
            0.0
        );
        // Another category.
        assert_eq!(
            score(
                &m,
                &[("shop", "supermarket"), ("name", "Avia Tankstelle Baar")]
            )
            .0,
            0.0
        );

        // Without NSI, the matchName is unknown.
        let m = PoiMatcher::for_place(&place, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        assert_eq!(
            score(&m, &[("amenity", "fuel"), ("name", "Agip Zug")]).0,
            0.0
        );
    }

    /// Former brands: in Japan, an OSM station still tagged as
    /// Idemitsu or Showa Shell matches an apollostation in ATP, but a
    /// Shell station elsewhere doesn't, and neither does another brand.
    #[test]
    fn test_poi_matcher_former_brand() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "amenity",
            "brand:wikidata",
            "fuel",
            "shop",
            "supermarket",
            "Q114731101",
            "Q2216770",
            "Q110716465",
            "Q1",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let tokyo = CellID::from(LatLng::from_degrees(35.6812, 139.7671)).0;
        let berlin = CellID::from(LatLng::from_degrees(52.5200, 13.4050)).0;
        let score = |matcher: &PoiMatcher, s2_cell_id: u64, tags: &[(&str, &str)]| {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            let feature = Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            };
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &pool,
            };
            (
                matcher.score_osm_candidate(&candidate),
                matcher.rule(&candidate),
            )
        };
        let apollostation = |s2_cell_id: u64| Place {
            s2_cell_id,
            spider: String::from("apollostation_jp"),
            mask: MatchMask::FUEL,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: tags(&[("amenity", "fuel"), ("brand:wikidata", "Q114731101")]),
        };

        let jp = apollostation(tokyo);
        let m = PoiMatcher::for_place(&jp, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let (same, rule) = score(
            &m,
            tokyo,
            &[("amenity", "fuel"), ("brand:wikidata", "Q114731101")],
        );
        assert_eq!(rule, "brand_wikidata");
        for old in ["Q2216770", "Q110716465"] {
            let (former, rule) = score(&m, tokyo, &[("amenity", "fuel"), ("brand:wikidata", old)]);
            assert_eq!(rule, "former_brand");
            assert!(former > 0.9 && former < same, "{former}");
        }
        // Another brand still vetoes.
        let (other, rule) = score(&m, tokyo, &[("amenity", "fuel"), ("brand:wikidata", "Q1")]);
        assert_eq!((other, rule), (0.0, "brand_wikidata"));
        // So does another category.
        let (shop, _) = score(
            &m,
            tokyo,
            &[("shop", "supermarket"), ("brand:wikidata", "Q2216770")],
        );
        assert_eq!(shop, 0.0);

        // Outside Japan, Shell is still Shell.
        let de = apollostation(berlin);
        let m = PoiMatcher::for_place(&de, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let (shell, rule) = score(
            &m,
            berlin,
            &[("amenity", "fuel"), ("brand:wikidata", "Q110716465")],
        );
        assert_eq!((shell, rule), (0.0, "brand_wikidata"));
    }

    /// Address evidence: an OSM feature without a brand of its own, at
    /// our street address, with a compatible category and a name that
    /// fits ours.
    #[test]
    fn test_poi_matcher_address() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "addr:city",
            "addr:housenumber",
            "addr:street",
            "amenity",
            "brand",
            "car",
            "chemist",
            "fuel",
            "name",
            "shop",
            "Fortune Boulevard",
            "Fortune Blvd",
            "Milford",
            "300",
            "302",
            "Imperial Toyota",
            "DCH Toyota of Milford",
            "Honda",
            "Rossmann",
            "Rossmann Drogerie-Markt",
            "Toyota",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let score = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            let feature = Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            };
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &pool,
            };
            (
                matcher.score_osm_candidate(&candidate),
                matcher.rule(&candidate),
            )
        };
        let atp = |tags: &[(&str, &str)]| Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::SHOP,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: self::tags(tags),
        };
        let dealer = atp(&[
            ("shop", "car"),
            ("name", "Imperial Toyota"),
            ("brand", "Toyota"),
            ("addr:street", "Fortune Blvd"),
            ("addr:housenumber", "300"),
            ("addr:city", "Milford"),
        ]);
        let m = PoiMatcher::for_place(&dealer, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        let at = |extra: &[(&'static str, &'static str)]| {
            let mut tags = vec![
                ("addr:street", "Fortune Boulevard"),
                ("addr:housenumber", "300"),
                ("addr:city", "Milford"),
            ];
            tags.extend_from_slice(extra);
            tags
        };
        let (same, rule) = score(&m, &at(&[("shop", "car"), ("name", "Imperial Toyota")]));
        assert_eq!(rule, "address");
        assert!(
            same > ADDRESS_FACTOR - 0.01 && same <= ADDRESS_FACTOR,
            "{same}"
        );
        // No name: fine, this close.
        assert_eq!(score(&m, &at(&[("shop", "car")])).0, same);
        // Another dealer of the same brand at the same address.
        assert_eq!(
            score(
                &m,
                &at(&[("shop", "car"), ("name", "DCH Toyota of Milford")])
            )
            .0,
            0.0
        );
        // A brand of its own.
        assert_eq!(
            score(&m, &at(&[("shop", "car"), ("brand", "Honda")])).0,
            0.0
        );
        // Another category.
        assert_eq!(score(&m, &at(&[("amenity", "fuel")])).0, 0.0);
        // Another house number.
        let other = [
            ("addr:street", "Fortune Boulevard"),
            ("addr:housenumber", "302"),
            ("shop", "car"),
        ];
        assert_eq!(score(&m, &other).0, 0.0);

        // The OSM name is just our brand.
        let chemist = atp(&[
            ("shop", "chemist"),
            ("name", "Rossmann Drogerie-Markt"),
            ("brand", "Rossmann"),
            ("addr:street", "Fortune Blvd"),
            ("addr:housenumber", "300"),
        ]);
        let m = PoiMatcher::for_place(&chemist, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        assert!(score(&m, &at(&[("shop", "chemist"), ("name", "Rossmann")])).0 > 0.0);
        assert!(m.reports_missing());

        // A matcher with only an address can't say a place is missing.
        let unbranded = atp(&[
            ("shop", "car"),
            ("name", "Imperial Motors"),
            ("addr:street", "Fortune Blvd"),
            ("addr:housenumber", "300"),
        ]);
        let m = PoiMatcher::for_place(&unbranded, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        assert!(!m.reports_missing());
        assert!(!m.counts_for_coverage());
    }

    /// Brand domain evidence: an OSM feature without `brand:wikidata`
    /// whose website is on a domain of the ATP feature's brand.
    #[test]
    fn test_poi_matcher_brand_domain() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;
        use wkb::writer::{WriteOptions, write_point};

        let dir = TempDir::new().expect("tempdir");
        let strings = [
            "brand:wikidata",
            "operator:wikidata",
            "shop",
            "supermarket",
            "florist",
            "website",
            "url",
            "https://www.denner.ch/de/filialen/zug",
            "https://www.denner.ch/",
            "http://filialen.denner.ch",
            "https://www.coop.ch/",
            "Q1",
            "Q2",
        ];
        let pool = StringPool::create(
            strings.iter().map(|s| s.to_string()),
            dir.path(),
            &dir.path().join("strings"),
        )
        .expect("StringPool::create");
        let s2_cell_id = CH_CLOTHES_ATP.s2_cell_id;
        let score = |matcher: &PoiMatcher, tags: &[(&str, &str)]| {
            let mut flat_tags = Vec::with_capacity(tags.len() * 2);
            for (k, v) in tags {
                flat_tags.push(pool.lookup(k).expect("key in pool") as u32);
                flat_tags.push(pool.lookup(v).expect("value in pool") as u32);
            }
            let ll = LatLng::from(CellID(s2_cell_id));
            let mut geometry_wkb = Vec::new();
            write_point(
                &mut geometry_wkb,
                &geo::Point::new(ll.lng.deg(), ll.lat.deg()),
                &WriteOptions {
                    endianness: wkb::Endianness::LittleEndian,
                },
            )
            .expect("wkb encode");
            let feature = Feature {
                id: 1,
                tags: flat_tags,
                geometry_wkb,
                ..Default::default()
            };
            matcher.score_osm_candidate(&OsmCandidate {
                feature: &feature,
                strings: &pool,
            })
        };
        let atp = |tags: &[(&str, &str)]| Place {
            s2_cell_id,
            spider: String::from("test"),
            mask: MatchMask::SHOP,
            fetched: test_timestamp(),
            shape_wkb: test_shape(s2_cell_id),
            tags: self::tags(tags),
        };
        let domains = BrandDomains::from_pairs(&[("denner.ch", 1), ("coop.ch", 2)]);
        let denner = atp(&[
            ("brand:wikidata", "Q1"),
            ("shop", "supermarket"),
            ("website", "https://www.denner.ch/de/filialen/zug"),
        ]);
        let m = PoiMatcher::for_place(&denner, &Nondescript::none(), &domains, &NO_NSI)
            .expect("matcher");
        let same = score(
            &m,
            &[
                ("shop", "supermarket"),
                ("website", "https://www.denner.ch/"),
            ],
        );
        assert!(same > 0.9 && same < BRAND_DOMAIN_FACTOR + 1e-9, "{same}");
        assert_eq!(
            score(
                &m,
                &[
                    ("shop", "supermarket"),
                    ("url", "http://filialen.denner.ch")
                ]
            ),
            same
        );
        // Another brand's domain, another category, or no website.
        assert_eq!(
            score(
                &m,
                &[("shop", "supermarket"), ("website", "https://www.coop.ch/")]
            ),
            0.0
        );
        assert_eq!(
            score(
                &m,
                &[("shop", "florist"), ("website", "https://www.denner.ch/")]
            ),
            0.0
        );
        assert_eq!(score(&m, &[("shop", "supermarket")]), 0.0);
        // OSM has its own brand:wikidata, which wins over the domain.
        assert_eq!(
            score(
                &m,
                &[
                    ("brand:wikidata", "Q2"),
                    ("shop", "supermarket"),
                    ("website", "https://www.denner.ch/"),
                ],
            ),
            0.0
        );

        // Without brand:wikidata, ATP's operator:wikidata counts.
        let operated = atp(&[("operator:wikidata", "Q1"), ("shop", "supermarket")]);
        let m = PoiMatcher::for_place(&operated, &Nondescript::none(), &domains, &NO_NSI)
            .expect("matcher");
        assert_eq!(
            score(
                &m,
                &[
                    ("shop", "supermarket"),
                    ("website", "https://www.denner.ch/")
                ]
            ),
            same
        );

        // Or the same operator:wikidata on OSM, website or not.
        assert_eq!(
            score(&m, &[("operator:wikidata", "Q1"), ("shop", "supermarket")]),
            same
        );
        assert_eq!(
            score(&m, &[("operator:wikidata", "Q2"), ("shop", "supermarket")]),
            0.0
        );

        // A brand without any domain gets no matcher of its own.
        let unknown = atp(&[("operator:wikidata", "Q3"), ("shop", "supermarket")]);
        assert!(PoiMatcher::for_place(&unknown, &Nondescript::none(), &domains, &NO_NSI).is_none());
        // Nor without domains at all.
        assert!(
            PoiMatcher::for_place(&operated, &Nondescript::none(), &NO_DOMAINS, &NO_NSI).is_none()
        );

        // Matchers from operator:wikidata and websites don't count for
        // the coverage of deletion candidates.
        assert!(!m.counts_for_coverage());
        let with_website = atp(&[
            ("operator:wikidata", "Q1"),
            ("shop", "supermarket"),
            ("website", "https://www.denner.ch/de/filialen/zug"),
        ]);
        let m = PoiMatcher::for_place(&with_website, &Nondescript::none(), &domains, &NO_NSI)
            .expect("matcher");
        assert!(!m.counts_for_coverage());
        let m = PoiMatcher::for_place(&denner, &Nondescript::none(), &domains, &NO_NSI)
            .expect("matcher");
        assert!(m.counts_for_coverage());
    }

    /// Official ID evidence: a French school's UAI (`ref:FR:UAI` in
    /// ATP, `ref:UAI` in OSM). The same ID matches whatever else
    /// differs; a different one vetoes any other evidence.
    #[test]
    fn test_poi_matcher_official_id() {
        use crate::tables::{Feature, StringPool};
        use s2::{cellid::CellID, latlng::LatLng};
        use tempfile::TempDir;

        let school = |t: &[(&str, &str)]| {
            let mut mask = MatchMask::default();
            for (k, v) in t {
                mask.add_tag(k, v);
            }
            Place {
                s2_cell_id: CH_CLOTHES_ATP.s2_cell_id,
                spider: String::from("enseignement_premier_second_degre_fr"),
                mask,
                fetched: test_timestamp(),
                shape_wkb: test_shape(CH_CLOTHES_ATP.s2_cell_id),
                tags: tags(t),
            }
        };
        let atp = school(&[
            ("amenity", "school"),
            ("ref:FR:UAI", "0351686T"),
            ("name", "Ecole primaire publique Françoise Dolto"),
            ("addr:street_address", "8 rue Frédéric Lanne"),
        ]);
        assert!(atp.mask.intersects(&MatchMask::OFFICIAL_ID));
        let m = PoiMatcher::for_place(&atp, &Nondescript::none(), &NO_DOMAINS, &NO_NSI)
            .expect("matcher");
        assert!(!m.reports_missing());
        assert!(!m.counts_for_coverage());

        // A school with nothing but its ID still gets a matcher.
        let id_only = school(&[("amenity", "school"), ("ref:FR:UAI", "0351686T")]);
        assert!(
            PoiMatcher::for_place(&id_only, &Nondescript::none(), &NO_DOMAINS, &NO_NSI).is_some()
        );

        let dir = TempDir::new().expect("tempdir");
        let mut strings: Vec<String> = [
            "addr:housenumber",
            "addr:street",
            "amenity",
            "building",
            "name",
            "ref:UAI",
            "school",
            "yes",
            "8",
            "Rue Frédéric Lanne",
            "0351686T",
            "0350001A",
            "École Françoise Dolto",
            "École maternelle Françoise Dolto",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        strings.sort();
        let pool = StringPool::create(strings.into_iter(), dir.path(), &dir.path().join("strings"))
            .expect("StringPool::create");
        let score = |m: &PoiMatcher, t: &[(&str, &str)], meters: f64| {
            let ll = LatLng::from(CellID(atp.s2_cell_id));
            let point = geo::Point::new(ll.lng.deg(), ll.lat.deg() + meters / 111_195.0);
            let feature = Feature {
                id: 1,
                tags: t
                    .iter()
                    .flat_map(|(k, v)| {
                        [
                            pool.lookup(k).unwrap() as u32,
                            pool.lookup(v).unwrap() as u32,
                        ]
                    })
                    .collect(),
                geometry_wkb: crate::geometry::encode_wkb(&geo::Geometry::from(point)),
                ..Default::default()
            };
            let candidate = OsmCandidate {
                feature: &feature,
                strings: &pool,
            };
            (
                m.score_osm_candidate(&candidate),
                m.rule(&candidate),
                m.may_share(&candidate),
            )
        };
        let address = [
            ("addr:street", "Rue Frédéric Lanne"),
            ("addr:housenumber", "8"),
        ];

        // The same ID: a match even far away, and for a building
        // without any school tags.
        let (s, rule, share) = score(&m, &[("building", "yes"), ("ref:UAI", "0351686T")], 0.0);
        assert!((s - 1.0).abs() < 1e-6, "{s}");
        assert_eq!(rule, "official_id");
        assert!(share);
        let (s, _, _) = score(&m, &[("amenity", "school"), ("ref:UAI", "0351686T")], 390.0);
        assert!(s >= official_id::SAME_ID_MIN_SCORE, "{s}");

        // Without an ID in OSM, the address still counts.
        let mut unknown = vec![("amenity", "school"), ("name", "École Françoise Dolto")];
        unknown.extend(address);
        let (s, rule, share) = score(&m, &unknown, 0.0);
        assert!(s > 0.5 && s < 1.0, "{s}");
        assert_eq!(rule, "address");
        assert!(!share);

        // A different ID at the same address: the école maternelle
        // next door, which no other evidence can outweigh.
        let mut other = vec![
            ("amenity", "school"),
            ("name", "École maternelle Françoise Dolto"),
            ("ref:UAI", "0350001A"),
        ];
        other.extend(address);
        let (s, rule, _) = score(&m, &other, 0.0);
        assert_eq!(s, 0.0);
        assert_eq!(rule, "official_id");
    }
}
