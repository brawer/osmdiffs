//! Proposed OpenStreetMap edits, from `conflated.parquet`; see
//! `docs/notes/2026-11-23-edits-parquet.md` for the plan. For now this
//! only holds the rules that the old `suggest_edits` step had and the
//! new `edits` step will use again.

mod agreement;
mod post_partner;
