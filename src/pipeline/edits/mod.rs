//! Turns `conflated.parquet` into `edits.parquet`: one row per proposed
//! OpenStreetMap edit, with everything needed to turn it into an
//! OSMChange element or a MapRoulette task. See
//! `docs/notes/2026-11-23-edits-parquet.md` for the plan and
//! `docs/outputs/EDITS_PARQUET.md` for the format.
//!
//! So far, the step proposes modifies of OSM features that AllThePlaces
//! matched: contact tags ([contact]), the Wikidata IDs of brands,
//! operators and networks ([wikidata]), opening hours
//! ([opening_hours]), and the service tags of postal partners
//! ([post_partner]).
//!
//! It reads `conflated.parquet` twice, and keeps in memory only the
//! statistics over all rows that the rules need, not the rows
//! themselves, so that the planet fits (#1059):
//!
//! 1. The first pass spills the rows that matched OSM features, and
//!    the ATP features that may become creates, to disk ([spill]), and
//!    collects the positions and sites that the create rules check.
//! 2. The matched rows come back sorted by OSM feature, so the rows of
//!    one feature are next to each other; they are grouped into a
//!    spool file. From it, the contact values that several features
//!    share are counted on disk, and then each feature is decided.
//! 3. The create candidates come back sorted by latitude, and are
//!    decided in clusters of nearby candidates; see
//!    [create::make_creates_sorted].
//! 4. The second pass picks up the full OSM side of the features that
//!    get an edit, from the first row that matched each, in the order
//!    of the rows.
//! 5. All edits come back sorted as in `edits.parquet`, and are written
//!    a row group at a time.
//!
//! Memory grows with the statistics (positions, matched sites,
//! deletion candidates, per-spider counts), not with the number of
//! matched rows or edits.

mod agreement;
mod contact;
pub(crate) mod create;
mod opening_hours;
mod post_partner;
mod read;
mod spill;
mod wikidata;
mod writer;

pub(crate) use writer::DISCLAIMER;

use crate::make_progress_bar;
use crate::matchers::brand_names::NsiNames;
use crate::matchers::contact::Nondescript;
use anyhow::{Context, Result};
use indicatif::MultiProgress;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Tags as key-value pairs.
type Tags = Vec<(String, String)>;

/// Why a tag changes; written as `changes.reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Reason {
    /// OSM has no such tag.
    MissingInOsm,
    /// OSM's value says something else.
    Differs,
    /// OSM's value is generic, such as a brand's home page, and ours
    /// is the page of this one place.
    GenericValue,
    /// OSM lists several values; ours is added to them.
    AddedValue,
    /// OSM's Wikidata IDs don't line up with its names.
    Misaligned,
    /// The edit only touches Wikidata IDs.
    WikidataOnly,
    /// Opening hours on the same days, differing by at most an hour a
    /// day.
    Small,
    /// Opening hours on the same days, differing by more than an hour
    /// on some day.
    Over60Min,
    /// Open on some day in one value and closed in the other.
    OtherDays,
    /// The same week, but other hours on public holidays.
    PublicHolidays,
    /// OSM's value has rules we don't edit by machine, such as
    /// seasons or comments; `new` is the same as `old`, and `atp` says
    /// what AllThePlaces has, for a person to decide.
    NeedsReview,
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reason::MissingInOsm => "missing_in_osm",
            Reason::Differs => "differs",
            Reason::GenericValue => "generic_value",
            Reason::AddedValue => "added_value",
            Reason::Misaligned => "misaligned",
            Reason::WikidataOnly => "wikidata_only",
            Reason::Small => "small",
            Reason::Over60Min => "over_60_min",
            Reason::OtherDays => "other_days",
            Reason::PublicHolidays => "public_holidays",
            Reason::NeedsReview => "needs_review",
        }
    }
}

/// One tag change: `old` is OSM's value, `None` if OSM has no such
/// tag; `new` is the value to set, `None` to remove the tag. `atp` is
/// AllThePlaces' value as written, for changes that a person must
/// decide ([Reason::NeedsReview]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Change {
    pub key: String,
    pub old: Option<String>,
    pub new: Option<String>,
    pub reason: Reason,
    pub atp: Option<String>,
}

