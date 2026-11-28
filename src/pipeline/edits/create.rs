//! Creates: new OSM features for AllThePlaces features that OSM lacks,
//! from `missing_in_osm` rows and from tenants of a `located_in` host.
//! See "Create" and "Located in another place" in
//! `docs/notes/2026-11-23-edits-parquet.md`, and the evaluation in
//! `docs/notes/2026-11-24-create-suggestions-eval.md`.
//!
//! A missing place is weaker evidence than a matched one: the matcher
//! may have missed OSM's feature, or the spider's position may be off.
//! So a feature only becomes a create when nothing speaks against it:
//!
//! - OSM has no feature of an agreeing kind within 50 m (100 m for
//!   large sites), and none of any kind within 100 m whose name shares
//!   a distinctive word (`match.evidence.similar_osm`), since that is
//!   more likely a missed match than a missing place;
//! - the spider's matched features lie close to their OSM match, and
//!   its positions aren't Google Maps camera centers ([SpiderGate]);
//! - no other feature of the spider sits at exactly the same point, as
//!   when a spider puts every store at the center of its town;
//! - OSM has no deletion candidate of the same brand within 200 m,
//!   since that is likely the same place, moved or renamed;
//! - the kind is one that OSM maps as a feature of its own, and an
//!   office has a street address ([excluded_kind]).
//!
//! Features of one place merge into one create: within 30 m, of
//! agreeing kinds, and sharing a phone number, an email address or a
//! brand, such as the Audi, Škoda and Volkswagen spiders listing one
//! dealer.

use super::writer::{Edit, Host};
use super::{Change, Reason, Source, Tags, contact, opening_hours};
use crate::matchers::category::{categories, categories_agree, main_category};
use crate::matchers::contact::Nondescript;
use crate::utils::opening_hours as oh;
use crate::utils::parquet::{get_string, get_u64};
use anyhow::{Context, Result};
use arrow::array::{Array, ListArray, StringArray, StructArray, UInt64Array};
use geo::{Closest, ClosestPoint, Contains, Distance, Geometry, Haversine, InteriorPoint, Point};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Serializes a geometry as WKB, which keeps its coordinates exactly.
mod wkb_geometry {
    use geo::Geometry;
    use geo_traits::to_geo::ToGeoGeometry;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(g: &Geometry<f64>, s: S) -> Result<S::Ok, S::Error> {
        crate::geometry::encode_wkb(g).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Geometry<f64>, D::Error> {
        let bytes: Vec<u8> = Vec::deserialize(d)?;
        wkb::reader::read_wkb(&bytes)
            .map(|g| g.to_geometry())
            .map_err(serde::de::Error::custom)
    }
}

/// Features closer than this, in meters, can merge into one create.
pub(super) const MERGE_RADIUS_M: f64 = 30.0;

/// A deletion candidate of the same brand closer than this, in meters,
/// is likely the same place, moved or renamed.
pub(super) const MOVED_RADIUS_M: f64 = 200.0;

/// A tenant farther than this from its host, in meters, gets no
/// create: the host or the tenant's position is likely wrong.
pub(super) const MAX_HOST_DISTANCE_M: f64 = 100.0;

/// A tenant of a host node within this distance, in meters, keeps its
/// own position.
pub(super) const HOST_NODE_RADIUS_M: f64 = 30.0;

/// How far inside its host polygon, or from its host node, a tenant
/// is placed when it has to be moved, in meters.
pub(super) const HOST_INSET_M: f64 = 2.0;

/// A spider needs this many `same` rows before we trust its positions.
pub(super) const MIN_SAME: u64 = 20;

/// ... and this share of them within 50 m of their OSM match.
pub(super) const MIN_CLOSE_SHARE: f64 = 0.8;

/// How many of a spider's matched features must have a `name` on both
/// sides before [SpiderGate] judges its names.
pub(super) const MIN_NAME_PAIRS: u64 = 20;

/// Below this share of matched features whose `name` agrees with
/// OSM's, a spider's names are taken for its own labels, such as
/// `kkiosk Faido EKZ MI` or `TKA BP Hägendorf`, and a create is named
/// after the brand instead.
pub(super) const MIN_NAME_AGREEMENT: f64 = 0.1;

/// How many bins of `spider-stats.parquet`'s `distance_m` histogram lie
/// below 50 m: below 1, 10, 25 and 50 m.
const CLOSE_BINS: usize = 4;

/// An AllThePlaces feature that OSM lacks, as the first pass sees it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Candidate {
    /// `source.tags` holds the ATP tags that a create may carry; see
    /// [is_create_key].
    pub source: Source,
    /// ATP's main category, such as `shop=bakery`.
    pub category: String,
    /// ATP's point, as (longitude, latitude).
    pub point: (f64, f64),
    /// From `match.evidence.similar_osm`: how far the nearest OSM
    /// feature of an agreeing kind is, if there is one within 100 m.
    pub similar_m: Option<f32>,
    /// Likewise, how far the nearest OSM feature of any kind is whose
    /// name shares a distinctive word with ATP's.
    pub similar_name_m: Option<f32>,
    /// For a tenant, the OSM feature it is in.
    pub host: Option<HostFeature>,
}

/// The host of a tenant, from the OSM side of its `located_in` row.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HostFeature {
    pub osm_type: String,
    pub osm_id: u64,
    pub name: Option<String>,
    pub opening_hours: Option<String>,
    #[serde(with = "wkb_geometry")]
    pub geometry: Geometry<f64>,
}

/// Why a candidate didn't become a create; counted for the log and
/// for `spider-stats.parquet`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Skip {
    /// A kind that OSM doesn't map as a feature of its own, or that we
    /// leave out for now; see [excluded_kind].
    Kind,
    /// OSM has a feature of the same kind within 50 m.
    Similar,
    /// The spider's positions aren't good enough; see [SpiderGate].
    SpiderPosition,
    /// Another feature of the spider is at the same point.
    SharedPosition,
    /// Another feature of the spider, of an agreeing kind, is within
    /// [TWIN_RADIUS_M]; see [twins].
    Twin,
    /// OSM has a deletion candidate of the same brand nearby.
    Moved,
    /// The spider lists a place of another kind right here that
    /// matched OSM: this one is likely part of it, such as the shop of
    /// a fuel station.
    MatchedSite,
    /// The tenant is too far from its host.
    HostFar,
    /// The point has only three decimals, about 100 m, and no host
    /// says where the place is.
    CoarsePosition,
    /// Merged sources disagree on the kind.
    Incomplete,
    /// No `name`, `brand` or `operator` is left, as for a spider with
    /// its own names and no brand.
    Unnamed,
}

impl Skip {
    pub(crate) const ALL: [Skip; 11] = [
        Skip::Kind,
        Skip::Similar,
        Skip::SpiderPosition,
        Skip::SharedPosition,
        Skip::Twin,
        Skip::Moved,
        Skip::MatchedSite,
        Skip::HostFar,
        Skip::CoarsePosition,
        Skip::Incomplete,
        Skip::Unnamed,
    ];

    /// The name in the log and in `spider-stats.parquet`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Skip::Kind => "kind",
            Skip::Similar => "similar_nearby",
            Skip::SpiderPosition => "spider_position",
            Skip::SharedPosition => "shared_position",
            Skip::Twin => "twin",
            Skip::Moved => "moved",
            Skip::MatchedSite => "matched_site",
            Skip::HostFar => "host_far",
            Skip::CoarsePosition => "coarse_position",
            Skip::Incomplete => "incomplete",
            Skip::Unnamed => "unnamed",
        }
    }
}

fn get<'a>(tags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

/// Why AllThePlaces features of this kind never become creates, or
/// `None` if they may. Charge points wait for a decision of their own
/// (see the plan); money transfer agents and ATMs are services of
/// their host, as are postal partners, which get tag edits instead;
/// transit stops need tagging (platforms, stop positions, route
/// relations) that a point can't give.
pub(crate) fn excluded_kind(tags: &[(String, String)]) -> Option<&'static str> {
    let amenity = get(tags, "amenity");
    if amenity == Some("charging_station") {
        return Some("charging_station");
    }
    if matches!(amenity, Some("money_transfer" | "atm" | "bureau_de_change")) {
        return Some("service");
    }
    // Street furniture and machines: OSM tells them apart by `ref`,
    // collection times and the like, which a create doesn't carry,
    // and spiders name a parcel locker after the shop it stands at.
    if matches!(
        amenity,
        Some(
            "post_box"
                | "parcel_locker"
                | "vending_machine"
                | "bicycle_rental"
                | "kick-scooter_rental"
                | "letter_box"
                | "recycling"
                | "drinking_water"
                | "bench"
                | "telephone"
                | "toilets"
                | "waste_basket"
                | "parking"
                | "bicycle_parking"
                | "motorcycle_parking"
        )
    ) {
        return Some("street_furniture");
    }
    if get(tags, "post_office") == Some("post_partner") {
        return Some("service");
    }
    if get(tags, "public_transport").is_some()
        || get(tags, "railway").is_some()
        || get(tags, "highway") == Some("bus_stop")
        || matches!(amenity, Some("taxi" | "ferry_terminal" | "bus_station"))
    {
        return Some("transit");
    }
    if get(tags, "disused:shop").is_some() || get(tags, "disused:amenity").is_some() {
        return Some("disused");
    }
    // An office without a street address is often an advisor who
    // visits customers at home, such as the 4,389 "AXA Épargne et
    // Protection" advisors of `axa_fr`, which the spider places at the
    // town center; see docs/notes/2026-11-24-offices.md.
    if get(tags, "office").is_some()
        && ["addr:street_address", "addr:street", "addr:full"]
            .iter()
            .all(|key| get(tags, key).is_none())
    {
        return Some("office_without_address");
    }
    None
}

/// Spiders whose features never become creates: `gbfs` gives virtual
/// parking zones of dockless scooters and bikes, such as Dott's and
/// Voi's, next to real docks; `standard_chartered_bank` gives every
/// branch the brand of Standard Chartered Taiwan (`渣打國際商業銀行`,
/// Q62267023), even in Kenya or India; `little_free_library` is a
/// registry of book boxes, many in private front yards and named
/// after their owners, which keeps boxes long after they are gone,
/// and which nobody can check from home; `worldcat` is the OCLC
/// registry of member institutions rather than of library buildings,
/// so it gives library systems at their head office, consortia, whole
/// universities, PO boxes and towns (`Stadt Kaufbeuren`), even among
/// its public libraries.
pub(crate) fn excluded_spider(spider: &str) -> bool {
    matches!(
        spider,
        "gbfs" | "standard_chartered_bank" | "little_free_library" | "worldcat"
    )
}

