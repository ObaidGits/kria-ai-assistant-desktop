#!/usr/bin/env python3
"""
Model Downloader — Tier-Aware
==============================
Downloads quantized GGUF models and Piper TTS voice data based on the
detected hardware tier.

Default destination: <project-root>/models/  (no sudo needed)
Override with:       MODELS_DIR=/your/path python scripts/download_models.py

Tier selection (set by detect_hardware.sh → ~/.kria/hardware_tier.env):
  lite        : Qwen2.5-3B Q4_K_M + Whisper small-q5_1
  standard    : Phi-4-mini Q4_K_M + Whisper medium-q5_0
  performance : Qwen2.5-VL-7B Q4_K_M + mmproj-F16 + Whisper turbo-q5_0
  high        : (same as performance — higher ctx at runtime only)

Override tier:  KRIA_TIER=lite python scripts/download_models.py
Dry run:        DRY_RUN=1 python scripts/download_models.py
"""
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

# Auto-load .env from project root if HF_TOKEN not already set
_ENV_FILE = _SCRIPT_DIR.parent / ".env"
if not os.getenv("HF_TOKEN") and _ENV_FILE.exists():
    for _line in _ENV_FILE.read_text().splitlines():
        _line = _line.strip()
        if _line.startswith("HF_TOKEN=") and not _line.startswith("#"):
            os.environ["HF_TOKEN"] = _line.split("=", 1)[1].strip()
            break

HF_TOKEN = os.getenv("HF_TOKEN", "")

# HuggingFace download base
HF_BASE = "https://huggingface.co"

# ── Tier detection ────────────────────────────────────────────────
# Priority: KRIA_TIER env var > ~/.kria/hardware_tier.env > default "standard"
def _detect_tier() -> str:
    tier = os.getenv("KRIA_TIER", "")
    if tier:
        return tier.lower()
    cache = Path.home() / ".kria" / "hardware_tier.env"
    if cache.exists():
        for line in cache.read_text().splitlines():
            line = line.strip()
            if line.startswith("KRIA_TIER="):
                val = line.split("=", 1)[1].strip().strip('"')
                if val:
                    return val.lower()
    return "standard"

TIER = _detect_tier()
VALID_TIERS = {"lite", "standard", "performance", "high"}
if TIER not in VALID_TIERS:
    print(f"ERROR: Unknown tier '{TIER}'. Valid: {', '.join(sorted(VALID_TIERS))}", file=sys.stderr)
    sys.exit(1)

# ── Tier → Model Downloads ───────────────────────────────────────
# "high" uses the same models as "performance" (only ctx/VRAM differs at runtime)
_effective_tier = "performance" if TIER == "high" else TIER

TIER_MODELS: dict[str, list[dict]] = {
    "lite": [
        {
            "url": f"{HF_BASE}/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
            "dest": MODELS_DIR / "llm" / "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
            "desc": "Qwen2.5-3B-Instruct Q4_K_M (~1.93 GB)",
        },
        {
            "url": f"{HF_BASE}/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
            "dest": MODELS_DIR / "stt" / "ggml-small-q5_1.bin",
            "desc": "Whisper small Q5_1 (~181 MB)",
        },
    ],
    "standard": [
        {
            "url": f"{HF_BASE}/bartowski/microsoft_Phi-4-mini-instruct-GGUF/resolve/main/microsoft_Phi-4-mini-instruct-Q4_K_M.gguf",
            "dest": MODELS_DIR / "llm" / "microsoft_Phi-4-mini-instruct-Q4_K_M.gguf",
            "desc": "Phi-4-mini-instruct Q4_K_M (~2.5 GB)",
        },
        {
            "url": f"{HF_BASE}/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin",
            "dest": MODELS_DIR / "stt" / "ggml-medium-q5_0.bin",
            "desc": "Whisper medium Q5_0 (~514 MB)",
        },
    ],
    "performance": [
        {
            "url": f"{HF_BASE}/unsloth/Qwen2.5-VL-7B-Instruct-GGUF/resolve/main/Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf",
            "dest": MODELS_DIR / "llm" / "Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf",
            "desc": "Qwen2.5-VL-7B-Instruct Q4_K_M (~4.68 GB)",
        },
        {
            "url": f"{HF_BASE}/unsloth/Qwen2.5-VL-7B-Instruct-GGUF/resolve/main/mmproj-F16.gguf",
            "dest": MODELS_DIR / "llm" / "mmproj-F16.gguf",
            "desc": "Qwen2.5-VL Vision Encoder mmproj-F16 (~1.35 GB)",
        },
        {
            "url": f"{HF_BASE}/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
            "dest": MODELS_DIR / "stt" / "ggml-large-v3-turbo-q5_0.bin",
            "desc": "Whisper large-v3-turbo Q5_0 (~547 MB)",
        },
    ],
}