impl Change {
    fn new(key: &str, old: Option<&str>, new: Option<String>, reason: Reason) -> Change {
        Change {
            key: key.to_string(),
            old: old.map(str::to_string),
            new,
            reason,
            atp: None,
        }
    }
}

/// One `conflated.parquet` row that feeds an edit.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Source {
    pub spider: String,
    /// `atp.fetched.timestamp`, in Unix milliseconds.
    pub fetched_millis: i64,
    /// ATP's `ref`, the spider's own ID for the feature.
    pub atp_ref: Option<String>,
    /// A page a human can check: ATP's `website` or `contact:website`,
    /// else `atp.fetched.source_uri`.
    pub source_url: Option<String>,
    pub relation: String,
    pub rule: Option<String>,
    pub score: Option<f32>,
    pub distance_m: Option<f32>,
    pub importance: f32,
    /// ATP's representative point, as (longitude, latitude).
    pub atp_point: Option<(f64, f64)>,
    /// The ATP tags that the rules read; see [read::is_rule_key].
    pub tags: Tags,
    /// For a postal partner, the `post_office` tags it would set on
    /// the OSM feature; see [post_partner::has_partner_edits].
    pub partner_tags: Option<Tags>,
}

impl Source {
    fn order_key(&self) -> (&str, Option<&str>, i64) {
        (&self.spider, self.atp_ref.as_deref(), self.fetched_millis)
    }
}

/// An OSM feature that AllThePlaces matched, as the first pass sees it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Element {
    /// The OSM tags that the rules read; see [read::is_rule_key].
    pub tags: Tags,
    /// Where the feature lies, as (longitude, latitude).
    pub position: Option<(f64, f64)>,
    pub sources: Vec<Source>,
}

/// What the rules decided for one OSM feature.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Decision {
    pub changes: Vec<Change>,
    #[serde(deserialize_with = "spill::static_strs")]
    pub topics: Vec<&'static str>,
    pub warnings: Vec<String>,
}

/// What the rules need besides the feature itself.
pub(crate) struct RuleContext<'a> {
    pub nondescript: &'a Nondescript,
    pub nsi: &'a NsiNames,
    /// Contact values that several OSM features carry, and words that
    /// recur in AllThePlaces email addresses; see
    /// [contact::shared_osm_values] and [contact::shared_email_words].
    pub shared_osm_values: &'a HashSet<String>,
    /// The spiders whose opening hours we trust.
    pub opening_hours: &'a opening_hours::Gate,
}

