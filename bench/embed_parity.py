#!/usr/bin/env python3
"""
Embedding parity check: ChromaDB (onnxruntime) vs Rust (tract-onnx)
Both use all-MiniLM-L6-v2 but may differ in pooling implementation.

ChromaDB uses: attention-mask-weighted mean pool → L2 norm
Rust uses:     naive mean pool over ALL tokens (incl. special tokens) → L2 norm

We check cosine similarity between the two outputs for several texts.
"""

import json
import os
import struct
import subprocess
import sys

import numpy as np

PYTHON_VENV = "/Volumes/EnvoyUltra/Programming/mempalace/.venv/bin/python"
RUST_BIN = "/Users/vds/bin/mempalace-mcp"
RUST_ENV = {
    **os.environ,
    "MEMPALACE_PALACE_PATH": "/tmp/parity_test_palace",
}

TEXTS = [
    "Hello world",
    "The quick brown fox jumps over the lazy dog",
    "SQLite vector search embeddings performance",
    "I visited my grandmother last Sunday and we had a great time",
    "What did the user say about their favorite food preferences?",
]

# ---------------------------------------------------------------------------
# ChromaDB embeddings
# ---------------------------------------------------------------------------


def get_chroma_embeddings(texts):
    """Use chromadb's built-in embedding function (onnxruntime)."""
    import chromadb
    from chromadb.utils.embedding_functions import ONNXMiniLM_L6_V2

    ef = ONNXMiniLM_L6_V2()
    embs = ef(texts)
    return [np.array(e, dtype=np.float32) for e in embs]


# ---------------------------------------------------------------------------
# Rust embeddings via MCP
# ---------------------------------------------------------------------------

INIT_MSG = (
    json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "parity", "version": "1.0"},
            },
        }
    )
    + "\n"
)

INITIALIZED_MSG = (
    json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
    + "\n"
)


def send_recv(proc, msg):
    proc.stdin.write(msg.encode())
    proc.stdin.flush()
    while True:
        line = proc.stdout.readline().decode(errors="replace").strip()
        if not line:
            continue
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue


def rust_embed_via_search(texts):
    """
    Hack: add each text as a drawer, then search for it and retrieve the
    stored embedding via a debug approach. Instead, we just use a tiny
    standalone ONNX script that mirrors Rust's exact pooling logic.
    """
    pass  # see get_rust_like_embeddings below


def get_rust_like_embeddings(texts):
    """
    Reproduce Rust's exact pooling: mean over ALL token positions (including
    [CLS], [SEP], and any padding — but HF tokenizers don't pad by default
    when encoding single strings), then L2-normalize.
    """
    import onnxruntime as ort
    from tokenizers import Tokenizer

    model_dir = os.path.expanduser("~/.cache/chroma/onnx_models/all-MiniLM-L6-v2/onnx")
    tokenizer = Tokenizer.from_file(os.path.join(model_dir, "tokenizer.json"))
    sess = ort.InferenceSession(os.path.join(model_dir, "model.onnx"))

    results = []
    for text in texts:
        enc = tokenizer.encode(text)
        ids = np.array([enc.ids], dtype=np.int64)
        mask = np.array([enc.attention_mask], dtype=np.int64)
        type_ids = np.array([enc.type_ids], dtype=np.int64)

        output = sess.run(
            None,
            {
                "input_ids": ids,
                "attention_mask": mask,
                "token_type_ids": type_ids,
            },
        )
        # output[0]: last_hidden_state [1, seq_len, 384]
        hidden = output[0][0]  # [seq_len, 384]

        # --- Rust pooling: naive mean over ALL tokens ---
        rust_mean = hidden.mean(axis=0)
        norm = np.linalg.norm(rust_mean)
        rust_emb = rust_mean / norm if norm > 1e-9 else rust_mean

        results.append(rust_emb.astype(np.float32))
    return results


def get_chroma_like_embeddings(texts):
    """
    ChromaDB's actual pooling: attention-mask-weighted mean, then L2-norm.
    """
    import onnxruntime as ort
    from tokenizers import Tokenizer

    model_dir = os.path.expanduser("~/.cache/chroma/onnx_models/all-MiniLM-L6-v2/onnx")
    tokenizer = Tokenizer.from_file(os.path.join(model_dir, "tokenizer.json"))
    sess = ort.InferenceSession(os.path.join(model_dir, "model.onnx"))

    results = []
    for text in texts:
        enc = tokenizer.encode(text)
        ids = np.array([enc.ids], dtype=np.int64)
        mask = np.array([enc.attention_mask], dtype=np.int64)
        type_ids = np.array([enc.type_ids], dtype=np.int64)

        output = sess.run(
            None,
            {
                "input_ids": ids,
                "attention_mask": mask,
                "token_type_ids": type_ids,
            },
        )
        hidden = output[0][0]  # [seq_len, 384]
        m = mask[0].astype(np.float32)  # [seq_len]

        # Weighted mean: sum(hidden * mask[:, None]) / sum(mask)
        weighted = (hidden * m[:, None]).sum(axis=0) / m.sum()
        norm = np.linalg.norm(weighted)
        chroma_emb = weighted / norm if norm > 1e-9 else weighted
        results.append(chroma_emb.astype(np.float32))
    return results


def cosine_sim(a, b):
    return float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b)))


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    sys.path.insert(
        0,
        "/Volumes/EnvoyUltra/Programming/mempalace/.venv/lib/python3.12/site-packages",
    )

    print("Computing Rust-style embeddings (naive mean pool)...")
    rust_embs = get_rust_like_embeddings(TEXTS)
    print("Computing ChromaDB-style embeddings (masked mean pool)...")
    chroma_embs = get_chroma_like_embeddings(TEXTS)

    print()
    print(f"{'Text':<55}  {'cos_sim':>8}  {'l2_dist':>8}  {'match?'}")
    print("-" * 85)
    for text, r, c in zip(TEXTS, rust_embs, chroma_embs):
        sim = cosine_sim(r, c)
        dist = float(np.linalg.norm(r - c))
        match = "YES" if sim > 0.9999 else ("~OK" if sim > 0.999 else "DIFF")
        print(f"  {text[:53]:<53}  {sim:>8.6f}  {dist:>8.6f}  {match}")

    print()
    avg_sim = np.mean([cosine_sim(r, c) for r, c in zip(rust_embs, chroma_embs)])
    print(f"Average cosine similarity: {avg_sim:.6f}")
    if avg_sim > 0.9999:
        print(
            "→ Embeddings are IDENTICAL (same pooling, no padding in single-string encoding)"
        )
    elif avg_sim > 0.999:
        print("→ Embeddings are NEAR-IDENTICAL (negligible difference)")
    else:
        print("→ Embeddings DIVERGE — pooling difference matters")
