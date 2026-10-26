use crate::{TileLayer, make_progress_bar};
use anyhow::{Ok, Result};
use indicatif::MultiProgress;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How [`render_tiles`] should choose which zoom levels to build.
pub enum ZoomRange {
    /// `-zg --extend-zooms-if-still-dropping`: let tippecanoe pick a
    /// maximum zoom automatically, extending further if features are
    /// still being dropped once it gets there. Safe for ordinary
    /// point/line/polygon data, where zooming in genuinely separates
    /// crowded features from each other -- but see
    /// `conflated_tiles::DETAIL_MAX_ZOOM`'s doc comment for a
    /// real case where that assumption doesn't hold, and this never
    /// terminates in practice.
    Auto,
    /// `-Z<min> -z<max>`: an explicit, non-negotiable zoom range, no
    /// auto-detection or extension. Use this for any input that might
    /// contain the kind of degenerate geometry `Auto`'s doc comment
    /// warns about.
    Bounded { min: u8, max: u8 },
}

/// Share of features, by `importance` (see `pipeline::importance`),
/// that tiles show at z0. Each zoom level above that shows four times
/// as many (a tile splits into four, so features per tile stay about
/// the same), until every feature is shown from [`ALL_FEATURES_ZOOM`]
/// on. On a planet of ~4M conflated rows, 1% is ~40k features, about
/// what fits into z0 today.
const TOP_SHARE_AT_Z0: f64 = 0.01;

/// From this zoom level on, tiles show features of any importance.
const ALL_FEATURES_ZOOM: u8 = 4;

/// Rounds `importance` to two decimals for writing it into tiles, so a
/// tile holds at most 101 distinct values of it.
pub(crate) fn round_importance(importance: f32) -> f64 {
    (f64::from(importance) * 100.0).round() / 100.0
}

/// The lowest (rounded) importance that tiles show at `zoom`: 0.99 at
/// z0, 0.96 at z1, 0.84 at z2, 0.36 at z3.
fn min_importance_at_zoom(zoom: u8) -> f64 {
    let share = TOP_SHARE_AT_Z0 * 4f64.powi(i32::from(zoom));
    ((1.0 - share) * 100.0).round() / 100.0
}

/// A tippecanoe feature filter (`-j`) so that coarse zooms show the
/// most important places, rather than whatever survives density-based
/// dropping ([#713](https://github.com/brawer/osmdiffs/issues/713)).
/// `attr` names the feature property holding the rounded importance.
///
/// A filter rather than a per-feature `"tippecanoe": {"minzoom": N}`,
/// because with tippecanoe v2.49 the latter made it drop nearly all
/// features at every zoom (2 of 1,436 at z0 in a Swiss test run).
fn importance_filter(attr: &str) -> String {
    let mut any = vec![
        serde_json::json!("any"),
        serde_json::json!([">=", "$zoom", ALL_FEATURES_ZOOM]),
    ];
    for zoom in 0..ALL_FEATURES_ZOOM {
        any.push(serde_json::json!([
            "all",
            [">=", "$zoom", zoom],
            [">=", attr, min_importance_at_zoom(zoom)]
        ]));
    }
    serde_json::json!({ "*": any }).to_string()
}

pub fn render_tiles(
    layers: &[TileLayer],
    progress: &MultiProgress,
    workdir: &Path,
    out_filename: &str,
    zoom_range: ZoomRange,
    importance_attr: Option<&str>,
) -> Result<PathBuf> {
    let out_path = workdir.join(out_filename);
    if out_path.exists() {
        return Ok(out_path);
    }

    // Write tiles to a temporary file, which we’ll rename to the
    // final filename in an atomic operation.  However, the temporary
    // file needs to have a suffix of `.pmtiles`; otherwise, tippecanoe
    // will produce an SQLite database with MapBox Vector Tiles.
    let mut tmp_path = PathBuf::from(&out_path);
    tmp_path.add_extension("tmp.pmtiles");

    let progress_bar = make_progress_bar(progress, "tiles.render", 100, "percent");

    let mut cmd = make_tippecanoe_command(layers, workdir, &tmp_path, zoom_range, importance_attr)?;
    let mut child = cmd.stderr(Stdio::piped()).spawn()?;
    let stderr = child.stderr.take().expect("Failed to capture stderr");
    let stderr_reader = BufReader::new(stderr);
    for line in stderr_reader.lines() {
        let line = line?;
        if let Some(progress) = parse_progress(&line) {
            progress_bar.set_position((progress + 0.5) as u64);
        } else {
            log::info!("tippecanoe: {}", line);
        }
    }

    if !child.wait()?.success() {
        anyhow::bail!("tippecanoe failed");
    }

    std::fs::rename(&tmp_path, &out_path)?;
    progress_bar.finish();
    Ok(out_path)
}