/// Decides the edit for one OSM feature, or `None` if there is
/// nothing to change. Sources are taken in a fixed order (spider, then
/// ATP ref), so the same input always gives the same edit.
pub(crate) fn decide(element: &mut Element, ctx: &RuleContext) -> Option<Decision> {
    element
        .sources
        .sort_by(|a, b| a.order_key().cmp(&b.order_key()));
    let osm = &element.tags;
    let mut decision = Decision::default();

    // A business matched as `same`: contact tags and Wikidata IDs.
    let business: Vec<&Source> = element
        .sources
        .iter()
        .filter(|s| s.partner_tags.is_none() && s.relation == "same")
        .collect();
    // A registry's page about a place isn't the place's website; see
    // [contact::heritage_changes].
    let mut registry_pages: Vec<&str> = Vec::new();
    let is_page = |k: &str| k == "website" || k == "contact:website";
    let registry_tags: Vec<Tags> = business
        .iter()
        .filter(|s| contact::is_registry(&s.spider))
        .map(|s| {
            if let Some((_, v)) = s.tags.iter().find(|(k, _)| is_page(k)) {
                registry_pages.push(v.as_str());
            }
            s.tags
                .iter()
                .filter(|(k, _)| !is_page(k))
                .cloned()
                .collect()
        })
        .collect();
    let contacts: Vec<contact::ContactSource> = business
        .iter()
        .filter(|s| !contact::is_registry(&s.spider))
        .map(|s| contact::ContactSource { tags: &s.tags })
        .chain(
            registry_tags
                .iter()
                .map(|tags| contact::ContactSource { tags }),
        )
        .collect();
    let changes = contact::contact_changes(
        osm,
        &contacts,
        ctx.nondescript,
        ctx.shared_osm_values,
        element.position,
        &mut decision.warnings,
    );
    let mut changes = changes;
    changes.extend(contact::heritage_changes(
        osm,
        &registry_pages,
        ctx.shared_osm_values,
        &mut decision.warnings,
    ));
    if !changes.is_empty() {
        decision.topics.push("contact");
        decision.changes.extend(changes);
    }
    let business_tags: Vec<&Tags> = business.iter().map(|s| &s.tags).collect();
    let changes = wikidata::wikidata_changes(
        osm,
        &business_tags,
        ctx.nsi,
        element.position,
        &mut decision.warnings,
    );
    if !changes.is_empty() {
        decision.topics.push("wikidata");
        decision.changes.extend(changes);
    }
    let hours_sources: Vec<(&str, &Tags)> = business
        .iter()
        .map(|s| (s.spider.as_str(), &s.tags))
        .collect();
    let changes = opening_hours::opening_hours_changes(
        osm,
        &hours_sources,
        ctx.opening_hours,
        &mut decision.warnings,
    );
    if !changes.is_empty() {
        decision.topics.push("opening_hours");
        decision.changes.extend(changes);
    }

    // A postal partner, in its host or on its own: service tags only.
    let mut partner: Option<Tags> = None;
    for tags in element
        .sources
        .iter()
        .filter_map(|s| s.partner_tags.clone())
    {
        match &mut partner {
            None => partner = Some(tags),
            Some(merged) => post_partner::merge_edits(merged, tags),
        }
    }
    let changes = post_partner::partner_changes(osm, partner.unwrap_or_default());
    if !changes.is_empty() {
        decision.topics.push("post_partner");
        decision.changes.extend(changes);
    }

    if decision.changes.is_empty() {
        return None;
    }
    if decision
        .changes
        .iter()
        .all(|c| c.key.ends_with(":wikidata"))
    {
        for change in &mut decision.changes {
            change.reason = Reason::WikidataOnly;
        }
    }
    decision.changes.sort_by(|a, b| a.key.cmp(&b.key));
    decision.warnings.sort();
    decision.warnings.dedup();
    Some(decision)
}

/// The ID of a modify: stable across weeks while the same keys of the
/// same feature change, so that a MapRoulette task keeps its status.
pub(crate) fn modify_id(osm_type: &str, osm_id: u64, changes: &[Change]) -> String {
    let mut keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
    keys.sort_unstable();
    let text = format!("modify/{osm_type}/{osm_id}/{}", keys.join(";"));
    crate::utils::sha256_hex(text.as_bytes())[..16].to_string()
}

/// Bytes that the spills of `make_edits` buffer in all, at any time;
/// see [spill::Spill]. A quarter of what other steps give a single
/// external sort ([super::EXTERNAL_SORT_CHUNK_BYTES]), since a spill
/// buffers encoded items, which take less than decoded ones.
const SPILL_BYTES: usize = super::EXTERNAL_SORT_CHUNK_BYTES / 4;

/// How many OSM features are decided at once, in parallel.
const DECIDE_CHUNK: usize = 16_384;

/// Logs the memory in use, after a part of `make_edits`.
fn log_memory(message: &'static str) {
    let stats = super::memstats::snapshot();
    log::info!(
        rss_bytes = stats.rss_bytes,
        rss_peak_bytes = stats.rss_peak_bytes,
        rss_anon_bytes = stats.rss_anon_bytes,
        rss_file_bytes = stats.rss_file_bytes,
        cgroup_current_bytes = stats.cgroup_current_bytes,
        cgroup_peak_bytes = stats.cgroup_peak_bytes;
        "{message}"
    );
}

