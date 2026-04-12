#!/usr/bin/env python3
"""
Model Downloader
================
Downloads quantized GGUF models and Piper TTS voice data.

Default destination: <project-root>/models/  (no sudo needed)
Override with:       MODELS_DIR=/your/path python scripts/download_models.py

Models downloaded:
  LLM:    Qwen3-8B-Q4_K_M.gguf        (~5.2 GB)
  Draft:  Qwen3-0.6B-Q8_0.gguf        (~0.6 GB)
  STT:    ggml-large-v3-turbo.bin      (~1.6 GB)
  TTS:    en_US-ryan-high.onnx + .json (~65 MB)

Only downloads if the target file does not already exist.
Set DRY_RUN=1 to print URLs without downloading.
"""
import hashlib
import os
import sys
from pathlib import Path

try:
    import httpx
    from tqdm import tqdm
except ImportError:
    print("Install dependencies: pip install httpx tqdm", file=sys.stderr)
    sys.exit(1)

# Default to <project-root>/models so no sudo is needed.
# Override by setting MODELS_DIR env var (e.g. for Docker volumes at /models).
_SCRIPT_DIR = Path(__file__).resolve().parent
MODELS_DIR = Path(os.getenv("MODELS_DIR", str(_SCRIPT_DIR.parent / "models")))
DRY_RUN = os.getenv("DRY_RUN", "0") == "1"

# HuggingFace download base
HF_BASE = "https://huggingface.co"

DOWNLOADS = [
    {
        "url": f"{HF_BASE}/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf",
        "dest": MODELS_DIR / "llm" / "Qwen3-8B-Q4_K_M.gguf",
        "desc": "Qwen3-8B LLM (5.2 GB)",
    },
    {
        "url": f"{HF_BASE}/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q8_0.gguf",
        "dest": MODELS_DIR / "llm" / "Qwen3-0.6B-Q8_0.gguf",
        "desc": "Qwen3-0.6B Draft (0.6 GB)",
    },
    {
        "url": f"{HF_BASE}/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin",
        "dest": MODELS_DIR / "stt" / "ggml-large-v3-turbo.bin",
        "desc": "Whisper large-v3-turbo (1.5 GB)",
    },
    {
        "url": "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/ryan/high/en_US-ryan-high.onnx",
        "dest": MODELS_DIR / "piper" / "en_US-ryan-high.onnx",
        "desc": "Piper TTS voice model (~65 MB)",
    },
    {
        "url": "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/ryan/high/en_US-ryan-high.onnx.json",
        "dest": MODELS_DIR / "piper" / "en_US-ryan-high.onnx.json",
        "desc": "Piper TTS voice config",
    },
]


MAX_RETRIES = 5
RETRY_WAIT = 10  # seconds between retries


def download_file(url: str, dest: Path, desc: str) -> None:
    if dest.exists():
        print(f"  [SKIP] {desc} — already exists at {dest}")
        return

    if DRY_RUN:
        print(f"  [DRY]  Would download: {url}")
        print(f"         → {dest}")
        return

    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".tmp")

    print(f"  [DOWN] {desc}")
    print(f"         {url}")
    print(f"         → {dest}")

    for attempt in range(1, MAX_RETRIES + 1):
        # Resume from existing .tmp file if present
        resume_from = tmp.stat().st_size if tmp.exists() else 0
        headers = {"Range": f"bytes={resume_from}-"} if resume_from else {}

        if resume_from:
            print(f"  [RESUME] attempt {attempt}/{MAX_RETRIES} — resuming from {resume_from:,} bytes")
        elif attempt > 1:
            print(f"  [RETRY] attempt {attempt}/{MAX_RETRIES}")

        try:
            with httpx.stream(
                "GET", url,
                headers=headers,
                follow_redirects=True,
                timeout=httpx.Timeout(connect=30.0, read=120.0, write=30.0, pool=5.0),
            ) as resp:
                resp.raise_for_status()

                # Server may ignore Range and send full file (200 instead of 206)
                if resp.status_code == 200 and resume_from:
                    resume_from = 0
                    tmp.unlink(missing_ok=True)

                total_remaining = int(resp.headers.get("content-length", 0))
                total = resume_from + total_remaining

                with open(tmp, "ab" if resume_from else "wb") as f, tqdm(
                    total=total,
                    initial=resume_from,
                    unit="B", unit_scale=True, unit_divisor=1024,
                    desc=desc[:40],
                ) as bar:
                    for chunk in resp.iter_bytes(chunk_size=65536):
                        f.write(chunk)
                        bar.update(len(chunk))

            tmp.rename(dest)
            print(f"  [DONE] {dest.name} ({dest.stat().st_size:,} bytes)")
            return

        except Exception as exc:
            print(f"  [FAIL] attempt {attempt}/{MAX_RETRIES}: {exc}")
            if attempt < MAX_RETRIES:
                import time
                print(f"         Waiting {RETRY_WAIT}s before retry...")
                time.sleep(RETRY_WAIT)

    raise RuntimeError(f"Failed after {MAX_RETRIES} attempts")


def main() -> None:
    print("K.R.I.A. Model Downloader")
    print("=" * 50)
    print(f"Models directory: {MODELS_DIR}")
    if DRY_RUN:
        print("DRY_RUN=1 — no files will be downloaded\n")

    errors = []
    for item in DOWNLOADS:
        try:
            download_file(item["url"], item["dest"], item["desc"])
        except Exception as exc:
            print(f"  [FAIL] {item['desc']}: {exc}")
            errors.append(item["desc"])

    print("\n" + "=" * 50)
    if errors:
        print(f"WARNING: {len(errors)} download(s) failed:")
        for e in errors:
            print(f"  - {e}")
        sys.exit(1)
    else:
        print("All models downloaded successfully.")


if __name__ == "__main__":
    main()