fn make_tippecanoe_command(
    layers: &[TileLayer],
    workdir: &Path,
    out_path: &Path,
    zoom_range: ZoomRange,
    importance_attr: Option<&str>,
) -> Result<Command> {
    let mut cmd = Command::new("tippecanoe");
    // Clear all environment variables, so we don't leak secrets to an untrusted subprocess.
    cmd.env_clear()
        .env("PATH", env!("PATH"))
        .arg("--json-progress")
        .arg("--read-parallel")
        .arg("--force")
        .arg("--temporary-directory")
        .arg(std::path::absolute(workdir)?);
    match zoom_range {
        ZoomRange::Auto => {
            cmd.arg("-zg").arg("--extend-zooms-if-still-dropping");
        }
        ZoomRange::Bounded { min, max } => {
            cmd.arg(format!("-Z{min}")).arg(format!("-z{max}"));
        }
    }
    match importance_attr {
        // Importance decides what coarse zooms show, so no additional
        // random dot-dropping; `--drop-densest-as-needed` still thins
        // out any tile that would otherwise be too large.
        Some(attr) => {
            cmd.arg("-r")
                .arg("1")
                .arg("-j")
                .arg(importance_filter(attr));
        }
        None => {
            cmd.arg("-r").arg("1.2");
        }
    }
    cmd.arg("--drop-densest-as-needed");
    for layer in layers.iter() {
        // Suppress empty layers; Tippecanoe fails if any input file is empty.
        let metadata = std::fs::metadata(&layer.path)?;
        if metadata.len() > 0 {
            cmd.arg(format!(
                "--named-layer={}:{}",
                layer.name,
                layer.path.display()
            ));
        } else {
            log::info!("dropping empty layer {:?}", layer);
        }
    }
    cmd.arg("--output").arg(out_path);
    Ok(cmd)
}

/// Merges PMTiles archives built at disjoint zoom ranges into one,
/// via `tile-join` (a sibling binary tippecanoe's own build already
/// produces -- see `Containerfile`). Used to combine
/// `conflated.pmtiles`' coarse overview pass (built with
/// `ZoomRange::Bounded { min: 0, max: DETAIL_MIN_ZOOM - 1 }`) and its
/// high-zoom detail pass (`Bounded { min: DETAIL_MIN_ZOOM, max:
/// DETAIL_MAX_ZOOM }`) into one file spanning the whole range -- see
/// `pipeline::conflated_tiles`' module doc comment for why that split
/// exists. `tile-join` itself warns (not an error) about the two
/// inputs' differing max zoom; that's expected here, not a sign
/// something went wrong.
pub fn join_tiles(inputs: &[PathBuf], workdir: &Path, out_filename: &str) -> Result<PathBuf> {
    let out_path = workdir.join(out_filename);
    if out_path.exists() {
        return Ok(out_path);
    }

    let mut tmp_path = PathBuf::from(&out_path);
    tmp_path.add_extension("tmp.pmtiles");

    let mut cmd = make_tile_join_command(inputs, &tmp_path);
    let mut child = cmd.stderr(Stdio::piped()).spawn()?;
    let stderr = child.stderr.take().expect("Failed to capture stderr");
    for line in BufReader::new(stderr).lines() {
        log::info!("tile-join: {}", line?);
    }

    if !child.wait()?.success() {
        anyhow::bail!("tile-join failed");
    }

    std::fs::rename(&tmp_path, &out_path)?;
    Ok(out_path)
}

fn make_tile_join_command(inputs: &[PathBuf], out_path: &Path) -> Command {
    let mut cmd = Command::new("tile-join");
    // Clear all environment variables, so we don't leak secrets to an untrusted subprocess.
    cmd.env_clear().env("PATH", env!("PATH")).arg("--force");
    for input in inputs {
        cmd.arg(input);
    }
    cmd.arg("--output").arg(out_path);
    cmd
}

/// Structure of the JSON record that tippecanoe writes to stderr
/// when it gets invoked with --json-progress.
#[derive(Deserialize)]
struct Progress {
    progress: f64,
}