/// The values that the rules take for generic because several places
/// share them: the contact values that more than two of the matched OSM
/// features carry, and the words of email addresses that recur in the
/// ATP rows that matched them; see [contact::shared_osm_keys] and
/// [contact::email_addresses]. Counted on disk, in `scratch`, so
/// that memory doesn't grow with the number of distinct values.
fn shared_values(
    grouped: &crate::tables::RecordReader,
    workdir: &Path,
    scratch: &Path,
) -> Result<HashSet<String>> {
    use crate::tables::StringCounts;
    let budget = SPILL_BYTES;
    // StringCounts takes plain items, so a read error stops the items
    // here and comes out after the count.
    let error: std::cell::RefCell<Option<anyhow::Error>> = Default::default();
    let elements = || -> Result<_> {
        Ok(read::grouped_elements(grouped)?.map_while(|e| match e {
            Ok(e) => Some(e),
            Err(e) => {
                error.borrow_mut().get_or_insert(e);
                None
            }
        }))
    };
    let check = || match error.borrow_mut().take() {
        Some(e) => Err(e.context("could not read the matched features")),
        None => Ok(()),
    };
    let mut shared = HashSet::new();
    let counts = StringCounts::create(
        elements()?.flat_map(|g| {
            contact::shared_osm_keys(&g.element.tags)
                .into_iter()
                .map(|k| (k, 1))
        }),
        workdir,
        &scratch.join("osm-values"),
        budget,
    )?;
    check()?;
    shared.extend(
        counts
            .iter()
            .filter(|(_, n)| contact::is_shared_count(*n))
            .map(|(k, _)| k.to_string()),
    );
    drop(counts);
    let emails = StringCounts::create(
        elements()?.flat_map(|g| {
            g.element
                .sources
                .into_iter()
                .filter(|s| s.partner_tags.is_none())
                .flat_map(|s| contact::email_addresses(&s.tags))
                .map(|e| (e, 1))
                .collect::<Vec<_>>()
        }),
        workdir,
        &scratch.join("emails"),
        budget,
    )?;
    check()?;
    let words = StringCounts::create(
        emails
            .iter()
            .flat_map(|(email, _)| contact::email_word_keys(email).into_iter().map(|k| (k, 1))),
        workdir,
        &scratch.join("email-words"),
        budget,
    )?;
    shared.extend(
        words
            .iter()
            .filter(|(_, n)| contact::is_shared_count(*n))
            .map(|(k, _)| k.to_string()),
    );
    Ok(shared)
}

/// The files `make_edits` reads besides `conflated.parquet`.
fn inputs(workdir: &Path) -> Vec<PathBuf> {
    let mut paths = super::atp::nondescript::paths(workdir);
    let nsi = super::nsi::path(workdir);
    if nsi.exists() {
        paths.push(nsi);
    }
    paths
}

