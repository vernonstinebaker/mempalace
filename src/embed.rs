use crate::log::log;
/// Pure-Rust ONNX embedding using tract-onnx + HuggingFace tokenizers.
/// Loads all-MiniLM-L6-v2 (384-dim) and produces L2-normalised float32 vectors.
/// Returns None on any failure so callers can gracefully fall back to FTS5.
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokenizers::Tokenizer;
use tract_onnx::prelude::tract_ndarray::{s, Array2};
use tract_onnx::prelude::*;

pub struct Embedder {
    tokenizer: Tokenizer,
    model: Arc<TypedRunnableModel>,
}

impl Embedder {
    /// Try to load the model from the given directory.
    /// Returns Err if model files are missing or loading fails.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        let model_path = model_dir.join("model.onnx");

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("tokenizer load error: {e}"))?;

        let model = tract_onnx::onnx()
            .model_for_path(&model_path)?
            .into_optimized()?
            .into_runnable()?;

        Ok(Self { tokenizer, model })
    }

    /// Compute a 384-dim L2-normalised embedding and return it as 1536 raw bytes
    /// (384 × f32 little-endian), exactly matching the blob format expected by sqlite-vec.
    pub fn embed(&self, text: &str) -> Option<Vec<u8>> {
        self.embed_inner(text).ok()
    }

    fn embed_inner(&self, text: &str) -> Result<Vec<u8>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenize error: {e}"))?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
        let mask_raw: Vec<u32> = encoding.get_attention_mask().to_vec();
        let mask: Vec<i64> = mask_raw.iter().map(|&x| x as i64).collect();
        let type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&x| x as i64).collect();

        let seq_len = ids.len();

        let input_ids: Tensor = Array2::from_shape_vec((1, seq_len), ids)?.into();
        let attention_mask: Tensor = Array2::from_shape_vec((1, seq_len), mask)?.into();
        let token_type_ids: Tensor = Array2::from_shape_vec((1, seq_len), type_ids)?.into();

        let outputs = self.model.run(tvec![
            input_ids.into(),
            attention_mask.into(),
            token_type_ids.into(),
        ])?;

        // outputs[0] is last_hidden_state: shape [1, seq_len, 384]
        let hidden = outputs[0].to_dense_array_view::<f32>()?;

        // Attention-mask-weighted mean pool (matches ChromaDB / sentence-transformers).
        // Only average over real (non-padding) tokens — mask value 1 = real, 0 = padding.
        let hidden2d = hidden.slice(s![0, .., ..]); // [seq_len, 384]
        let mask_f32: Vec<f32> = mask_raw.iter().map(|&x| x as f32).collect();
        let mask_sum: f32 = mask_f32.iter().sum::<f32>().max(1e-9);
        let mean: Vec<f32> = (0..384)
            .map(|d| {
                let col = hidden2d.slice(s![.., d]);
                col.iter()
                    .zip(mask_f32.iter())
                    .map(|(h, m)| h * m)
                    .sum::<f32>()
                    / mask_sum
            })
            .collect();

        // L2 normalise
        let norm: f32 = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
        let normed: Vec<f32> = if norm > 1e-9 {
            mean.iter().map(|x| x / norm).collect()
        } else {
            mean
        };

        // Pack as little-endian bytes
        let mut bytes = Vec::with_capacity(384 * 4);
        for v in normed {
            bytes.extend_from_slice(&f32::to_le_bytes(v));
        }
        Ok(bytes)
    }
}