fn parse_progress(line: &str) -> Option<f64> {
    let p: Progress = serde_json::from_str(line).ok()?;
    if p.progress >= 0.0 && p.progress <= 100.0 {
        Some(p.progress)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_importance() {
        assert_eq!(round_importance(0.98765), 0.99);
        assert_eq!(round_importance(0.4567), 0.46);
        assert_eq!(round_importance(0.0), 0.0);
        assert_eq!(round_importance(1.0), 1.0);
    }

    #[test]
    fn test_min_importance_at_zoom() {
        let got: Vec<f64> = (0..ALL_FEATURES_ZOOM).map(min_importance_at_zoom).collect();
        assert_eq!(got, [0.99, 0.96, 0.84, 0.36]);
    }

    #[test]
    fn test_importance_filter() {
        let filter: serde_json::Value =
            serde_json::from_str(&importance_filter("imp")).expect("valid JSON");
        assert_eq!(
            filter,
            serde_json::json!({"*": [
                "any",
                [">=", "$zoom", 4],
                ["all", [">=", "$zoom", 0], [">=", "imp", 0.99]],
                ["all", [">=", "$zoom", 1], [">=", "imp", 0.96]],
                ["all", [">=", "$zoom", 2], [">=", "imp", 0.84]],
                ["all", [">=", "$zoom", 3], [">=", "imp", 0.36]],
            ]})
        );
    }

    #[test]
    fn test_make_tippecanoe_command_importance_filter() {
        let layers = [make_test_tile_layer("Shops", "diffed-shops.jsonl")];
        let cmd = make_tippecanoe_command(
            &layers,
            &PathBuf::from("test-workdir"),
            &PathBuf::from("test-output.pmtiles"),
            ZoomRange::Auto,
            Some("@importance"),
        )
        .expect("should not fail");
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let r = args.iter().position(|a| a == "-r").expect("-r");
        assert_eq!(args[r + 1], "1");
        let j = args.iter().position(|a| a == "-j").expect("-j");
        assert_eq!(args[j + 1], importance_filter("@importance"));
        assert!(args.iter().any(|a| a == "--drop-densest-as-needed"));
    }

    #[test]
    fn test_make_tippecanoe_command_auto_zoom() {
        let layers = [
            // Empty input file, expected to be suppressed from arguments.
            make_test_tile_layer("Empty", "empty.jsonl"),
            make_test_tile_layer("Shops", "diffed-shops.jsonl"),
        ];
        let workdir = PathBuf::from("test-workdir");
        let out = PathBuf::from("test-output.pmtiles");
        let cmd = make_tippecanoe_command(&layers, &workdir, &out, ZoomRange::Auto, None)
            .expect("should not fail");
        assert_eq!(cmd.get_program(), "tippecanoe");
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let shop_layer_arg = format!("--named-layer=Shops:{}", layers[1].path.display());
        let workdir_abs = std::path::absolute(&workdir).expect("absolute path of workdir");
        assert_eq!(
            args,
            &[
                "--json-progress",
                "--read-parallel",
                "--force",
                "--temporary-directory",
                workdir_abs.to_str().expect("workdir.to_str"),
                "-zg",
                "--extend-zooms-if-still-dropping",
                "-r",
                "1.2",
                "--drop-densest-as-needed",
                &shop_layer_arg,
                "--output",
                "test-output.pmtiles"
            ]
        );
    }

    #[test]
    fn test_make_tippecanoe_command_bounded_zoom() {
        let layers = [make_test_tile_layer("Shops", "diffed-shops.jsonl")];
        let workdir = PathBuf::from("test-workdir");
        let out = PathBuf::from("test-output.pmtiles");
        let cmd = make_tippecanoe_command(
            &layers,
            &workdir,
            &out,
            ZoomRange::Bounded { min: 13, max: 16 },
            None,
        )
        .expect("should not fail");
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|a| a == "-Z13"),
            "expected -Z13 in {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "-z16"),
            "expected -z16 in {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|a| a == "-zg" || a == "--extend-zooms-if-still-dropping"),
            "bounded zoom range should not also pass auto-zoom flags: {args:?}"
        );
    }

    #[test]
    fn test_make_tile_join_command() {
        let inputs = [
            PathBuf::from("overview.pmtiles"),
            PathBuf::from("detail.pmtiles"),
        ];
        let out = PathBuf::from("out.tmp.pmtiles");
        let cmd = make_tile_join_command(&inputs, &out);
        assert_eq!(cmd.get_program(), "tile-join");
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            &[
                "--force",
                "overview.pmtiles",
                "detail.pmtiles",
                "--output",
                "out.tmp.pmtiles"
            ]
        );
    }

    fn make_test_tile_layer(name: &str, filename: &str) -> TileLayer {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests");
        path.push("test_data");
        path.push(filename);
        TileLayer {
            name: String::from(name),
            path,
        }
    }

    #[test]
    fn test_parse_progress() {
        assert_eq!(parse_progress("{\"progress\":23.5}"), Some(23.5));
        assert_eq!(parse_progress("{\"progress\":-23.5}"), None);
        assert_eq!(parse_progress("{\"progress\":123.4}"), None);
        assert_eq!(parse_progress("warning: test message"), None);
    }
}
