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
                eprintln!("[embed] loaded from MEMPALACE_MODEL_DIR: {dir}");
                return Some(e);
            }
            Err(e) => eprintln!("[embed] MEMPALACE_MODEL_DIR failed ({dir}): {e}"),
        }
    }

    // 2. <MEMPALACE_PALACE_PATH>/../models/all-MiniLM-L6-v2
    if let Ok(palace) = std::env::var("MEMPALACE_PALACE_PATH") {
        let candidate = Path::new(&palace)
            .join("..")
            .join("models")
            .join("all-MiniLM-L6-v2");
        if let Ok(e) = Embedder::load(&candidate) {
            eprintln!(
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
                eprintln!("[embed] loaded from ChromaDB path: {}", candidate.display());
                return Some(e);
            }
            Err(e) => eprintln!("[embed] ChromaDB path failed: {e}"),
        }
    }

    eprintln!("[embed] no embedder found — falling back to FTS5");
    None
}
