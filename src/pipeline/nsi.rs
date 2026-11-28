//! The [name-suggestion-index](https://nsi.guide) (NSI): its
//! `dist/json/nsi.min.json`, which lists the names of brands, operators
//! and networks with their Wikidata IDs and where they apply. Matching
//! uses it to find a brand in the `name` of an OpenStreetMap feature;
//! see [crate::matchers::brand_names].
//!
//! NSI publishes to npm about weekly; [super::npm] fetches the latest.

use super::npm::NpmFile;
use std::path::{Path, PathBuf};

/// The file we fetch.
pub(crate) const NSI: NpmFile = NpmFile {
    package: "name-suggestion-index",
    file_in_package: "/dist/json/nsi.min.json",
    filename: "nsi.json",
    meta_filename: "nsi.meta.json",
    label: "nsi.fetch     ",
};

/// NSI's license, as an SPDX ID; for the provenance BOM.
pub(crate) const LICENSE: &str = "BSD-3-Clause";

/// NSI's license text.
pub(crate) const LICENSE_URL: &str =
    "https://github.com/osmlab/name-suggestion-index/blob/main/LICENSE.md";

/// The base name of the BOM component, for [crate::pipeline::provenance].
pub(crate) const BOM_REF: &str = NSI.filename;

/// Where the pipeline keeps NSI in `workdir`.
pub(crate) fn path(workdir: &Path) -> PathBuf {
    NSI.path(workdir)
}