/// Keys that a create takes from AllThePlaces as they are: what the
/// place is, and its names. Contact tags and `opening_hours` go through
/// the rules of the modifies instead, so that they come out as a modify
/// would write them. Everything else is left out: AllThePlaces'
/// own properties, the spider's store number (`ref`, see
/// brawer/osmdiffs#908), and attributes such as `fuel:*`, `payment:*`
/// or `wheelchair`, which spiders often set for the whole brand rather
/// than for the place. So are addresses, which spiders give in many
/// forms (`addr:city=Mount Kenya East`, `addr:postcode=Masai Mara`).
const CREATE_KEYS: &[&str] = &[
    "shop",
    "amenity",
    "tourism",
    "leisure",
    "healthcare",
    "craft",
    "office",
    "name",
    "brand",
    "brand:wikidata",
    "operator",
    "operator:wikidata",
    "network",
    "network:wikidata",
    "branch",
    "cuisine",
    "religion",
    "denomination",
    "vending",
];

/// Whether a create may carry the ATP tag `key`: one of [CREATE_KEYS],
/// or a name in another language, such as `name:th` or `brand:en`.
pub(super) fn is_create_key(key: &str) -> bool {
    if CREATE_KEYS.contains(&key) {
        return true;
    }
    let Some((base, lang)) = key.split_once(':') else {
        return false;
    };
    let is_lang = |l: &str| {
        let (code, rest) = l.split_once('-').unwrap_or((l, ""));
        (2..=3).contains(&code.len())
            && code.bytes().all(|b| b.is_ascii_lowercase())
            && rest.bytes().all(|b| b.is_ascii_alphanumeric())
    };
    matches!(base, "name" | "brand" | "branch" | "operator") && is_lang(lang)
}

/// The category of a create, as [main_category] but passing over
/// `shop=yes` when there is another, as on a fuel station that says it
/// has a shop.
pub(crate) fn create_category(tags: &[(String, String)]) -> Option<String> {
    let all = categories(tags.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    all.iter()
        .find(|c| *c != "shop=yes")
        .or(all.first())
        .cloned()
}

/// How close an OSM feature of an agreeing kind may be, in meters,
/// before we take it for the place itself: farther for large sites,
/// whose position the spider and OSM may put far apart.
pub(super) fn similar_radius_m(category: &str) -> f32 {
    const LARGE: &[&str] = &[
        "amenity=fuel",
        "amenity=hospital",
        "amenity=university",
        "amenity=college",
        "amenity=school",
        "amenity=marketplace",
        "tourism=hotel",
        "tourism=motel",
        "tourism=resort",
        "tourism=camp_site",
        "tourism=caravan_site",
        "shop=car",
        "shop=mall",
        "shop=department_store",
        "shop=supermarket",
        "shop=wholesale",
        "shop=garden_centre",
        "shop=doityourself",
        "shop=furniture",
    ];
    if LARGE.contains(&category) || category.starts_with("leisure=") {
        100.0
    } else {
        50.0
    }
}

/// What the spiders' matched features tell about their positions, from
/// `spider-stats.parquet`.
#[derive(Debug, Default)]
pub(crate) struct SpiderGate {
    trusted: HashSet<String>,
    /// Spiders whose names rarely agree with OSM's; see
    /// [MIN_NAME_AGREEMENT].
    own_names: HashSet<String>,
}

impl SpiderGate {
    pub(crate) fn allows(&self, spider: &str) -> bool {
        self.trusted.contains(spider)
    }

    /// Whether `spider`'s names are its own labels rather than the
    /// names on the shop front.
    pub(crate) fn has_own_names(&self, spider: &str) -> bool {
        self.own_names.contains(spider)
    }

    /// Whether a spider whose matched features have `agreeing` and
    /// `differing` names names its features as OSM would.
    pub(crate) fn names_agree(agreeing: u64, differing: u64) -> bool {
        let total = agreeing + differing;
        total < MIN_NAME_PAIRS || agreeing as f64 >= MIN_NAME_AGREEMENT * total as f64
    }

    #[cfg(test)]
    pub(crate) fn trusting(spiders: &[&str]) -> SpiderGate {
        SpiderGate {
            trusted: spiders.iter().map(|s| s.to_string()).collect(),
            own_names: HashSet::new(),
        }
    }

    /// Whether a spider with these counts may propose creates: at
    /// least [MIN_SAME] matched features, [MIN_CLOSE_SHARE] of them
    /// within 50 m of their OSM match, and no Google Maps camera
    /// centers, which conflate only corrects for features that match.
    pub(crate) fn passes(same: u64, distance_bins: &[u64], camera: bool) -> bool {
        if camera || same < MIN_SAME {
            return false;
        }
        let total: u64 = distance_bins.iter().sum();
        let close: u64 = distance_bins.iter().take(CLOSE_BINS).sum();
        total > 0 && close as f64 >= MIN_CLOSE_SHARE * total as f64
    }

    pub(crate) fn open(spider_stats: &Path) -> Result<SpiderGate> {
        let file =
            File::open(spider_stats).with_context(|| format!("could not open {spider_stats:?}"))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
        let mut gate = SpiderGate::default();
        let (mut passed, mut failed) = (0u64, 0u64);
        for batch in reader {
            let batch = batch?;
            let column = |name: &str| {
                batch
                    .column_by_name(name)
                    .with_context(|| format!("no column {name}"))
            };
            let spiders = column("spider")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("spider is not a string")?;
            let u64s = |name: &str| -> Result<&UInt64Array> {
                column(name)?
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .with_context(|| format!("{name} is not UInt64"))
            };
            let same = u64s("same")?;
            let exact = u64s("exact_coords_hits")?;
            let hits = u64s("google_camera_coords_hits")?;
            let control = u64s("google_camera_coords_control")?;
            let bins = column("distance_m")?
                .as_any()
                .downcast_ref::<ListArray>()
                .context("distance_m is not a list")?;
            let tags = column("tags")?
                .as_any()
                .downcast_ref::<ListArray>()
                .context("tags is not a list")?;
            for row in 0..batch.num_rows() {
                let value = |a: &UInt64Array| if a.is_valid(row) { a.value(row) } else { 0 };
                // The same test as `atp::camera::is_google_camera_coords`.
                let h = value(hits);
                let camera = h >= 8 && h >= 4 * (value(control) + 1) && h * 10 >= h + value(exact);
                let row_bins = bins.value(row);
                let row_bins = row_bins
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .context("distance_m items are not UInt64")?;
                if Self::passes(same.value(row), row_bins.values(), camera) {
                    passed += 1;
                    gate.trusted.insert(spiders.value(row).to_string());
                } else {
                    failed += 1;
                }
                if tags.is_valid(row) {
                    let entries = tags.value(row);
                    let entries = entries
                        .as_any()
                        .downcast_ref::<StructArray>()
                        .context("tags items are not structs")?;
                    for i in 0..entries.len() {
                        if get_string(entries, "tag", i)? == "name"
                            && !Self::names_agree(
                                get_u64(entries, "agreeing", i)?,
                                get_u64(entries, "differing", i)?,
                            )
                        {
                            gate.own_names.insert(spiders.value(row).to_string());
                        }
                    }
                }
            }
        }
        log::info!(
            passed = passed,
            failed = failed,
            own_names = gate.own_names.len();
            "make_edits: spiders allowed to propose creates"
        );
        Ok(gate)
    }
}

/// Points on a grid of cells, to find the ones near a point quickly.
struct Grid {
    cell_deg: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(radius_m: f64) -> Grid {
        Grid {
            cell_deg: radius_m / 111_320.0,
            cells: HashMap::new(),
        }
    }

    fn cell(&self, (x, y): (f64, f64)) -> (i64, i64) {
        (
            (x / self.cell_deg).floor() as i64,
            (y / self.cell_deg).floor() as i64,
        )
    }

    fn insert(&mut self, point: (f64, f64), item: usize) {
        let cell = self.cell(point);
        self.cells.entry(cell).or_default().push(item);
    }

    /// The items in the cells that may hold points within the grid's
    /// radius of `point`; callers check the actual distance.
    fn near(&self, point: (f64, f64)) -> impl Iterator<Item = usize> + '_ {
        let (cx, cy) = self.cell(point);
        let cos = point.1.to_radians().cos().max(0.01);
        let dx = (1.0 / cos).ceil() as i64;
        (cx - dx..=cx + dx)
            .flat_map(move |x| (cy - 1..=cy + 1).map(move |y| (x, y)))
            .filter_map(|c| self.cells.get(&c))
            .flatten()
            .copied()
    }
}

fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    Haversine.distance(Point::new(a.0, a.1), Point::new(b.0, b.1))
}

/// Points to find the ones near a point, like [Grid], but compact:
/// per point, its coordinates and four bytes, which matters for the
/// statistics that `make_edits` keeps over the whole planet. All
/// points are added before the first lookup, which sorts them by cell.
#[derive(Debug, Default)]
struct PointIndex {
    cell_deg: f64,
    points: Vec<(f64, f64)>,
    /// Indices into `points`, sorted by cell; made on the first lookup.
    by_cell: std::sync::OnceLock<Vec<u32>>,
}

impl PointIndex {
    fn new(radius_m: f64) -> PointIndex {
        PointIndex {
            cell_deg: radius_m / 111_320.0,
            ..PointIndex::default()
        }
    }

    fn len(&self) -> usize {
        self.points.len()
    }

    fn cell(&self, (x, y): (f64, f64)) -> (i64, i64) {
        (
            (x / self.cell_deg).floor() as i64,
            (y / self.cell_deg).floor() as i64,
        )
    }

    /// Adds `point`, and returns its index.
    fn push(&mut self, point: (f64, f64)) -> usize {
        self.by_cell.take();
        self.points.push(point);
        self.points.len() - 1
    }

    /// The indices of the points in the cells that may hold points
    /// within the radius of `point`, as [Grid::near]; callers check
    /// the actual distance.
    fn near(&self, point: (f64, f64)) -> impl Iterator<Item = usize> + '_ {
        let by_cell = self.by_cell.get_or_init(|| {
            let mut order: Vec<u32> = (0..self.points.len() as u32).collect();
            order.sort_by_key(|&i| self.cell(self.points[i as usize]));
            order
        });
        let (cx, cy) = self.cell(point);
        let cos = point.1.to_radians().cos().max(0.01);
        let dx = (1.0 / cos).ceil() as i64;
        (cx - dx..=cx + dx).flat_map(move |x| {
            let cell = |i: &u32| self.cell(self.points[*i as usize]);
            let start = by_cell.partition_point(|i| cell(i) < (x, cy - 1));
            let end = by_cell.partition_point(|i| cell(i) <= (x, cy + 1));
            by_cell[start..end].iter().map(|&i| i as usize)
        })
    }
}

