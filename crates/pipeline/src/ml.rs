use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use rayon::prelude::*;

use crate::catalog::MlRow;

#[derive(Debug, Default)]
pub struct MlReport {
    pub embedded: u64,
    pub iqa_scored: u64,
    pub errored: u64,
}

/// Run embedder and IQA scorer on every file that doesn't have a result yet.
///
/// Skips gracefully when the model slots in `hub` are `None`.  Each file that
/// needs at least one ML result is loaded from the preview cache exactly once;
/// both models run in the same pass.
pub fn analyze_ml(
    catalog: &crate::catalog::Catalog,
    cache: &crate::cache::Cache,
    hub: &crate::models::ModelHub,
    batch_size: usize,
) -> anyhow::Result<MlReport> {
    if hub.is_empty() {
        tracing::info!("no ML models loaded — skipping analyze_ml");
        return Ok(MlReport::default());
    }

    // Collect the files that need each kind of ML work.
    let need_emb: std::collections::HashSet<i64> = if hub.embedder.is_some() {
        catalog
            .files_needing_embedding()?
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    } else {
        Default::default()
    };

    let need_iqa: std::collections::HashSet<i64> = if hub.iqa.is_some() {
        catalog
            .files_needing_iqa()?
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    } else {
        Default::default()
    };

    if need_emb.is_empty() && need_iqa.is_empty() {
        tracing::debug!("all files already have ML results — nothing to do");
        return Ok(MlReport::default());
    }

    // Build the union work list: (file_id, path, hash, need_emb, need_iqa).
    let mut work_map: std::collections::HashMap<i64, (std::path::PathBuf, u128, bool, bool)> =
        Default::default();

    if !need_emb.is_empty() {
        for (id, path, hash) in catalog.files_needing_embedding()? {
            work_map
                .entry(id)
                .or_insert_with(|| (path, hash, false, false))
                .2 = true;
        }
    }
    if !need_iqa.is_empty() {
        for (id, path, hash) in catalog.files_needing_iqa()? {
            let entry = work_map
                .entry(id)
                .or_insert_with(|| (path, hash, false, false));
            entry.3 = true;
        }
    }

    let work_items: Vec<(i64, std::path::PathBuf, u128, bool, bool)> = work_map
        .into_iter()
        .map(|(id, (path, hash, ne, ni))| (id, path, hash, ne, ni))
        .collect();

    tracing::info!(
        files = work_items.len(),
        embedder = hub.embedder.is_some(),
        iqa = hub.iqa.is_some(),
        "starting ML analysis"
    );

    let pending: Mutex<Vec<MlRow>> = Mutex::new(Vec::new());
    let embedded = AtomicU64::new(0);
    let iqa_scored = AtomicU64::new(0);
    let errored = AtomicU64::new(0);

    work_items
        .par_iter()
        .for_each(|(file_id, path, hash, want_emb, want_iqa)| {
            let preview_path = cache.path(*hash);
            if !preview_path.exists() {
                tracing::warn!(
                    path = %path.display(),
                    "preview not in cache — skipping ML"
                );
                errored.fetch_add(1, Ordering::Relaxed);
                return;
            }

            let img = match image::open(&preview_path) {
                Ok(img) => img,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to open preview for ML");
                    errored.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };

            let embedding = if *want_emb {
                hub.embedder.as_ref().and_then(|emb| {
                    match emb.embed(&img) {
                        Ok(vec) => {
                            embedded.fetch_add(1, Ordering::Relaxed);
                            Some((emb.name().to_string(), vec))
                        }
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "embedder failed");
                            errored.fetch_add(1, Ordering::Relaxed);
                            None
                        }
                    }
                })
            } else {
                None
            };

            let iqa_score = if *want_iqa {
                hub.iqa.as_ref().and_then(|scorer| {
                    match scorer.score(&img) {
                        Ok(score) => {
                            iqa_scored.fetch_add(1, Ordering::Relaxed);
                            Some((scorer.name().to_string(), score))
                        }
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "IQA scorer failed");
                            errored.fetch_add(1, Ordering::Relaxed);
                            None
                        }
                    }
                })
            } else {
                None
            };

            if embedding.is_none() && iqa_score.is_none() {
                return;
            }

            let to_flush = {
                let mut batch = pending.lock().expect("pending mutex poisoned");
                batch.push(MlRow { file_id: *file_id, embedding, iqa_score });
                if batch.len() >= batch_size {
                    Some(std::mem::take(&mut *batch))
                } else {
                    None
                }
            };

            if let Some(rows) = to_flush {
                if let Err(e) = catalog.flush_ml_batch(&rows) {
                    tracing::warn!(error = %e, "failed to flush ML batch");
                }
            }
        });

    // Final flush.
    let remaining = std::mem::take(&mut *pending.lock().expect("pending mutex poisoned"));
    if !remaining.is_empty() {
        catalog.flush_ml_batch(&remaining)?;
    }

    let report = MlReport {
        embedded: embedded.load(Ordering::Relaxed),
        iqa_scored: iqa_scored.load(Ordering::Relaxed),
        errored: errored.load(Ordering::Relaxed),
    };
    tracing::info!(
        embedded = report.embedded,
        iqa_scored = report.iqa_scored,
        errored = report.errored,
        "ML analysis complete"
    );
    Ok(report)
}
