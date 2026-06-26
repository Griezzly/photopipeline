//! K-nearest-neighbor search over embedding vectors.
//!
//! Vectors are L2-normalized once up front, so cosine similarity reduces to a
//! plain dot product.  Only the brute-force backend is implemented this phase;
//! a `DuckDbVssKnn` (HNSW via the DuckDB `vss` extension, gated on
//! `cfg.catalog.enable_vss`) is a documented future alternative — see
//! `run_dedupe` for the runtime note about the omission.

use rayon::prelude::*;

/// Normalize `v` to unit L2 length in place.  A zero vector is left untouched
/// (avoids dividing by zero / producing NaN).
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity of two already-L2-normalized vectors (a dot product).
/// Iterates over `min(len)` so mismatched dims don't panic.
pub fn cosine_normalized(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Abstraction over neighbor search so a `vss`/HNSW backend can slot in later.
pub trait KnnIndex {
    /// Top-`k` neighbors of `query_idx` (excluding itself), as
    /// `(index, cosine)`, sorted descending by cosine.
    fn neighbors(&self, query_idx: usize, k: usize) -> Vec<(usize, f32)>;
}

/// Brute-force KNN: stores normalized vectors and computes cosine on demand.
pub struct BruteForceKnn {
    normalized: Vec<Vec<f32>>,
}

impl BruteForceKnn {
    /// `normalized` must already be L2-normalized (see [`l2_normalize`]).
    pub fn new(normalized: Vec<Vec<f32>>) -> Self {
        Self { normalized }
    }

    pub fn len(&self) -> usize {
        self.normalized.len()
    }

    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }

    /// Cosine similarity between stored vectors `i` and `j`.
    pub fn cosine(&self, i: usize, j: usize) -> f32 {
        cosine_normalized(&self.normalized[i], &self.normalized[j])
    }
}

impl KnnIndex for BruteForceKnn {
    fn neighbors(&self, query_idx: usize, k: usize) -> Vec<(usize, f32)> {
        let q = &self.normalized[query_idx];
        let mut sims: Vec<(usize, f32)> = (0..self.normalized.len())
            .into_par_iter()
            .filter(|&i| i != query_idx)
            .map(|i| (i, cosine_normalized(q, &self.normalized[i])))
            .collect();
        // Sort descending by cosine; tie-break by index for determinism.
        sims.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        sims.truncate(k);
        sims
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_yields_unit_norm() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "norm was {norm}");
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_is_safe() {
        let mut v = vec![0.0f32, 0.0, 0.0];
        l2_normalize(&mut v);
        // Stays all-zero, no NaN.
        assert!(v.iter().all(|x| *x == 0.0), "zero vector must not NaN");
    }

    #[test]
    fn cosine_of_identical_normalized_is_one() {
        let mut a = vec![1.0f32, 1.0, 0.0];
        l2_normalize(&mut a);
        let sim = cosine_normalized(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6, "sim was {sim}");
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let sim = cosine_normalized(&a, &b);
        assert!(sim.abs() < 1e-6, "sim was {sim}");
    }

    #[test]
    fn brute_force_neighbors_ranks_by_cosine() {
        // Index 0 close to 1, far from 2.
        let mut v0 = vec![1.0f32, 0.0];
        let mut v1 = vec![0.99f32, 0.14];
        let mut v2 = vec![0.0f32, 1.0];
        l2_normalize(&mut v0);
        l2_normalize(&mut v1);
        l2_normalize(&mut v2);
        let knn = BruteForceKnn::new(vec![v0, v1, v2]);
        assert_eq!(knn.len(), 3);

        let nbrs = knn.neighbors(0, 2);
        assert_eq!(nbrs.len(), 2, "should exclude self, return up to k");
        // Nearest neighbor of 0 is 1.
        assert_eq!(nbrs[0].0, 1, "closest neighbor of 0 should be index 1");
        assert!(
            nbrs[0].1 > nbrs[1].1,
            "neighbors must be sorted desc by cosine"
        );
    }
}
