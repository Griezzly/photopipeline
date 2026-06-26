//! Duplicate detection: time-window + embedding-similarity clustering.

pub mod cluster;
pub mod knn;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::catalog::{Catalog, DuplicateMember};
use crate::config::DedupeConfig;

pub use cluster::{build_edges, connected_components_sorted, quality_score};
pub use knn::{l2_normalize, BruteForceKnn, KnnIndex};

/// Summary of a dedupe run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DedupeReport {
    pub groups: u64,
    pub members: u64,
    pub keepers: u64,
}

/// Rebuild all duplicate groups from current embeddings.
///
/// Clears `duplicate_groups` + `duplicate_members`, then rebuilds. Running
/// twice on unchanged data produces identical groups (file_ids are sorted
/// before graph construction, so component membership and keeper selection
/// are deterministic).
pub fn run_dedupe(catalog: &Catalog, cfg: &DedupeConfig) -> anyhow::Result<DedupeReport> {
    if !cfg.enable {
        tracing::info!("dedupe disabled in config — skipping");
        return Ok(DedupeReport::default());
    }

    // Load embeddings, already ordered by file_id ASC (deterministic).
    let raw = catalog.load_all_embeddings()?;
    if raw.len() < cfg.min_group_size {
        tracing::info!(
            count = raw.len(),
            "fewer embeddings than min_group_size — nothing to cluster"
        );
        catalog.clear_duplicate_groups()?;
        return Ok(DedupeReport::default());
    }

    // Parallel arrays: ids[i], normalized[i], captured_at[i] all describe node i.
    let ids: Vec<i64> = raw.iter().map(|(id, _)| *id).collect();
    let mut normalized: Vec<Vec<f32>> = raw.into_iter().map(|(_, v)| v).collect();
    for v in normalized.iter_mut() {
        l2_normalize(v);
    }

    let captured_map = catalog.captured_at_map()?;
    let captured_at: Vec<Option<i64>> = ids
        .iter()
        .map(|id| captured_map.get(id).copied().flatten())
        .collect();

    let quality_map = catalog.quality_inputs_map()?;

    tracing::info!(
        photos = ids.len(),
        knn_k = cfg.knn_k,
        time_window_s = cfg.time_window_seconds,
        "building dedupe graph (brute-force KNN)"
    );

    // Brute force only this phase; surface the omission rather than cap silently.
    // (enable_vss lives on CatalogConfig, not DedupeConfig; we cannot read it
    // here, so the runtime note is emitted by the CLI in cmd_dedupe — see Task 9.)

    let edges = build_edges(&ids, &normalized, &captured_at, cfg);
    let components = connected_components_sorted(ids.len(), &edges);

    // Clear and rebuild.
    catalog.clear_duplicate_groups()?;

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut report = DedupeReport::default();

    for comp in &components {
        if comp.len() < cfg.min_group_size {
            continue;
        }

        // Score every member; pick the highest as keeper (tie-break: lowest
        // file_id, for determinism).
        let mut scored: Vec<(i64, f32)> = comp
            .iter()
            .map(|&idx| {
                let fid = ids[idx];
                let score = quality_score(quality_map.get(&fid));
                (fid, score)
            })
            .collect();
        // Sort by score desc, then file_id asc.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let keeper_id = scored[0].0;

        let group_id = catalog.insert_duplicate_group("time+embed", created_at)?;
        let members: Vec<DuplicateMember> = scored
            .iter()
            .map(|(fid, score)| DuplicateMember {
                file_id: *fid,
                is_suggested_keeper: *fid == keeper_id,
                quality_score: *score,
            })
            .collect();
        catalog.insert_duplicate_members(group_id, &members)?;

        report.groups += 1;
        report.members += members.len() as u64;
        report.keepers += 1;
    }

    tracing::info!(
        groups = report.groups,
        members = report.members,
        keepers = report.keepers,
        "dedupe complete"
    );
    Ok(report)
}
