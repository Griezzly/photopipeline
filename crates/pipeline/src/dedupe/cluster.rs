//! Edge construction, connected components, quality scoring, keeper selection.

use std::collections::HashSet;

use petgraph::graph::UnGraph;

use crate::catalog::QualityInputs;
use crate::config::DedupeConfig;
use crate::dedupe::knn::{BruteForceKnn, KnnIndex};

/// `quality_score = iqa.score
///                - 0.3 * has_blur
///                - 0.2 * has_back_focus
///                - 0.2 * max(clipped_highlights, clipped_shadows)`
///
/// A file with no `QualityInputs` (or no IQA score) scores from a 0.0 base —
/// documented choice: an unmeasured photo is treated as worst-quality so a
/// measured sibling wins the keeper slot.
pub fn quality_score(q: Option<&QualityInputs>) -> f32 {
    let Some(q) = q else { return 0.0 };
    let base = q.iqa_score.unwrap_or(0.0);
    let blur_pen = if q.has_blur { 0.3 } else { 0.0 };
    let bf_pen = if q.has_back_focus { 0.2 } else { 0.0 };
    let clip_pen = 0.2 * q.clipped_highlights.max(q.clipped_shadows);
    base - blur_pen - bf_pen - clip_pen
}

/// Build undirected edges from time-window and global-KNN rules.
///
/// `ids`, `normalized`, `captured_at` are parallel arrays indexed by node.
/// Returns deduplicated `(min, max)` index pairs, sorted for determinism.
///
/// **Edge sources:**
/// - Time-window: for each pair `(i, j)` where `|captured_at[i] - captured_at[j]|
///   <= cfg.time_window_seconds`, add an edge when cosine ≥
///   `cfg.cosine_threshold_within_window`.
/// - Global KNN: each node's top-`cfg.knn_k` neighbors with cosine ≥
///   `cfg.cosine_threshold_global`.
pub fn build_edges(
    ids: &[i64],
    normalized: &[Vec<f32>],
    captured_at: &[Option<i64>],
    cfg: &DedupeConfig,
) -> Vec<(usize, usize)> {
    let n = ids.len();
    let mut edge_set: HashSet<(usize, usize)> = HashSet::new();

    let knn = BruteForceKnn::new(normalized.to_vec());

    // --- Time-window edges ---------------------------------------------------
    // O(n^2) pairwise; fine at the spec's scale. For each captured pair within
    // the window, add an edge when cosine ≥ within-window threshold.
    for i in 0..n {
        let Some(ti) = captured_at[i] else { continue };
        for (offset, ts_j) in captured_at[(i + 1)..].iter().enumerate() {
            let j = i + 1 + offset;
            let Some(tj) = ts_j else { continue };
            let dt = ti.abs_diff(*tj);
            if dt <= cfg.time_window_seconds {
                let sim = knn.cosine(i, j);
                if sim >= cfg.cosine_threshold_within_window {
                    edge_set.insert((i, j));
                }
            }
        }
    }

    // --- Global KNN edges ----------------------------------------------------
    for i in 0..n {
        for (j, sim) in knn.neighbors(i, cfg.knn_k) {
            if sim >= cfg.cosine_threshold_global {
                let edge = if i < j { (i, j) } else { (j, i) };
                edge_set.insert(edge);
            }
        }
    }

    let mut edges: Vec<(usize, usize)> = edge_set.into_iter().collect();
    // Deterministic ordering.
    edges.sort_unstable();
    edges
}

