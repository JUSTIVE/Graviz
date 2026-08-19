//! Schema loading: SDL file (+ optional overlay file) → marked ParsedGraph.

use anyhow::{bail, Context, Result};
use gompass_core::graph::{self, OverlayDiff, ParsedGraph, SdlToGraphOptions};
use std::path::Path;

pub struct LoadedSchema {
    pub graph: ParsedGraph,
    pub name: String,
    /// Present when an overlay was applied.
    pub diff: Option<OverlayDiff>,
}

pub fn load(
    schema_path: &Path,
    overlay_sdl: Option<&str>,
    hide_relay: bool,
) -> Result<LoadedSchema> {
    let sdl = std::fs::read_to_string(schema_path)
        .with_context(|| format!("reading {}", schema_path.display()))?;
    let name = schema_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| schema_path.display().to_string());
    let loaded = load_sdl(&sdl, name, overlay_sdl, hide_relay)?;
    crate::config::push_recent(schema_path);
    Ok(loaded)
}

/// Same pipeline, but from SDL already in memory (pasted into the landing
/// editor). Warnings from the base parse are surfaced for the banner.
pub fn load_sdl(
    sdl: &str,
    name: String,
    overlay_sdl: Option<&str>,
    hide_relay: bool,
) -> Result<LoadedSchema> {

    let base_options = SdlToGraphOptions {
        hide_relay_boilerplate: hide_relay,
        ..Default::default()
    };
    let mut graph = graph::sdl_to_graph(&sdl, &base_options);
    if let Some(err) = &graph.error {
        bail!("schema parse error: {err}");
    }

    let mut applied_diff = None;
    if let Some(overlay_sdl) = overlay_sdl.filter(|s| !s.trim().is_empty()) {
        let prepared = graph::prepare_overlay(&sdl, overlay_sdl);
        for w in &prepared.warnings {
            eprintln!("overlay warning: {w}");
        }
        let merged = graph::sdl_to_graph(
            &prepared.sdl,
            &SdlToGraphOptions {
                hide_relay_boilerplate: hide_relay,
                remove: prepared.removals,
                override_duplicates: true,
            },
        );
        if let Some(err) = &merged.error {
            bail!("overlay parse error: {err}");
        }
        let (marked, diff) = graph::mark_overlay(&graph, merged);
        eprintln!(
            "overlay: +{} types, +{} fields, ~{} fields, -{} types, -{} fields",
            diff.added_types.len(),
            diff.added_fields.len(),
            diff.changed_fields.len(),
            diff.removed_types.len(),
            diff.removed_fields.len(),
        );
        graph = marked;
        applied_diff = Some(diff);
    }

    Ok(LoadedSchema { graph, name, diff: applied_diff })
}

/// Combined mtime fingerprint of the schema + overlay files.
pub fn fingerprint(schema_path: &Path) -> u128 {
    fn mtime(p: &Path) -> u128 {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }
    mtime(schema_path)
}