/// Distinct strings, each by a small number, such as the categories or
/// brands of many places.
#[derive(Debug, Default)]
struct Interned {
    ids: HashMap<String, u32>,
    names: Vec<String>,
}

impl Interned {
    fn id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), id);
        id
    }

    fn get(&self, name: &str) -> Option<u32> {
        self.ids.get(name).copied()
    }

    fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }
}

/// Where each spider has features that matched OSM, with their kind,
/// to spot features that are part of a matched site, such as the shop
/// of a fuel station that the same spider lists.
#[derive(Default)]
pub(crate) struct MatchedSites {
    points: Option<PointIndex>,
    /// By point, a hash of the spider and the category.
    sites: Vec<(u64, u32)>,
    categories: Interned,
}

fn spider_hash(spider: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    spider.hash(&mut hasher);
    hasher.finish()
}

impl MatchedSites {
    pub(crate) fn add(&mut self, spider: &str, category: String, point: (f64, f64)) {
        let points = self
            .points
            .get_or_insert_with(|| PointIndex::new(MERGE_RADIUS_M));
        points.push(point);
        let category = self.categories.id(&category);
        self.sites.push((spider_hash(spider), category));
    }

    /// Whether `spider` has a matched feature of another kind than
    /// `category` within [MERGE_RADIUS_M] of `point`.
    fn has_other_kind_near(&self, spider: &str, category: &str, point: (f64, f64)) -> bool {
        let Some(points) = &self.points else {
            return false;
        };
        let spider = spider_hash(spider);
        points.near(point).any(|i| {
            let (s, c) = self.sites[i];
            s == spider
                && !categories_agree(self.categories.name(c), category)
                && distance_m(points.points[i], point) <= MERGE_RADIUS_M
        })
    }
}

/// Words in a name that say a bank's site has machines only, or is a
/// bus: OSM maps those as `amenity=atm`, or not at all.
const SELF_SERVICE_WORDS: &[&str] = &[
    "sb-center",
    "sb center",
    "sb-filiale",
    "sb-stelle",
    "sb-standort",
    "sb-geschäftsstelle",
    "sb-bereich",
    "selbstbedienung",
    "mobile sparkasse",
    "mobile filiale",
    "atm lobby",
    "atmコーナー",
];

/// Whether a bank's `name` says it is a self-service site or a bus.
pub(super) fn is_self_service(tags: &[(String, String)]) -> bool {
    get(tags, "amenity") == Some("bank")
        && get(tags, "name").is_some_and(|name| {
            let name = name.to_lowercase();
            SELF_SERVICE_WORDS.iter().any(|w| name.contains(w))
        })
}

/// Deletion candidates by brand, to find places that moved.
#[derive(Default)]
pub(crate) struct DeletionCandidates {
    points: Option<PointIndex>,
    /// By point, its `brand:wikidata`.
    brands: Vec<u32>,
    names: Interned,
}

impl DeletionCandidates {
    pub(crate) fn add(&mut self, brand_wikidata: &str, point: (f64, f64)) {
        let points = self
            .points
            .get_or_insert_with(|| PointIndex::new(MOVED_RADIUS_M));
        points.push(point);
        let brand = self.names.id(brand_wikidata);
        self.brands.push(brand);
    }

    pub(crate) fn len(&self) -> usize {
        self.points.as_ref().map_or(0, PointIndex::len)
    }

    /// Whether a deletion candidate with a `brand:wikidata` among
    /// `brands` lies within [MOVED_RADIUS_M] of `point`.
    fn has_near(&self, brands: &[&str], point: (f64, f64)) -> bool {
        let Some(points) = &self.points else {
            return false;
        };
        let ids: Vec<u32> = brands.iter().filter_map(|b| self.names.get(b)).collect();
        if ids.is_empty() {
            return false;
        }
        points.near(point).any(|i| {
            ids.contains(&self.brands[i]) && distance_m(points.points[i], point) <= MOVED_RADIUS_M
        })
    }
}

/// The `brand:wikidata` IDs of `tags`.
fn brand_ids(tags: &[(String, String)]) -> Vec<&str> {
    get(tags, "brand:wikidata")
        .map(|v| {
            v.split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// What the create rules need besides the candidates.
pub(crate) struct CreateContext<'a> {
    pub nondescript: &'a Nondescript,
    pub shared_osm_values: &'a HashSet<String>,
    pub opening_hours: &'a opening_hours::Gate,
    pub spiders: &'a SpiderGate,
    pub deletion_candidates: &'a DeletionCandidates,
    pub positions: &'a Positions,
    pub matched_sites: &'a MatchedSites,
}

/// What happened to one spider's candidates.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SpiderCreates {
    /// Features that could have become creates: `missing_in_osm` and
    /// tenants with a category.
    pub candidates: u64,
    /// Creates that have a feature of the spider among their sources.
    pub creates: u64,
    /// Of [Self::creates], those with sources of other spiders too.
    pub merged: u64,
    /// Candidates left out, by reason.
    pub skipped: BTreeMap<Skip, u64>,
    /// Candidates with an OSM feature within 30 m whose name shares a
    /// distinctive word, most likely a match that conflate missed.
    pub similar_name_30m: u64,
    /// Whether the spider's names were replaced by its brand; see
    /// [SpiderGate::has_own_names].
    pub own_names: bool,
}

impl SpiderCreates {
    fn add(&mut self, other: &SpiderCreates) {
        self.candidates += other.candidates;
        self.creates += other.creates;
        self.merged += other.merged;
        for (skip, n) in &other.skipped {
            *self.skipped.entry(*skip).or_default() += n;
        }
        self.similar_name_30m += other.similar_name_30m;
        self.own_names |= other.own_names;
    }
}

/// How close a feature that shares a word of its name must be to
/// count in [SpiderCreates::similar_name_30m], in meters.
pub(crate) const SIMILAR_NAME_NEAR_M: f32 = 30.0;

/// Counts of the candidates, by spider, for the log and for
/// `spider-stats.parquet`.
#[derive(Debug, Default)]
pub(crate) struct CreateStats {
    pub spiders: BTreeMap<String, SpiderCreates>,
    /// Sources that merged into the create of another source.
    pub merged: u64,
}

impl CreateStats {
    fn spider(&mut self, spider: &str) -> &mut SpiderCreates {
        if !self.spiders.contains_key(spider) {
            self.spiders
                .insert(spider.to_string(), SpiderCreates::default());
        }
        self.spiders.get_mut(spider).expect("just inserted")
    }

    /// Counts `candidate`, before any rule judged it.
    pub(crate) fn count_candidate(&mut self, candidate: &Candidate) {
        let s = self.spider(&candidate.source.spider);
        s.candidates += 1;
        if candidate
            .similar_name_m
            .is_some_and(|d| d <= SIMILAR_NAME_NEAR_M)
        {
            s.similar_name_30m += 1;
        }
    }

    pub(crate) fn count_skip(&mut self, spider: &str, skip: Skip) {
        *self.spider(spider).skipped.entry(skip).or_default() += 1;
    }

    /// Adds the counts of `other`.
    pub(crate) fn add(&mut self, other: &CreateStats) {
        for (spider, counts) in &other.spiders {
            self.spider(spider).add(counts);
        }
        self.merged += other.merged;
    }

    /// Marks the spiders with their own names, among those counted.
    fn mark_own_names(&mut self, gate: &SpiderGate) {
        for (spider, s) in self.spiders.iter_mut() {
            s.own_names = gate.has_own_names(spider);
        }
    }

    pub(crate) fn candidates(&self) -> u64 {
        self.spiders.values().map(|s| s.candidates).sum()
    }

    pub(crate) fn skipped(&self, skip: Skip) -> u64 {
        self.spiders
            .values()
            .filter_map(|s| s.skipped.get(&skip))
            .sum()
    }
}

/// Why `candidate` can never become a create, judged by itself alone:
/// its kind, or a similar OSM feature nearby.
pub(crate) fn early_skip(candidate: &Candidate) -> Option<Skip> {
    if excluded_kind(&candidate.source.tags).is_some()
        || excluded_spider(&candidate.source.spider)
        || is_self_service(&candidate.source.tags)
    {
        return Some(Skip::Kind);
    }
    if candidate
        .similar_m
        .is_some_and(|d| d <= similar_radius_m(&candidate.category))
        || candidate.similar_name_m.is_some()
    {
        return Some(Skip::Similar);
    }
    None
}

/// Why `candidate` can't become a create on its own, before merging.
fn skip_reason(candidate: &Candidate, ctx: &CreateContext) -> Option<Skip> {
    if let Some(skip) = early_skip(candidate) {
        return Some(skip);
    }
    if !ctx.spiders.allows(&candidate.source.spider) {
        return Some(Skip::SpiderPosition);
    }
    if ctx
        .positions
        .is_shared(&candidate.source.spider, candidate.point)
    {
        return Some(Skip::SharedPosition);
    }
    if ctx.matched_sites.has_other_kind_near(
        &candidate.source.spider,
        &candidate.category,
        candidate.point,
    ) {
        return Some(Skip::MatchedSite);
    }
    let brands = brand_ids(&candidate.source.tags);
    if !brands.is_empty() && ctx.deletion_candidates.has_near(&brands, candidate.point) {
        return Some(Skip::Moved);
    }
    None
}

/// Where each spider has features without a match, to spot features
/// of one spider at the same point, as when a spider puts every store
/// at the center of its town. Keyed by a hash of the spider and the
/// point to about 1 cm, holding a hash of the first `ref` there and
/// whether another `ref` is there too.
#[derive(Debug, Default)]
pub(crate) struct Positions {
    seen: HashMap<u64, (u64, bool)>,
}

impl Positions {
    fn key(spider: &str, (x, y): (f64, f64)) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        (spider, (x * 1e7).round() as i64, (y * 1e7).round() as i64).hash(&mut hasher);
        hasher.finish()
    }

    pub(crate) fn add(&mut self, spider: &str, point: (f64, f64), atp_ref: Option<&str>) {
        let mut hasher = std::hash::DefaultHasher::new();
        atp_ref.hash(&mut hasher);
        let r = hasher.finish();
        self.seen
            .entry(Self::key(spider, point))
            .and_modify(|(first, shared)| *shared |= *first != r)
            .or_insert((r, false));
    }

    fn is_shared(&self, spider: &str, point: (f64, f64)) -> bool {
        self.seen
            .get(&Self::key(spider, point))
            .is_some_and(|(_, shared)| *shared)
    }
}