/// Connected components of an undirected graph over `node_count` nodes.
///
/// Returns one `Vec<usize>` of node indices per component.  Nodes are added
/// in ascending order before edges, so component membership is deterministic
/// regardless of edge order.
///
/// Note: `petgraph::algo::connected_components` only returns a COUNT, not
/// membership — so component membership is recovered via a deterministic
/// union-find that uses the same sorted-edge input.
pub fn connected_components_sorted(node_count: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    // Build petgraph graph for correctness (future use / visualisation), even
    // though membership itself comes from the union-find below.
    let mut graph: UnGraph<usize, ()> = UnGraph::new_undirected();
    // Add nodes 0..node_count in order; node index == graph NodeIndex order.
    let node_ids: Vec<_> = (0..node_count).map(|i| graph.add_node(i)).collect();
    let mut sorted_edges = edges.to_vec();
    sorted_edges.sort_unstable();
    for &(a, b) in &sorted_edges {
        graph.add_edge(node_ids[a], node_ids[b], ());
    }

    // Union-find over node indices to recover component membership.
    let mut parent: Vec<usize> = (0..node_count).collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        // Path compression.
        let mut cur = x;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }

    for &(a, b) in &sorted_edges {
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra != rb {
            // Attach larger root to smaller for deterministic structure.
            if ra < rb {
                parent[rb] = ra;
            } else {
                parent[ra] = rb;
            }
        }
    }

    let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for i in 0..node_count {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    by_root.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::QualityInputs;
    use crate::config::DedupeConfig;

    #[test]
    fn quality_score_missing_inputs_is_zero() {
        assert_eq!(quality_score(None), 0.0);
    }

    #[test]
    fn quality_score_applies_penalties() {
        let q = QualityInputs {
            iqa_score: Some(0.9),
            has_blur: true,
            has_back_focus: false,
            clipped_highlights: 0.2,
            clipped_shadows: 0.5,
        };
        // 0.9 - 0.3*1 - 0.2*0 - 0.2*max(0.2,0.5) = 0.9 - 0.3 - 0.1 = 0.5
        let s = quality_score(Some(&q));
        assert!((s - 0.5).abs() < 1e-6, "score was {s}");
    }

    #[test]
    fn components_two_clusters_and_a_singleton() {
        // 0-1-2 form one component, 3-4 another, 5 alone.
        let edges = vec![(0, 1), (1, 2), (3, 4)];
        let mut comps = connected_components_sorted(6, &edges);
        // Sort each component and the outer list for stable assertion.
        for c in comps.iter_mut() {
            c.sort_unstable();
        }
        comps.sort_by_key(|c| c[0]);
        assert_eq!(comps, vec![vec![0, 1, 2], vec![3, 4], vec![5]]);
    }

    #[test]
    fn time_window_edges_only_within_window_and_threshold() {
        let ids = vec![10i64, 11, 12];
        // 0 and 1 nearly identical; 2 orthogonal.
        let normalized = {
            use crate::dedupe::knn::l2_normalize;
            let mut a = vec![1.0f32, 0.0];
            let mut b = vec![0.999f32, 0.044];
            let mut c = vec![0.0f32, 1.0];
            l2_normalize(&mut a);
            l2_normalize(&mut b);
            l2_normalize(&mut c);
            vec![a, b, c]
        };
        // 0 and 1 within 5s; 2 is 10000s away.
        let captured_at = vec![Some(1000i64), Some(1003), Some(11000)];
        let cfg = DedupeConfig {
            enable: true,
            time_window_seconds: 5,
            cosine_threshold_within_window: 0.92,
            cosine_threshold_global: 0.97,
            knn_k: 10,
            min_group_size: 2,
        };
        let edges = build_edges(&ids, &normalized, &captured_at, &cfg);
        // Expect exactly the (0,1) edge: within window AND cosine ≥ 0.92.
        assert!(
            edges.contains(&(0, 1)) || edges.contains(&(1, 0)),
            "expected an edge between 0 and 1, got {edges:?}"
        );
        // No edge should touch the orthogonal, far-away node 2.
        assert!(
            !edges.iter().any(|(a, b)| *a == 2 || *b == 2),
            "node 2 must stay isolated, got {edges:?}"
        );
    }

    #[test]
    fn global_knn_edges_link_high_cosine_far_apart_in_time() {
        let ids = vec![10i64, 11];
        let normalized = {
            use crate::dedupe::knn::l2_normalize;
            let mut a = vec![1.0f32, 0.01];
            let mut b = vec![1.0f32, 0.0];
            l2_normalize(&mut a);
            l2_normalize(&mut b);
            vec![a, b]
        };
        // Far apart in time → time-window rule won't fire; global KNN must.
        let captured_at = vec![Some(0i64), Some(1_000_000)];
        let cfg = DedupeConfig {
            enable: true,
            time_window_seconds: 60,
            cosine_threshold_within_window: 0.92,
            cosine_threshold_global: 0.97,
            knn_k: 10,
            min_group_size: 2,
        };
        let edges = build_edges(&ids, &normalized, &captured_at, &cfg);
        assert!(
            edges.contains(&(0, 1)) || edges.contains(&(1, 0)),
            "global KNN should link near-identical vectors, got {edges:?}"
        );
    }
}