# Piper TTS — same for all tiers
COMMON_DOWNLOADS: list[dict] = [
    {
        "url": f"{HF_BASE}/rhasspy/piper-voices/resolve/main/en/en_US/ryan/high/en_US-ryan-high.onnx",
        "dest": MODELS_DIR / "piper" / "en_US-ryan-high.onnx",
        "desc": "Piper TTS voice model (~65 MB)",
    },
    {
        "url": f"{HF_BASE}/rhasspy/piper-voices/resolve/main/en/en_US/ryan/high/en_US-ryan-high.onnx.json",
        "dest": MODELS_DIR / "piper" / "en_US-ryan-high.onnx.json",
        "desc": "Piper TTS voice config",
    },
]

# ── Old files that may need cleanup ───────────────────────────────
OLD_FILES: list[Path] = [
    MODELS_DIR / "llm" / "Qwen_Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf",
    MODELS_DIR / "llm" / "Qwen2.5-VL-7B-Instruct-vision.gguf",
    MODELS_DIR / "stt" / "ggml-large-v3-turbo.bin",  # replaced by q5_0 variant
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
        headers: dict[str, str] = {}
        if HF_TOKEN:
            headers["Authorization"] = f"Bearer {HF_TOKEN}"
        if resume_from:
            headers["Range"] = f"bytes={resume_from}-"

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
                for remaining in range(RETRY_WAIT, 0, -1):
                    print(f"\r         Retrying in {remaining:2d}s...  ", end="", flush=True)
                    time.sleep(1)
                print(f"\r                              \r", end="", flush=True)

    raise RuntimeError(f"Failed after {MAX_RETRIES} attempts")


def cleanup_old_files() -> None:
    """Offer to delete old model files that have been superseded."""
    present = [p for p in OLD_FILES if p.exists()]
    if not present:
        return

    total_bytes = sum(p.stat().st_size for p in present)
    total_gb = total_bytes / (1024 ** 3)

    print()
    print("  [INFO] Superseded model files found:")
    for p in present:
        size_gb = p.stat().st_size / (1024 ** 3)
        print(f"         {p.name}  ({size_gb:.2f} GB)")
    print(f"         Total: {total_gb:.2f} GB")
    print()

    if DRY_RUN:
        print("  [DRY]  Would prompt to delete old files (DRY_RUN=1 — skipping)")
        return

    try:
        answer = input("  Delete old files to reclaim disk space? [Y/N]: ").strip().upper()
    except (EOFError, KeyboardInterrupt):
        print("\n  Skipping deletion.")
        return

    if answer == "Y":
        for p in present:
            try:
                p.unlink()
                print(f"  [DEL]  Deleted {p.name}")
            except OSError as exc:
                print(f"  [WARN] Could not delete {p.name}: {exc}")
        print()
    else:
        print("  Keeping old files — proceeding with downloads.")
        print()


def main() -> None:
    print("K.R.I.A. Model Downloader")
    print("=" * 50)
    print(f"Hardware tier : {TIER}")
    print(f"Models dir    : {MODELS_DIR}")
    if DRY_RUN:
        print("DRY_RUN=1 — no files will be downloaded")
    print()

    # Build download list for this tier
    downloads = TIER_MODELS[_effective_tier] + COMMON_DOWNLOADS

    print(f"Models for tier '{TIER}':")
    for item in downloads:
        print(f"  • {item['desc']}")
    print()

    # Offer to clean up old files
    cleanup_old_files()

    errors = []
    for item in downloads:
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
        print(f"All models for tier '{TIER}' downloaded successfully.")


if __name__ == "__main__":
    main()