/// Normalized phone numbers and email addresses of `tags`, to tell
/// whether two features are one business.
fn contact_keys(tags: &[(String, String)]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for (k, v) in tags {
        let base = k.strip_prefix("contact:").unwrap_or(k);
        for part in v.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            match base {
                "phone" => {
                    if let Ok(n) = phonenumber::parse(None, part)
                        && phonenumber::is_valid(&n)
                    {
                        keys.insert(format!(
                            "phone:{}",
                            n.format().mode(phonenumber::Mode::E164)
                        ));
                    }
                }
                "email" => {
                    keys.insert(format!("email:{}", part.to_lowercase()));
                }
                _ => {}
            }
        }
    }
    keys
}

/// Whether candidates `a` and `b` describe one place.
fn same_place(
    a: &Candidate,
    b: &Candidate,
    a_keys: &HashSet<String>,
    b_keys: &HashSet<String>,
    nondescript: &Nondescript,
) -> bool {
    if a.source.spider == b.source.spider {
        // One spider listing two places, such as two pharmacies of a
        // chain in one mall.
        return false;
    }
    match (&a.host, &b.host) {
        (Some(x), Some(y)) if (&x.osm_type, x.osm_id) != (&y.osm_type, y.osm_id) => return false,
        (Some(_), None) | (None, Some(_)) => return false,
        _ => {}
    }
    if !categories_agree(&a.category, &b.category) {
        return false;
    }
    if distance_m(a.point, b.point) > MERGE_RADIUS_M {
        return false;
    }
    let shared_contact = a_keys.intersection(b_keys).any(|k| {
        let (family, value) = k.split_once(':').unwrap_or(("", k));
        !nondescript.is_nondescript_value(family, value)
    });
    let a_brands = brand_ids(&a.source.tags);
    let shared_brand = brand_ids(&b.source.tags)
        .iter()
        .any(|id| a_brands.contains(id));
    shared_contact || shared_brand
}