/// Builds `edits.parquet` in `workdir` from `conflated`, leaving out
/// the rows of `unhealthy` spiders (see
/// [super::spider_churn::unhealthy_spiders], which reads
/// `spider_stats`). `provenance_bom` goes
/// into the file's metadata.
pub fn make_edits(
    conflated: &Path,
    spider_stats: &Path,
    unhealthy: &BTreeSet<String>,
    provenance_bom: &str,
    progress: &MultiProgress,
    workdir: &Path,
) -> Result<PathBuf> {
    let out = workdir.join("edits.parquet");
    let sidecar = super::spider_creates::sidecar_path(workdir);
    let extra = inputs(workdir);
    let mut all_inputs: Vec<&Path> = vec![conflated, spider_stats];
    all_inputs.extend(extra.iter().map(PathBuf::as_path));
    if super::is_current(&[&out, &sidecar], &all_inputs)? {
        return Ok(out);
    }
    let start = Instant::now();
    let paths = super::atp::nondescript::paths(workdir);
    let nondescript = Nondescript::open(&paths[0], &paths[1], &paths[2])?;
    let nsi_path = super::nsi::path(workdir);
    let nsi = if nsi_path.exists() {
        NsiNames::open(&nsi_path)?
    } else {
        NsiNames::default()
    };
    let bar = make_progress_bar(progress, "edits.collect", 0, "rows");
    let read::FirstPass {
        modifies,
        candidates,
        deletion_candidates,
        positions,
        matched_sites,
        early_skips,
    } = read::first_pass(conflated, unhealthy, workdir, &bar)?;
    bar.finish();
    log_memory("make_edits: first pass");
    let scratch = tempfile::Builder::new()
        .prefix("edits-")
        .tempdir_in(workdir)
        .context("could not create a scratch directory")?;
    let num_rows = modifies.len();
    let grouped = read::group_elements(modifies, &scratch.path().join("elements"))?;
    let num_elements = grouped.len();
    let shared_osm_values = shared_values(&grouped, workdir, scratch.path())?;
    log_memory("make_edits: shared values");
    let hours_gate = opening_hours::Gate::open(spider_stats)?;
    let ctx = RuleContext {
        nondescript: &nondescript,
        nsi: &nsi,
        shared_osm_values: &shared_osm_values,
        opening_hours: &hours_gate,
    };

    // The edits, sorted as in the file; see [read::EditKey].
    let mut edits: spill::Spill<read::EditKey, writer::Edit> =
        spill::Spill::new(workdir, "edits", SPILL_BYTES / 2)?;
    let mut decided: spill::Spill<u64, read::DecidedElement> =
        spill::Spill::new(workdir, "decided", SPILL_BYTES / 2)?;
    let mut elements = read::grouped_elements(&grouped)?.peekable();
    while elements.peek().is_some() {
        let chunk: Vec<read::GroupedElement> = elements
            .by_ref()
            .take(DECIDE_CHUNK)
            .collect::<Result<_>>()?;
        let results: Vec<(u64, read::DecidedElement)> = chunk
            .into_par_iter()
            .filter_map(|mut g| {
                let decision = decide(&mut g.element, &ctx)?;
                Some((
                    g.first_row,
                    read::DecidedElement {
                        position: g.element.position,
                        sources: g.element.sources,
                        decision,
                    },
                ))
            })
            .collect();
        for (row, element) in results {
            decided.push(row, &element)?;
        }
    }
    drop(grouped);
    let num_decided = decided.len();
    log_memory("make_edits: decisions");

    let spider_gate = create::SpiderGate::open(spider_stats)?;
    let mut num_creates = 0u64;
    let mut create_stats = create::make_creates_sorted(
        candidates
            .sorted()?
            .map(|c| c.map(|((_, row), candidate)| (row, candidate))),
        &create::CreateContext {
            nondescript: &nondescript,
            shared_osm_values: &shared_osm_values,
            opening_hours: &hours_gate,
            spiders: &spider_gate,
            deletion_candidates: &deletion_candidates,
            positions: &positions,
            matched_sites: &matched_sites,
        },
        |edit| {
            // After every modify, whose sequence numbers are rows.
            let seq = u64::MAX / 2 + num_creates;
            num_creates += 1;
            edits.push(read::edit_key(&edit, seq), &edit)
        },
    )?;
    create_stats.add(&early_skips);
    log_memory("make_edits: creates");
    let skipped = |skip: create::Skip| create_stats.skipped(skip);
    log::info!(
        candidates = create_stats.candidates(),
        creates = num_creates,
        merged = create_stats.merged,
        deletion_candidates = deletion_candidates.len(),
        skipped_kind = skipped(create::Skip::Kind),
        skipped_similar_nearby = skipped(create::Skip::Similar),
        skipped_spider_position = skipped(create::Skip::SpiderPosition),
        skipped_shared_position = skipped(create::Skip::SharedPosition),
        skipped_twin = skipped(create::Skip::Twin),
        skipped_moved = skipped(create::Skip::Moved),
        skipped_matched_site = skipped(create::Skip::MatchedSite),
        skipped_host_far = skipped(create::Skip::HostFar),
        skipped_coarse_position = skipped(create::Skip::CoarsePosition),
        skipped_incomplete = skipped(create::Skip::Incomplete),
        skipped_unnamed = skipped(create::Skip::Unnamed);
        "make_edits: creates"
    );
    super::spider_creates::write_sidecar(&create_stats, &sidecar)
        .context("could not write per-spider create counts")?;
    drop((deletion_candidates, positions, matched_sites));

    let bar = make_progress_bar(progress, "edits.write", 0, "rows");
    read::complete_edits(conflated, decided.sorted()?, &bar, |row, edit| {
        edits.push(read::edit_key(&edit, row), &edit)
    })?;
    bar.finish();
    log_memory("make_edits: modifies");
    let stats = writer::write(
        &out,
        edits.sorted()?.map(|e| e.map(|(_, edit)| edit)),
        provenance_bom,
    )
    .context("could not write edits")?;
    drop(scratch);
    log::info!(
        elapsed_seconds = start.elapsed().as_secs_f64(),
        matched_rows = num_rows,
        matched_osm_features = num_elements,
        modifies = num_decided,
        edits = stats.edits,
        changes = stats.changes,
        contact = stats.topic("contact"),
        wikidata = stats.topic("wikidata"),
        opening_hours = stats.topic("opening_hours"),
        post_partner = stats.topic("post_partner"),
        new_poi = stats.topic("new_poi"),
        bytes_written = std::fs::metadata(&out)?.len();
        "make_edits: done"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(t: &[(&str, &str)]) -> Tags {
        t.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn source(spider: &str, relation: &str, t: &[(&str, &str)]) -> Source {
        Source {
            spider: spider.to_string(),
            relation: relation.to_string(),
            tags: tags(t),
            ..Source::default()
        }
    }

    fn ctx_decide(element: &mut Element) -> Option<Decision> {
        let nondescript = Nondescript::none();
        let nsi = NsiNames::default();
        decide(
            element,
            &RuleContext {
                nondescript: &nondescript,
                nsi: &nsi,
                shared_osm_values: &HashSet::new(),
                opening_hours: &opening_hours::Gate::trusting(&["valora"]),
            },
        )
    }

    #[test]
    fn combines_topics_into_one_edit() {
        let mut element = Element {
            tags: tags(&[("shop", "kiosk"), ("brand", "Valora")]),
            sources: vec![
                source(
                    "valora",
                    "same",
                    &[
                        ("phone", "+41 44 123 45 67"),
                        ("brand", "Valora"),
                        ("brand:wikidata", "Q685551"),
                    ],
                ),
                Source {
                    partner_tags: Some(tags(&[("post_office", "post_partner")])),
                    ..source("dhl", "located_in", &[])
                },
            ],
            ..Element::default()
        };
        let decision = ctx_decide(&mut element).expect("an edit");
        assert_eq!(decision.topics, ["contact", "wikidata", "post_partner"]);
        let keys: Vec<&str> = decision.changes.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["brand:wikidata", "phone", "post_office"]);
        assert_eq!(decision.changes[0].reason, Reason::MissingInOsm);
    }

    #[test]
    fn id_only_edits_say_so() {
        let mut element = Element {
            tags: tags(&[("brand", "Valora")]),
            sources: vec![source(
                "valora",
                "same",
                &[("brand", "Valora"), ("brand:wikidata", "Q685551")],
            )],
            ..Element::default()
        };
        let decision = ctx_decide(&mut element).expect("an edit");
        assert_eq!(decision.changes[0].reason, Reason::WikidataOnly);
    }

    #[test]
    fn partners_never_edit_contacts() {
        // A partner matched as `same` only edits its service tags.
        let mut element = Element {
            tags: tags(&[("shop", "kiosk"), ("post_office", "post_partner")]),
            sources: vec![Source {
                partner_tags: Some(Vec::new()),
                ..source("dhl", "same", &[("phone", "+49 30 1234567")])
            }],
            ..Element::default()
        };
        assert_eq!(ctx_decide(&mut element), None);
    }

    #[test]
    fn decisions_do_not_depend_on_source_order() {
        let a = source("a", "same", &[("phone", "+41 44 765 43 21")]);
        let b = source("b", "same", &[("email", "b@example.ch")]);
        let mut one = Element {
            sources: vec![a.clone(), b.clone()],
            ..Element::default()
        };
        let mut two = Element {
            sources: vec![b, a],
            ..Element::default()
        };
        assert_eq!(ctx_decide(&mut one), ctx_decide(&mut two));
    }

    #[test]
    fn test_modify_id() {
        let change = |key: &str| Change::new(key, None, Some("x".into()), Reason::MissingInOsm);
        let id = modify_id("node", 42, &[change("phone"), change("email")]);
        assert_eq!(id.len(), 16);
        assert_eq!(
            id,
            modify_id("node", 42, &[change("email"), change("phone")])
        );
        assert_ne!(id, modify_id("node", 42, &[change("email")]));
        assert_ne!(
            id,
            modify_id("way", 42, &[change("email"), change("phone")])
        );
    }
}