/// Try to build an Embedder from the standard search paths.
/// Returns None if the model is not found (search falls back to FTS5).
pub fn try_load_embedder() -> Option<Embedder> {
    // 1. MEMPALACE_MODEL_DIR env var
    if let Ok(dir) = std::env::var("MEMPALACE_MODEL_DIR") {
        match Embedder::load(Path::new(&dir)) {
            Ok(e) => {
                log!("info", "[embed] loaded from MEMPALACE_MODEL_DIR: {dir}");
                return Some(e);
            }
            Err(e) => log!("info", "[embed] MEMPALACE_MODEL_DIR failed ({dir}): {e}"),
        }
    }

    // 2. <MEMPALACE_PALACE_PATH>/../models/all-MiniLM-L6-v2
    if let Ok(palace) = std::env::var("MEMPALACE_PALACE_PATH") {
        let candidate = Path::new(&palace)
            .join("..")
            .join("models")
            .join("all-MiniLM-L6-v2");
        if let Ok(e) = Embedder::load(&candidate) {
            log!(
                "info",
                "[embed] loaded from palace sibling: {}",
                candidate.display()
            );
            return Some(e);
        }
    }

    // 3. ~/.cache/chroma/onnx_models/all-MiniLM-L6-v2/onnx (ChromaDB default)
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = Path::new(&home)
            .join(".cache")
            .join("chroma")
            .join("onnx_models")
            .join("all-MiniLM-L6-v2")
            .join("onnx");
        match Embedder::load(&candidate) {
            Ok(e) => {
                log!(
                    "info",
                    "[embed] loaded from ChromaDB path: {}",
                    candidate.display()
                );
                return Some(e);
            }
            Err(e) => log!("info", "[embed] ChromaDB path failed: {e}"),
        }
    }

    log!("warn", "[embed] no embedder found — falling back to FTS5");
    None
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns Some(Embedder) if the model is available on this machine.
    fn get_embedder() -> Option<Embedder> {
        // Check the ChromaDB default path (most likely on macOS dev machines)
        if let Some(home) = std::env::var_os("HOME") {
            let candidate = Path::new(&home)
                .join(".cache")
                .join("chroma")
                .join("onnx_models")
                .join("all-MiniLM-L6-v2")
                .join("onnx");
            if candidate.exists() {
                return Embedder::load(&candidate).ok();
            }
        }
        // Try the env var
        if let Ok(dir) = std::env::var("MEMPALACE_MODEL_DIR") {
            return Embedder::load(Path::new(&dir)).ok();
        }
        None
    }

    #[test]
    fn test_embed_returns_1536_bytes() {
        let emb = match get_embedder() {
            Some(e) => e,
            None => return, // skip if model unavailable
        };
        let result = emb.embed("hello world").expect("embed should succeed");
        assert_eq!(result.len(), 384 * 4, "384 f32 × 4 bytes = 1536");
    }

    #[test]
    fn test_embed_deterministic() {
        let emb = match get_embedder() {
            Some(e) => e,
            None => return,
        };
        let a = emb.embed("the cat sat on the mat").unwrap();
        let b = emb.embed("the cat sat on the mat").unwrap();
        assert_eq!(a, b, "same input must produce same embedding");
    }

    #[test]
    fn test_embed_different_inputs_different_outputs() {
        let emb = match get_embedder() {
            Some(e) => e,
            None => return,
        };
        let a = emb.embed("cats").unwrap();
        let b = emb.embed("the earth orbits the sun").unwrap();
        assert_ne!(a, b, "different inputs should produce different embeddings");
    }

    #[test]
    fn test_embed_empty_string() {
        let emb = match get_embedder() {
            Some(e) => e,
            None => return,
        };
        // Empty string should produce a valid 1536-byte embedding (not panic)
        let result = emb.embed("");
        assert!(result.is_some(), "empty string should produce an embedding");
        assert_eq!(
            result.unwrap().len(),
            1536,
            "empty string embedding should be 1536 bytes"
        );
    }

    #[test]
    fn test_embed_l2_normalized() {
        let emb = match get_embedder() {
            Some(e) => e,
            None => return,
        };
        let bytes = emb.embed("test vector").unwrap();
        // Decode f32s and check L2 norm ≈ 1.0
        let mut floats = Vec::with_capacity(384);
        for chunk in bytes.chunks_exact(4) {
            let val = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            floats.push(val);
        }
        let norm: f32 = floats.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-6,
            "L2 norm should be 1.0, got {norm}"
        );
    }

    #[test]
    fn test_embed_unicode() {
        let emb = match get_embedder() {
            Some(e) => e,
            None => return,
        };
        // CJK text
        let result = emb.embed("こんにちは世界");
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1536);

        // Emoji
        let result = emb.embed("🚀✨🔥");
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1536);
    }
}