/// Groups of candidate indices that are one place, each in the fixed
/// order of the candidates.
fn merge_groups(candidates: &[Candidate], nondescript: &Nondescript) -> Vec<Vec<usize>> {
    let mut grid = Grid::new(MERGE_RADIUS_M);
    for (i, c) in candidates.iter().enumerate() {
        grid.insert(c.point, i);
    }
    let keys: Vec<HashSet<String>> = candidates
        .iter()
        .map(|c| contact_keys(&c.source.tags))
        .collect();
    let mut parent: Vec<usize> = (0..candidates.len()).collect();
    // The spiders in each group, by its root: a group never takes two
    // features of one spider, not even through a third feature that is
    // like both.
    let mut spiders: Vec<BTreeSet<&str>> = candidates
        .iter()
        .map(|c| BTreeSet::from([c.source.spider.as_str()]))
        .collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (i, c) in candidates.iter().enumerate() {
        for j in grid.near(c.point) {
            if j <= i {
                continue;
            }
            if same_place(c, &candidates[j], &keys[i], &keys[j], nondescript) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b && spiders[a].is_disjoint(&spiders[b]) {
                    let (root, child) = (a.min(b), a.max(b));
                    parent[child] = root;
                    let moved = std::mem::take(&mut spiders[child]);
                    spiders[root].extend(moved);
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..candidates.len() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Keys whose values add up across the sources of one place, each with
/// its `:wikidata` key, such as `brand=Audi;Volkswagen`.
const SET_KEYS: [&str; 3] = ["brand", "operator", "network"];

fn split(value: Option<&str>) -> Vec<String> {
    value
        .map(|v| {
            v.split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A name and its Wikidata ID, either of which may be missing.
type Entry = (Option<String>, Option<String>);

/// Whether two entries name the same brand: by ID if both have one,
/// else by name.
fn same_entry(a: &Entry, b: &Entry) -> bool {
    match (&a.1, &b.1) {
        (Some(x), Some(y)) => x == y,
        _ => match (&a.0, &b.0) {
            (Some(x), Some(y)) => x.to_lowercase() == y.to_lowercase(),
            _ => false,
        },
    }
}

/// The tags of one new feature from the create tags of its `sources`,
/// in their fixed order: tags the sources agree on, and the brands,
/// operators and networks of all of them, with their Wikidata IDs
/// lined up with the names. A key whose sources disagree is dropped.
pub(super) fn merge_tags(sources: &[Tags]) -> Tags {
    let is_set_key = |k: &str| {
        SET_KEYS
            .iter()
            .any(|s| k == *s || k.strip_suffix(":wikidata") == Some(*s))
    };
    let mut merged: BTreeMap<String, Option<String>> = BTreeMap::new();
    for tags in sources {
        for (k, v) in tags.iter().filter(|(k, _)| !is_set_key(k)) {
            match merged.get(k) {
                None => {
                    merged.insert(k.clone(), Some(v.clone()));
                }
                Some(Some(old)) if old != v => {
                    merged.insert(k.clone(), None);
                }
                _ => {}
            }
        }
    }
    let mut out: Tags = merged
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect();
    for key in SET_KEYS {
        let id_key = format!("{key}:wikidata");
        let mut entries: Vec<Entry> = Vec::new();
        let mut aligned = true;
        for tags in sources {
            let (names, ids) = (split(get(tags, key)), split(get(tags, &id_key)));
            let own: Vec<Entry> = if names.is_empty() {
                ids.into_iter().map(|i| (None, Some(i))).collect()
            } else if ids.is_empty() {
                names.into_iter().map(|n| (Some(n), None)).collect()
            } else if names.len() == ids.len() {
                names
                    .into_iter()
                    .map(Some)
                    .zip(ids.into_iter().map(Some))
                    .collect()
            } else {
                aligned = false;
                continue;
            };
            for entry in own {
                match entries.iter_mut().find(|e| same_entry(e, &entry)) {
                    Some(e) => {
                        if e.1.is_none() {
                            e.1 = entry.1;
                        }
                        if e.0.is_none() {
                            e.0 = entry.0;
                        }
                    }
                    None => entries.push(entry),
                }
            }
        }
        if !aligned && entries.is_empty() {
            // Lists that don't line up: keep them only from a single
            // source, as it gives them. With several sources, the
            // others' entries count.
            if let [tags] = sources {
                for k in [key, id_key.as_str()] {
                    if let Some(v) = get(tags, k) {
                        out.push((k.to_string(), v.to_string()));
                    }
                }
            }
            continue;
        }
        entries.sort();
        if entries.iter().all(|e| e.0.is_some()) && !entries.is_empty() {
            let names: Vec<&str> = entries.iter().filter_map(|e| e.0.as_deref()).collect();
            out.push((key.to_string(), names.join(";")));
            if entries.iter().all(|e| e.1.is_some()) {
                let ids: Vec<&str> = entries.iter().filter_map(|e| e.1.as_deref()).collect();
                out.push((id_key, ids.join(";")));
            }
        } else if let [(None, Some(id))] = entries.as_slice() {
            out.push((id_key, id.clone()));
        }
    }
    out.sort();
    out
}

/// Whether a point has at most three decimals in both coordinates,
/// which leaves it up to about 100 m off, such as `100.938, 13.2283`
/// rounded by the spider.
pub(super) fn is_coarse((x, y): (f64, f64)) -> bool {
    let coarse = |v: f64| ((v * 1000.0).round() - v * 1000.0).abs() < 1e-6;
    coarse(x) && coarse(y)
}

/// Where a tenant goes, given its point and its host's geometry, or
/// `None` if it is too far from the host; see "Located in another
/// place" in the plan. A `coarse` point only says which host: the
/// tenant then goes to the host's interior point, or next to the host
/// node.
pub(super) fn tenant_position(
    point: (f64, f64),
    host: &Geometry<f64>,
    coarse: bool,
) -> Option<(f64, f64)> {
    let p = Point::new(point.0, point.1);
    match host {
        Geometry::Point(h) => {
            let d = Haversine.distance(p, *h);
            if coarse && d <= MAX_HOST_DISTANCE_M {
                // Next to the host node, towards the point, or north of
                // it if the point is the node.
                if d < HOST_INSET_M {
                    let north = HOST_INSET_M / 111_320.0;
                    return Some((h.x(), h.y() + north));
                }
                let t = HOST_INSET_M / d;
                Some((h.x() + (p.x() - h.x()) * t, h.y() + (p.y() - h.y()) * t))
            } else if d <= HOST_NODE_RADIUS_M {
                Some(point)
            } else if d <= MAX_HOST_DISTANCE_M {
                let t = HOST_INSET_M / d;
                Some((h.x() + (p.x() - h.x()) * t, h.y() + (p.y() - h.y()) * t))
            } else {
                None
            }
        }
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => {
            if host.contains(&p) && !coarse {
                return Some(point);
            }
            let edge = match host.closest_point(&p) {
                Closest::Intersection(q) | Closest::SinglePoint(q) => q,
                Closest::Indeterminate => return None,
            };
            if Haversine.distance(p, edge) > MAX_HOST_DISTANCE_M {
                return None;
            }
            let inside = host.interior_point()?;
            if coarse {
                return Some((inside.x(), inside.y()));
            }
            let at = |t: f64| {
                Point::new(
                    p.x() + (inside.x() - p.x()) * t,
                    p.y() + (inside.y() - p.y()) * t,
                )
            };
            // Where the line from the point to the interior point
            // enters the host, by bisection.
            let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
            for _ in 0..40 {
                let mid = (lo + hi) / 2.0;
                if host.contains(&at(mid)) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let length = Haversine.distance(p, inside);
            if length <= 0.0 {
                return Some((inside.x(), inside.y()));
            }
            let t = (hi + HOST_INSET_M / length).min(1.0);
            let q = at(t);
            if host.contains(&q) {
                Some((q.x(), q.y()))
            } else {
                Some((inside.x(), inside.y()))
            }
        }
        _ => None,
    }
}

/// Whether the opening hours of a tenant fall outside those of its
/// host on some day. Values that don't parse never do.
pub(super) fn hours_outside_host(tenant: &str, host: &str) -> bool {
    let (Some(tenant), Some(host)) = (oh::parse(tenant), oh::parse(host)) else {
        return false;
    };
    let (tenant, host) = (oh::days_as_written(&tenant), oh::days_as_written(&host));
    tenant.iter().zip(host.iter()).any(|(t, h)| {
        t.iter()
            .any(|&(start, end)| !h.iter().any(|&(hs, he)| hs <= start && end <= he))
    })
}

/// The ID of a create: stable across weeks while the same spiders list
/// the place under the same refs.
pub(super) fn create_id(sources: &[Source]) -> String {
    let mut parts: Vec<String> = sources
        .iter()
        .map(|s| match &s.atp_ref {
            Some(r) => format!("{}/{r}", s.spider),
            None => {
                let (x, y) = s.atp_point.unwrap_or_default();
                format!("{}/@{x:.5},{y:.5}", s.spider)
            }
        })
        .collect();
    parts.sort();
    parts.dedup();
    let text = format!("create/{}", parts.join(";"));
    crate::utils::sha256_hex(text.as_bytes())[..16].to_string()
}

/// Turns `candidates` into creates; see the module documentation.
/// How close two features of one spider and kind are taken for one
/// place listed twice, in meters.
pub(super) const TWIN_RADIUS_M: f64 = 10.0;

/// The candidates that have a twin: a feature of the same spider, of an
/// agreeing kind, within [TWIN_RADIUS_M]. That is one place listed
/// twice, as BP lists some Swiss stations, or two places that the
/// spider puts at one spot; either way we can't tell which to create.
fn twins(candidates: &[Candidate]) -> HashSet<usize> {
    let mut grid = Grid::new(TWIN_RADIUS_M);
    for (i, c) in candidates.iter().enumerate() {
        grid.insert(c.point, i);
    }
    let mut found = HashSet::new();
    for (i, a) in candidates.iter().enumerate() {
        for j in grid.near(a.point) {
            let b = &candidates[j];
            if j != i
                && a.source.spider == b.source.spider
                && categories_agree(&a.category, &b.category)
                && distance_m(a.point, b.point) <= TWIN_RADIUS_M
            {
                found.insert(i);
            }
        }
    }
    found
}

/// The creates of `candidates`, all in memory; `make_edits` streams
/// them instead, see [make_creates_sorted]. Kept for the tests, which
/// check the rules on a handful of candidates.
#[cfg(test)]
pub(crate) fn make_creates(
    candidates: Vec<Candidate>,
    ctx: &CreateContext,
) -> (Vec<Edit>, CreateStats) {
    let mut stats = CreateStats::default();
    let mut kept: Vec<Candidate> = Vec::new();
    for candidate in candidates {
        stats.count_candidate(&candidate);
        match skip_reason(&candidate, ctx) {
            Some(skip) => stats.count_skip(&candidate.source.spider, skip),
            None => kept.push(candidate),
        }
    }
    let edits = cluster_creates(kept, ctx, &mut stats);
    stats.mark_own_names(ctx.spiders);
    (edits, stats)
}

/// The creates of `candidates`, which come sorted by latitude, each
/// with its row in `conflated.parquet`; each create goes to `emit`.
/// Twins and merges only join candidates within [MERGE_RADIUS_M], so
/// a sweep along the latitude ([Sweep]) cuts the candidates into
/// clusters that can't join any other, and only the clusters that the
/// sweep line still touches are in memory. Each cluster is decided as
/// if it were all candidates, in the order of their rows, which gives
/// the same creates as deciding all candidates at once.
pub(crate) fn make_creates_sorted(
    candidates: impl Iterator<Item = Result<(u64, Candidate)>>,
    ctx: &CreateContext,
    mut emit: impl FnMut(Edit) -> Result<()>,
) -> Result<CreateStats> {
    let mut stats = CreateStats::default();
    let mut sweep = Sweep::new(MERGE_RADIUS_M.max(TWIN_RADIUS_M));
    let mut decide = |cluster: Vec<(u64, Candidate)>, stats: &mut CreateStats| -> Result<()> {
        let mut cluster = cluster;
        cluster.sort_by_key(|(row, _)| *row);
        let kept = cluster.into_iter().map(|(_, c)| c).collect();
        for edit in cluster_creates(kept, ctx, stats) {
            emit(edit)?;
        }
        Ok(())
    };
    for candidate in candidates {
        let (row, candidate) = candidate?;
        stats.count_candidate(&candidate);
        if let Some(skip) = skip_reason(&candidate, ctx) {
            stats.count_skip(&candidate.source.spider, skip);
            continue;
        }
        for cluster in sweep.insert(row, candidate) {
            decide(cluster, &mut stats)?;
        }
    }
    for cluster in sweep.finish() {
        decide(cluster, &mut stats)?;
    }
    stats.mark_own_names(ctx.spiders);
    Ok(stats)
}

/// The creates of `kept`, the candidates that passed [skip_reason], in
/// the order of their rows: twins are left out, the others merge into
/// places.
fn cluster_creates(
    kept: Vec<Candidate>,
    ctx: &CreateContext,
    stats: &mut CreateStats,
) -> Vec<Edit> {
    let twins = twins(&kept);
    let mut kept: Vec<Candidate> = kept
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| {
            if twins.contains(&i) {
                stats.count_skip(&c.source.spider, Skip::Twin);
                None
            } else {
                Some(c)
            }
        })
        .collect();
    kept.sort_by(|a, b| a.source.order_key().cmp(&b.source.order_key()));
    let mut edits = Vec::new();
    for group in merge_groups(&kept, ctx.nondescript) {
        let members: Vec<&Candidate> = group.iter().map(|&i| &kept[i]).collect();
        match create(&members, ctx) {
            Ok(edit) => {
                for m in &members {
                    let s = stats.spider(&m.source.spider);
                    s.creates += 1;
                    if members.len() > 1 {
                        s.merged += 1;
                    }
                }
                stats.merged += members.len() as u64 - 1;
                edits.push(edit);
            }
            Err(skip) => {
                for m in &members {
                    stats.count_skip(&m.source.spider, skip);
                }
            }
        }
    }
    edits
}

/// Cuts points, which come sorted by latitude, into clusters: the
/// connected parts of the graph that links two points within
/// `radius_m`, and maybe a few more. A cluster is done once the sweep
/// line has moved more than `radius_m` north of its northernmost point,
/// since no later point can then link to it.
struct Sweep {
    radius_m: f64,
    /// How far, in degrees of latitude, a point within `radius_m` can
    /// be at most; a little more than the distance on the sphere.
    window_deg: f64,
    cell_deg: f64,
    cells: HashMap<(i64, i64), Vec<u64>>,
    /// The points in `cells`, in the order they came.
    window: std::collections::VecDeque<u64>,
    points: HashMap<u64, Candidate>,
    parent: HashMap<u64, u64>,
    /// By the root of each cluster, its points and its northernmost
    /// latitude.
    clusters: HashMap<u64, (Vec<u64>, f64)>,
    last_lat: f64,
}

impl Sweep {
    fn new(radius_m: f64) -> Sweep {
        // A degree of latitude is 111,195 m on Haversine's sphere;
        // rounding down widens the window and the cells.
        Sweep {
            radius_m,
            window_deg: radius_m / 111_000.0,
            cell_deg: radius_m / 111_000.0,
            cells: HashMap::new(),
            window: Default::default(),
            points: HashMap::new(),
            parent: HashMap::new(),
            clusters: HashMap::new(),
            last_lat: f64::NEG_INFINITY,
        }
    }

    fn cell(&self, (x, y): (f64, f64)) -> (i64, i64) {
        (
            (x / self.cell_deg).floor() as i64,
            (y / self.cell_deg).floor() as i64,
        )
    }

    fn find(&mut self, mut id: u64) -> u64 {
        let mut path = Vec::new();
        while let Some(&p) = self.parent.get(&id)
            && p != id
        {
            path.push(id);
            id = p;
        }
        for p in path {
            self.parent.insert(p, id);
        }
        id
    }

    fn union(&mut self, a: u64, b: u64) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        let (keep, gone) = if self.clusters[&a].0.len() >= self.clusters[&b].0.len() {
            (a, b)
        } else {
            (b, a)
        };
        let (members, lat) = self.clusters.remove(&gone).expect("a cluster");
        let cluster = self.clusters.get_mut(&keep).expect("a cluster");
        cluster.0.extend(members);
        cluster.1 = cluster.1.max(lat);
        self.parent.insert(gone, keep);
    }

    /// Adds a point, and returns the clusters that are done.
    fn insert(&mut self, id: u64, candidate: Candidate) -> Vec<Vec<(u64, Candidate)>> {
        let (x, y) = candidate.point;
        debug_assert!(y >= self.last_lat, "points must come sorted by latitude");
        self.last_lat = y;
        let mut left = Vec::new();
        while let Some(&first) = self.window.front() {
            let point = self.points[&first].point;
            if point.1 >= y - self.window_deg {
                break;
            }
            self.window.pop_front();
            let cell = self.cell(point);
            if let Some(ids) = self.cells.get_mut(&cell) {
                ids.retain(|&i| i != first);
                if ids.is_empty() {
                    self.cells.remove(&cell);
                }
            }
            left.push(first);
        }
        let mut done = Vec::new();
        for first in left {
            let root = self.find(first);
            if self
                .clusters
                .get(&root)
                .is_some_and(|(_, lat)| *lat < y - self.window_deg)
            {
                done.push(self.take(root));
            }
        }

        let (cx, cy) = self.cell((x, y));
        let cos = y.to_radians().cos().max(0.01);
        let dx = (1.0 / cos).ceil() as i64 + 1;
        let mut near = Vec::new();
        for i in cx - dx..=cx + dx {
            for j in cy - 1..=cy + 1 {
                if let Some(ids) = self.cells.get(&(i, j)) {
                    for &other in ids {
                        if distance_m(self.points[&other].point, (x, y)) <= self.radius_m + 1.0 {
                            near.push(other);
                        }
                    }
                }
            }
        }
        self.points.insert(id, candidate);
        self.parent.insert(id, id);
        self.clusters.insert(id, (vec![id], y));
        for other in near {
            self.union(id, other);
        }
        let cell = self.cell((x, y));
        self.cells.entry(cell).or_default().push(id);
        self.window.push_back(id);
        done
    }

    fn take(&mut self, root: u64) -> Vec<(u64, Candidate)> {
        let (members, _) = self.clusters.remove(&root).expect("a cluster");
        members
            .into_iter()
            .map(|id| {
                self.parent.remove(&id);
                (id, self.points.remove(&id).expect("a point"))
            })
            .collect()
    }

    /// The clusters that are left at the end.
    fn finish(mut self) -> Vec<Vec<(u64, Candidate)>> {
        let roots: Vec<u64> = self.clusters.keys().copied().collect();
        roots.into_iter().map(|r| self.take(r)).collect()
    }
}

/// Whether `branch` names the branch, as `Bahnhof Bern` or `博多月隈`,
/// rather than giving its address or the spider's label, as `66 Main
/// St Salisbury, Massachusetts 01952 - USA`, `Target Lanesborough 2127`
/// or `ICICI Bank Branch - Bengaluru - Bidadi`: no ASCII digits, and
/// not the brand.
fn is_branch_name(branch: &str, brand: Option<&str>) -> bool {
    !branch.bytes().any(|b| b.is_ascii_digit())
        && !brand.is_some_and(|b| branch.to_lowercase().contains(&b.to_lowercase()))
}

/// The tags that a create takes from one source's `tags`: those that
/// [is_create_key] allows, with whitespace collapsed, and without
/// `shop=yes` beside another category, sorted. With `own_names`, the spider's
/// names are dropped and the brand's taken instead, as `name` and
/// `name:*`.
pub(super) fn create_tags(tags: &[(String, String)], own_names: bool) -> Tags {
    let has_other_category = categories(tags.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .iter()
        .any(|c| c != "shop=yes");
    let mut out: Tags = tags
        .iter()
        .filter(|(k, v)| is_create_key(k) && !v.trim().is_empty())
        .filter(|(k, v)| !(has_other_category && k == "shop" && v == "yes"))
        .filter(|(k, _)| !(own_names && (k == "name" || k.starts_with("name:"))))
        .filter(|(k, v)| k != "branch" || is_branch_name(v, get(tags, "brand")))
        .map(|(k, v)| {
            (
                k.clone(),
                v.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    if own_names {
        let names: Vec<(String, String)> = out
            .iter()
            .filter(|(k, _)| k == "brand" || (k.starts_with("brand:") && k != "brand:wikidata"))
            .map(|(k, v)| (k.replacen("brand", "name", 1), v.clone()))
            .collect();
        out.extend(names);
    }
    out.sort();
    out
}

fn create(members: &[&Candidate], ctx: &CreateContext) -> std::result::Result<Edit, Skip> {
    let first = members[0];
    let n = members.len() as f64;
    let mut point = (
        members.iter().map(|c| c.point.0).sum::<f64>() / n,
        members.iter().map(|c| c.point.1).sum::<f64>() / n,
    );
    let host = first.host.as_ref();
    let coarse = members.iter().all(|c| is_coarse(c.point));
    match host {
        Some(host) => {
            point = tenant_position(point, &host.geometry, coarse).ok_or(Skip::HostFar)?;
        }
        None if coarse => return Err(Skip::CoarsePosition),
        None => {}
    }
    let create_tags: Vec<Tags> = members
        .iter()
        .map(|c| create_tags(&c.source.tags, ctx.spiders.has_own_names(&c.source.spider)))
        .collect();
    let mut tags = merge_tags(&create_tags);
    if main_category(tags.iter().map(|(k, v)| (k.as_str(), v.as_str()))).is_none() {
        return Err(Skip::Incomplete);
    }
    if ["name", "brand", "operator"]
        .iter()
        .all(|k| get(&tags, k).is_none())
    {
        return Err(Skip::Unnamed);
    }
    let mut warnings = Vec::new();
    let empty = Tags::new();
    let contacts: Vec<contact::ContactSource> = members
        .iter()
        .map(|c| contact::ContactSource {
            tags: &c.source.tags,
        })
        .collect();
    for change in contact::contact_changes(
        &empty,
        &contacts,
        ctx.nondescript,
        ctx.shared_osm_values,
        Some(point),
        &mut warnings,
    ) {
        if let Some(value) = change.new {
            tags.push((change.key, value));
        }
    }
    let hours: Vec<(&str, &Tags)> = members
        .iter()
        .map(|c| (c.source.spider.as_str(), &c.source.tags))
        .collect();
    for change in
        opening_hours::opening_hours_changes(&empty, &hours, ctx.opening_hours, &mut warnings)
    {
        if change.reason != Reason::NeedsReview
            && let Some(value) = change.new
        {
            tags.push((change.key, value));
        }
    }
    tags.sort();
    if let (Some(host), Some(hours)) = (host, get(&tags, "opening_hours"))
        && let Some(host_hours) = &host.opening_hours
        && hours_outside_host(hours, host_hours)
    {
        warnings.push("hours_outside_host".to_string());
    }
    warnings.sort();
    warnings.dedup();
    let changes: Vec<Change> = tags
        .iter()
        .map(|(k, v)| Change::new(k, None, Some(v.clone()), Reason::MissingInOsm))
        .collect();
    let sources: Vec<Source> = members.iter().map(|c| c.source.clone()).collect();
    let geometry = Geometry::Point(Point::new(point.0, point.1));
    let atp_points: Vec<Point<f64>> = members
        .iter()
        .map(|c| Point::new(c.point.0, c.point.1))
        .collect();
    let atp_geometry = if atp_points.len() == 1 {
        Geometry::Point(atp_points[0])
    } else {
        Geometry::MultiPoint(geo::MultiPoint(atp_points))
    };
    Ok(Edit {
        id: create_id(&sources),
        action: "create",
        topics: vec!["new_poi"],
        osm: None,
        changes,
        importance: sources.iter().map(|s| s.importance).fold(0.0, f32::max),
        sources,
        geometry_wkb: crate::geometry::encode_wkb(&geometry),
        atp_geometry_wkb: Some(crate::geometry::encode_wkb(&atp_geometry)),
        host: host.map(|h| Host {
            osm_type: h.osm_type.clone(),
            osm_id: h.osm_id,
            name: h.name.clone(),
        }),
        warnings,
        s2_cell_id: super::read::s2_cell_id(point),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{LineString, Polygon};

    fn tags(t: &[(&str, &str)]) -> Tags {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn candidate(spider: &str, atp_ref: &str, point: (f64, f64), t: &[(&str, &str)]) -> Candidate {
        let tags = tags(t);
        let category =
            main_category(tags.iter().map(|(k, v)| (k.as_str(), v.as_str()))).expect("a category");
        Candidate {
            source: Source {
                spider: spider.to_string(),
                atp_ref: Some(atp_ref.to_string()),
                relation: "missing_in_osm".to_string(),
                atp_point: Some(point),
                tags,
                ..Source::default()
            },
            category,
            point,
            similar_m: None,
            similar_name_m: None,
            host: None,
        }
    }

    fn run(candidates: Vec<Candidate>, deletions: &DeletionCandidates) -> (Vec<Edit>, CreateStats) {
        let nondescript = Nondescript::none();
        let mut positions = Positions::default();
        for c in &candidates {
            positions.add(&c.source.spider, c.point, c.source.atp_ref.as_deref());
        }
        let spiders: Vec<&str> = candidates
            .iter()
            .map(|c| c.source.spider.as_str())
            .collect();
        make_creates(
            candidates.clone(),
            &CreateContext {
                nondescript: &nondescript,
                shared_osm_values: &HashSet::new(),
                opening_hours: &opening_hours::Gate::trusting(&["bakery"]),
                spiders: &SpiderGate::trusting(&spiders),
                deletion_candidates: deletions,
                positions: &positions,
                matched_sites: &MatchedSites::default(),
            },
        )
    }

    /// Runs both [make_creates] and [make_creates_sorted] on
    /// `candidates`, and checks that they agree.
    fn run_both(candidates: Vec<Candidate>) -> Vec<Edit> {
        let nondescript = Nondescript::none();
        let mut positions = Positions::default();
        for c in &candidates {
            positions.add(&c.source.spider, c.point, c.source.atp_ref.as_deref());
        }
        let spiders: Vec<&str> = candidates
            .iter()
            .map(|c| c.source.spider.as_str())
            .collect();
        let deletions = DeletionCandidates::default();
        let ctx = CreateContext {
            nondescript: &nondescript,
            shared_osm_values: &HashSet::new(),
            opening_hours: &opening_hours::Gate::trusting(&["bakery"]),
            spiders: &SpiderGate::trusting(&spiders),
            deletion_candidates: &deletions,
            positions: &positions,
            matched_sites: &MatchedSites::default(),
        };
        let (mut all, all_stats) = make_creates(candidates.clone(), &ctx);
        let mut by_latitude: Vec<(u64, Candidate)> = candidates
            .into_iter()
            .enumerate()
            .map(|(i, c)| (i as u64, c))
            .collect();
        by_latitude.sort_by(|a, b| a.1.point.1.total_cmp(&b.1.point.1).then(a.0.cmp(&b.0)));
        let mut streamed = Vec::new();
        let stats = make_creates_sorted(by_latitude.into_iter().map(Ok), &ctx, |e| {
            streamed.push(e);
            Ok(())
        })
        .expect("creates");
        let key = |e: &Edit| (e.id.clone(), e.geometry_wkb.clone());
        all.sort_by_key(key);
        streamed.sort_by_key(key);
        let show = |edits: &[Edit]| -> Vec<(String, Vec<String>, Vec<String>)> {
            edits
                .iter()
                .map(|e| {
                    (
                        e.id.clone(),
                        e.sources
                            .iter()
                            .map(|s| format!("{}/{:?}", s.spider, s.atp_ref))
                            .collect(),
                        new_tags(e),
                    )
                })
                .collect()
        };
        assert_eq!(show(&all), show(&streamed));
        for skip in [
            Skip::Kind,
            Skip::Similar,
            Skip::SpiderPosition,
            Skip::SharedPosition,
            Skip::Twin,
            Skip::Moved,
            Skip::MatchedSite,
            Skip::HostFar,
            Skip::CoarsePosition,
            Skip::Incomplete,
            Skip::Unnamed,
        ] {
            assert_eq!(all_stats.skipped(skip), stats.skipped(skip), "{skip:?}");
        }
        assert_eq!(all_stats.candidates(), stats.candidates());
        assert_eq!(all_stats.merged, stats.merged);
        streamed
    }

    #[test]
    fn streamed_creates_are_the_same() {
        // Clusters of bakeries of a few spiders and brands, some close
        // enough to merge or to be twins, at many latitudes, chained
        // north to south, across the equator and near the pole.
        let mut candidates = Vec::new();
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        let origins = [
            ZURICH,
            (8.5417, 47.37719),
            (-0.0001, 0.0),
            (0.0001, -0.0001),
            (10.0, 89.9995),
            (179.9999, -33.0),
            (-179.9999, -33.0),
        ];
        for (n, &origin) in origins.iter().enumerate() {
            for i in 0..40 {
                let p = (
                    origin.0 + (next() - 0.5) * 0.0008,
                    origin.1 + (next() - 0.5) * 0.0008,
                );
                let spider = ["bakery", "beck", "bread", "loaf"][(next() * 4.0) as usize];
                let brand = ["Q1", "Q2"][(next() * 2.0) as usize];
                let name = format!("Beck {n} {i}");
                candidates.push(candidate(
                    spider,
                    &format!("{n}-{i}"),
                    p,
                    &[
                        ("shop", "bakery"),
                        ("name", &name),
                        ("brand", "Beck"),
                        ("brand:wikidata", brand),
                    ],
                ));
            }
        }
        let edits = run_both(candidates);
        assert!(!edits.is_empty());
        assert!(edits.iter().any(|e| e.sources.len() > 1), "some merge");
    }

    fn new_tags(edit: &Edit) -> Vec<String> {
        edit.changes
            .iter()
            .map(|c| format!("{}={}", c.key, c.new.as_deref().unwrap_or("")))
            .collect()
    }

    /// About `m` meters east of `p`, at Zurich's latitude.
    fn east(p: (f64, f64), m: f64) -> (f64, f64) {
        (p.0 + m / 75_500.0, p.1)
    }

    const ZURICH: (f64, f64) = (8.5417, 47.3769);

    #[test]
    fn test_is_create_key() {
        for key in [
            "shop",
            "name",
            "name:th",
            "name:zh-Hant",
            "brand:wikidata",
            "brand:en",
            "branch",
            "operator:wikidata",
            "cuisine",
        ] {
            assert!(is_create_key(key), "{key}");
        }
        for key in [
            "ref",
            "@source_uri",
            "phone",
            "contact:email",
            "opening_hours",
            "addr:city",
            "addr:full",
            "fuel:diesel",
            "payment:visa",
            "website:th",
            "name:Bangkok",
            "image",
        ] {
            assert!(!is_create_key(key), "{key}");
        }
    }

    #[test]
    fn creates_a_missing_place() {
        let c = candidate(
            "bakery",
            "17",
            ZURICH,
            &[
                ("shop", "bakery"),
                ("name", "Beck"),
                ("ref", "17"),
                ("phone", "+41441234567"),
                ("opening_hours", "Mo-Fr 06:00-18:00"),
                ("addr:city", "ZÜRICH"),
                ("payment:cash", "yes"),
            ],
        );
        let (edits, stats) = run(vec![c], &DeletionCandidates::default());
        assert_eq!(stats.candidates(), 1);
        let [edit] = edits.as_slice() else {
            panic!("one create, got {edits:?}");
        };
        assert_eq!((edit.action, edit.osm.is_none()), ("create", true));
        assert_eq!(edit.topics, ["new_poi"]);
        assert_eq!(
            new_tags(edit),
            [
                "name=Beck",
                "opening_hours=Mo-Fr 06:00-18:00",
                "phone=+41 44 123 45 67",
                "shop=bakery"
            ]
        );
        assert!(edit.changes.iter().all(|c| c.old.is_none()));
        assert_eq!(edit.id, create_id(&edit.sources));
    }

    #[test]
    fn skips_places_that_osm_likely_has() {
        let mut near = candidate("a", "1", ZURICH, &[("shop", "bakery"), ("name", "A")]);
        near.similar_m = Some(40.0);
        let mut far = candidate(
            "b",
            "2",
            east(ZURICH, 500.0),
            &[("shop", "bakery"), ("name", "B")],
        );
        far.similar_m = Some(60.0);
        // A fuel station is a large site: 60 m is still close.
        let mut fuel = candidate(
            "c",
            "3",
            east(ZURICH, 1000.0),
            &[("amenity", "fuel"), ("name", "C")],
        );
        fuel.similar_m = Some(60.0);
        // A hotel that OSM maps as its restaurant, by name.
        let mut hotel = candidate(
            "d",
            "4",
            east(ZURICH, 1500.0),
            &[("tourism", "hotel"), ("name", "D")],
        );
        hotel.similar_name_m = Some(90.0);
        let candidates = vec![near, far, fuel, hotel];
        let (edits, stats) = run(candidates, &DeletionCandidates::default());
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].sources[0].spider, "b");
        assert_eq!(stats.skipped(Skip::Similar), 3);
    }

    #[test]
    fn skips_excluded_kinds_and_spiders() {
        let atm = candidate("bank", "1", ZURICH, &[("amenity", "atm"), ("name", "A")]);
        let stop = candidate(
            "sbb",
            "2",
            east(ZURICH, 300.0),
            &[
                ("public_transport", "platform"),
                ("amenity", "shelter"),
                ("name", "B"),
            ],
        );
        let dott = candidate(
            "gbfs",
            "3",
            east(ZURICH, 600.0),
            &[("amenity", "bicycle_rental"), ("name", "C")],
        );
        let library = candidate(
            "worldcat",
            "4",
            east(ZURICH, 900.0),
            &[("amenity", "library"), ("name", "Haringey Libraries")],
        );
        let (edits, stats) = run(
            vec![atm, stop, dott, library],
            &DeletionCandidates::default(),
        );
        assert!(edits.is_empty());
        assert_eq!(stats.skipped(Skip::Kind), 4);
    }

    #[test]
    fn skips_offices_without_address() {
        let advisor = [("office", "insurance"), ("brand", "AXA")];
        assert_eq!(
            excluded_kind(&tags(&advisor)),
            Some("office_without_address")
        );
        let agency = [
            ("office", "insurance"),
            ("brand", "AXA"),
            ("addr:street_address", "9 Avenue Victor Hugo"),
        ];
        assert_eq!(excluded_kind(&tags(&agency)), None);
        // Shops without an address may still become creates.
        assert_eq!(excluded_kind(&tags(&[("shop", "bakery")])), None);
    }

    #[test]
    fn skips_untrusted_spiders() {
        let c = candidate("bakery", "1", ZURICH, &[("shop", "bakery"), ("name", "A")]);
        let nondescript = Nondescript::none();
        let positions = Positions::default();
        let (edits, stats) = make_creates(
            vec![c],
            &CreateContext {
                nondescript: &nondescript,
                shared_osm_values: &HashSet::new(),
                opening_hours: &opening_hours::Gate::default(),
                spiders: &SpiderGate::default(),
                deletion_candidates: &DeletionCandidates::default(),
                positions: &positions,
                matched_sites: &MatchedSites::default(),
            },
        );
        assert!(edits.is_empty());
        assert_eq!(stats.skipped(Skip::SpiderPosition), 1);
    }

    #[test]
    fn skips_shared_positions() {
        let a = candidate("bakery", "1", ZURICH, &[("shop", "bakery"), ("name", "A")]);
        let b = candidate("bakery", "2", ZURICH, &[("shop", "bakery"), ("name", "B")]);
        let (edits, stats) = run(vec![a, b], &DeletionCandidates::default());
        assert!(edits.is_empty());
        assert_eq!(stats.skipped(Skip::SharedPosition), 2);
    }

    #[test]
    fn skips_parts_of_matched_sites() {
        let shop = candidate(
            "shell",
            "1-shop",
            ZURICH,
            &[("shop", "convenience"), ("name", "Shell Select")],
        );
        let mut sites = MatchedSites::default();
        sites.add("shell", "amenity=fuel".to_string(), east(ZURICH, 10.0));
        sites.add("other", "amenity=fuel".to_string(), ZURICH);
        assert!(sites.has_other_kind_near("shell", &shop.category, shop.point));
        assert!(!sites.has_other_kind_near("shell", "amenity=fuel", shop.point));
        assert!(!sites.has_other_kind_near("bp", &shop.category, shop.point));
    }

    #[test]
    fn skips_self_service_banks() {
        let t = tags(&[
            ("amenity", "bank"),
            ("name", "Berliner Sparkasse SB-Center Rheinstraße"),
        ]);
        assert!(is_self_service(&t));
        let t = tags(&[
            ("amenity", "bank"),
            ("name", "Berliner Sparkasse Filiale Rheinstraße"),
        ]);
        assert!(!is_self_service(&t));
    }

    #[test]
    fn skips_coarse_positions() {
        let c = candidate(
            "bakery",
            "1",
            (8.541, 47.377),
            &[("shop", "bakery"), ("name", "A")],
        );
        let (edits, stats) = run(vec![c], &DeletionCandidates::default());
        assert!(edits.is_empty());
        assert_eq!(stats.skipped(Skip::CoarsePosition), 1);
        assert!(is_coarse((100.938, 13.228)));
        assert!(!is_coarse((100.9381, 13.228)));
    }

    #[test]
    fn skips_places_that_moved() {
        let t = [
            ("shop", "supermarket"),
            ("name", "Coop"),
            ("brand:wikidata", "Q432564"),
        ];
        let c = candidate("coop", "1", ZURICH, &t);
        let mut deletions = DeletionCandidates::default();
        deletions.add("Q432564", east(ZURICH, 150.0));
        let (edits, stats) = run(vec![c.clone()], &deletions);
        assert!(edits.is_empty());
        assert_eq!(stats.skipped(Skip::Moved), 1);

        let mut other = DeletionCandidates::default();
        other.add("Q432564", east(ZURICH, 300.0));
        other.add("Q1", ZURICH);
        assert_eq!(run(vec![c], &other).0.len(), 1);
    }

    #[test]
    fn merges_one_dealer_of_several_brands() {
        let dealer = |spider: &str, brand: &str, id: &str, p| {
            candidate(
                spider,
                "x",
                p,
                &[
                    ("shop", "car"),
                    ("name", "Garage Muster"),
                    ("brand", brand),
                    ("brand:wikidata", id),
                    ("phone", "+41 44 765 43 21"),
                ],
            )
        };
        let (edits, stats) = run(
            vec![
                dealer("vw", "Volkswagen", "Q246", ZURICH),
                dealer("audi", "Audi", "Q23317", east(ZURICH, 10.0)),
                dealer("skoda", "Škoda", "Q29637", east(ZURICH, 20.0)),
            ],
            &DeletionCandidates::default(),
        );
        assert_eq!(edits.len(), 1, "{edits:?}");
        assert_eq!(stats.merged, 2);
        let edit = &edits[0];
        assert_eq!(edit.sources.len(), 3);
        let spiders: Vec<&str> = edit.sources.iter().map(|s| s.spider.as_str()).collect();
        assert_eq!(spiders, ["audi", "skoda", "vw"]);
        let t = new_tags(edit);
        assert!(
            t.contains(&"brand=Audi;Volkswagen;Škoda".to_string()),
            "{t:?}"
        );
        assert!(
            t.contains(&"brand:wikidata=Q23317;Q246;Q29637".to_string()),
            "{t:?}"
        );
        assert!(t.contains(&"name=Garage Muster".to_string()), "{t:?}");
    }

    #[test]
    fn does_not_merge_without_shared_contact_or_brand() {
        let a = candidate(
            "a",
            "1",
            ZURICH,
            &[("shop", "car"), ("name", "A"), ("brand", "Audi")],
        );
        let b = candidate(
            "b",
            "2",
            east(ZURICH, 10.0),
            &[("shop", "car"), ("name", "B"), ("brand", "Kia")],
        );
        // Too far apart, though of one brand.
        let c = candidate(
            "c",
            "3",
            east(ZURICH, 200.0),
            &[
                ("shop", "car"),
                ("brand", "Kia"),
                ("brand:wikidata", "Q35349"),
            ],
        );
        let d = candidate(
            "d",
            "4",
            east(ZURICH, 240.0),
            &[
                ("shop", "car"),
                ("brand", "Kia"),
                ("brand:wikidata", "Q35349"),
            ],
        );
        let (edits, stats) = run(vec![a, b, c, d], &DeletionCandidates::default());
        assert_eq!(edits.len(), 4);
        assert_eq!(stats.merged, 0);
    }

    #[test]
    fn one_spider_never_merges_two_features() {
        let t = [
            ("shop", "bakery"),
            ("brand:wikidata", "Q1"),
            ("brand", "Beck"),
        ];
        let a = candidate("beck", "1", ZURICH, &t);
        let b = candidate("beck", "2", east(ZURICH, 20.0), &t);
        // Not even through a feature of another spider that is like
        // both: one of them stays alone.
        let c = candidate("cards", "3", east(ZURICH, 10.0), &t);
        let (edits, stats) = run(vec![a, b, c], &DeletionCandidates::default());
        assert_eq!(edits.len(), 2);
        assert_eq!(stats.merged, 1);
        for edit in &edits {
            let beck = edit.sources.iter().filter(|s| s.spider == "beck").count();
            assert_eq!(beck, 1);
        }
    }

    #[test]
    fn test_merge_tags() {
        let a = tags(&[
            ("shop", "car"),
            ("name", "A"),
            ("brand", "Audi"),
            ("brand:wikidata", "Q23317"),
        ]);
        let b = tags(&[("shop", "car"), ("name", "B"), ("brand", "audi")]);
        assert_eq!(
            merge_tags(&[a.clone(), b]),
            tags(&[
                ("brand", "Audi"),
                ("brand:wikidata", "Q23317"),
                ("shop", "car")
            ])
        );
        // Lists that don't line up stay as they are from one source,
        // and go from several.
        let odd = tags(&[("brand", "A;B"), ("brand:wikidata", "Q1;Q2;Q3")]);
        assert_eq!(merge_tags(std::slice::from_ref(&odd)), odd);
        assert_eq!(
            merge_tags(&[odd, a]),
            tags(&[
                ("brand", "Audi"),
                ("brand:wikidata", "Q23317"),
                ("name", "A"),
                ("shop", "car")
            ])
        );
        // An ID alone, from a single source.
        let id = tags(&[("operator:wikidata", "Q6676271")]);
        assert_eq!(merge_tags(std::slice::from_ref(&id)), id);
    }

    fn square(center: (f64, f64), half_m: f64) -> Geometry<f64> {
        let (dx, dy) = (half_m / 75_500.0, half_m / 111_320.0);
        let (x, y) = center;
        Geometry::Polygon(Polygon::new(
            LineString::from(vec![
                (x - dx, y - dy),
                (x + dx, y - dy),
                (x + dx, y + dy),
                (x - dx, y + dy),
                (x - dx, y - dy),
            ]),
            vec![],
        ))
    }

    #[test]
    fn test_tenant_position() {
        let host = square(ZURICH, 20.0);
        let inside = east(ZURICH, 5.0);
        assert_eq!(tenant_position(inside, &host, false), Some(inside));
        // Outside: moved to 2 m inside the edge.
        let outside = east(ZURICH, 60.0);
        let moved = tenant_position(outside, &host, false).expect("a position");
        assert!(host.contains(&Point::new(moved.0, moved.1)));
        let to_edge = distance_m(moved, east(ZURICH, 20.0));
        assert!((to_edge - 2.0).abs() < 0.2, "{to_edge}");
        assert_eq!(tenant_position(east(ZURICH, 200.0), &host, false), None);
        // A coarse point only says which host.
        let center = tenant_position(inside, &host, true).expect("a position");
        assert!(distance_m(center, ZURICH) < 0.5);

        let node = Geometry::Point(Point::new(ZURICH.0, ZURICH.1));
        let near = east(ZURICH, 20.0);
        assert_eq!(tenant_position(near, &node, false), Some(near));
        let next = tenant_position(east(ZURICH, 60.0), &node, false).expect("a position");
        assert!((distance_m(next, ZURICH) - 2.0).abs() < 0.1);
        assert_eq!(tenant_position(east(ZURICH, 150.0), &node, false), None);
        let coarse = tenant_position(near, &node, true).expect("a position");
        assert!((distance_m(coarse, ZURICH) - 2.0).abs() < 0.1);
    }

    #[test]
    fn creates_tenants_inside_their_host() {
        let mut c = candidate(
            "starbucks",
            "1",
            east(ZURICH, 60.0),
            &[
                ("amenity", "cafe"),
                ("name", "Starbucks"),
                ("opening_hours", "Mo-Su 06:00-22:00"),
            ],
        );
        c.source.relation = "located_in".to_string();
        c.host = Some(HostFeature {
            osm_type: "way".to_string(),
            osm_id: 42,
            name: Some("Target".to_string()),
            opening_hours: Some("Mo-Su 08:00-22:00".to_string()),
            geometry: square(ZURICH, 20.0),
        });
        let nondescript = Nondescript::none();
        let positions = Positions::default();
        let (edits, _) = make_creates(
            vec![c],
            &CreateContext {
                nondescript: &nondescript,
                shared_osm_values: &HashSet::new(),
                opening_hours: &opening_hours::Gate::trusting(&["starbucks"]),
                spiders: &SpiderGate::trusting(&["starbucks"]),
                deletion_candidates: &DeletionCandidates::default(),
                positions: &positions,
                matched_sites: &MatchedSites::default(),
            },
        );
        let [edit] = edits.as_slice() else {
            panic!("one create, got {edits:?}");
        };
        let host = edit.host.as_ref().expect("a host");
        assert_eq!((host.osm_type.as_str(), host.osm_id), ("way", 42));
        assert_eq!(edit.warnings, ["hours_outside_host"]);
    }

    #[test]
    fn test_hours_outside_host() {
        assert!(hours_outside_host("Mo-Fr 07:00-20:00", "Mo-Sa 08:00-20:00"));
        assert!(hours_outside_host("Su 10:00-12:00", "Mo-Sa 08:00-20:00"));
        assert!(!hours_outside_host(
            "Mo-Fr 09:00-18:00",
            "Mo-Sa 08:00-20:00"
        ));
        assert!(!hours_outside_host("Mo-Fr 09:00-18:00", "24/7"));
        assert!(!hours_outside_host("bogus", "Mo-Sa 08:00-20:00"));
    }

    #[test]
    fn test_create_tags() {
        let fuel = tags(&[
            ("amenity", "fuel"),
            ("shop", "yes"),
            ("brand", "Tamoil"),
            ("brand:wikidata", "Q706793"),
            ("name", "TAMOIL  Station"),
            ("addr:city", "Bulle"),
        ]);
        assert_eq!(
            create_tags(&fuel, false),
            tags(&[
                ("amenity", "fuel"),
                ("brand", "Tamoil"),
                ("brand:wikidata", "Q706793"),
                ("name", "TAMOIL Station"),
            ])
        );
        assert_eq!(
            create_tags(&fuel, true),
            tags(&[
                ("amenity", "fuel"),
                ("brand", "Tamoil"),
                ("brand:wikidata", "Q706793"),
                ("name", "Tamoil"),
            ])
        );
        let kiosk = tags(&[
            ("shop", "convenience"),
            ("brand", "k kiosk"),
            ("brand:de", "k kiosk"),
            ("name", "kkiosk Faido EKZ MI"),
            ("name:de", "kkiosk Faido EKZ MI"),
        ]);
        assert_eq!(
            create_tags(&kiosk, true),
            tags(&[
                ("brand", "k kiosk"),
                ("brand:de", "k kiosk"),
                ("name", "k kiosk"),
                ("name:de", "k kiosk"),
                ("shop", "convenience"),
            ])
        );
        // A shop that is only `shop=yes` keeps it.
        assert_eq!(
            create_tags(&tags(&[("shop", "yes"), ("name", "A")]), false),
            tags(&[("name", "A"), ("shop", "yes")])
        );
        assert_eq!(create_category(&fuel).as_deref(), Some("amenity=fuel"));
    }

    #[test]
    fn skips_places_listed_twice() {
        let t = [
            ("amenity", "fuel"),
            ("brand", "BP"),
            ("name", "BP Klosters"),
        ];
        let a = candidate("bp", "1", ZURICH, &t);
        let b = candidate("bp", "2", east(ZURICH, 6.0), &t);
        let c = candidate("bp", "3", east(ZURICH, 500.0), &t);
        let (edits, stats) = run(vec![a, b, c], &DeletionCandidates::default());
        assert_eq!(edits.len(), 1);
        assert_eq!(stats.skipped(Skip::Twin), 2);
    }

    #[test]
    fn test_is_branch_name() {
        assert!(is_branch_name("Kiambu Road", Some("Rubis")));
        assert!(is_branch_name("博多月隈", Some("7-ELEVEN")));
        assert!(is_branch_name("天神４丁目店", None));
        assert!(!is_branch_name("Target Watertown 1442", Some("Starbucks")));
        assert!(!is_branch_name(
            "66 Main St Salisbury, Massachusetts 01952 - USA",
            Some("Burger King")
        ));
        assert!(!is_branch_name(
            "ICICI Bank Branch - Bengaluru - Bidadi",
            Some("ICICI Bank")
        ));
    }

    #[test]
    fn test_names_agree() {
        assert!(SpiderGate::names_agree(0, 19));
        assert!(SpiderGate::names_agree(3, 27));
        assert!(!SpiderGate::names_agree(0, 1391));
        assert!(!SpiderGate::names_agree(2, 27));
    }

    #[test]
    fn test_spider_gate() {
        // Bins below 1, 10, 25, 50, 100, 250, 1000 m, and beyond.
        assert!(SpiderGate::passes(
            100,
            &[10, 50, 15, 5, 10, 5, 5, 0],
            false
        ));
        assert!(!SpiderGate::passes(
            100,
            &[10, 40, 15, 5, 20, 5, 5, 0],
            false
        ));
        assert!(!SpiderGate::passes(
            100,
            &[10, 50, 15, 5, 10, 5, 5, 0],
            true
        ));
        assert!(!SpiderGate::passes(10, &[10, 0, 0, 0, 0, 0, 0, 0], false));
    }

    #[test]
    fn test_create_id() {
        let source = |spider: &str, r: Option<&str>| Source {
            spider: spider.to_string(),
            atp_ref: r.map(str::to_string),
            atp_point: Some((8.5, 47.4)),
            ..Source::default()
        };
        let a = create_id(&[source("vw", Some("1")), source("audi", Some("2"))]);
        assert_eq!(a.len(), 16);
        assert_eq!(
            a,
            create_id(&[source("audi", Some("2")), source("vw", Some("1"))])
        );
        assert_ne!(a, create_id(&[source("vw", Some("1"))]));
        assert_ne!(
            create_id(&[source("vw", None)]),
            create_id(&[source("vw", Some("1"))])
        );
    }
}
