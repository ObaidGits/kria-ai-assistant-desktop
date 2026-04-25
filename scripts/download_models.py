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

ComfyUI models (Tier B image generation):
  python scripts/download_models.py --comfyui
  KRIA_DATA_DIR=/custom/path python scripts/download_models.py --comfyui
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

# ── CLI flags ───────────────────────────────────────────────────────────────
COMFYUI_MODE = "--comfyui" in sys.argv

# Default to <project-root>/models so no sudo is needed.
# Override by setting MODELS_DIR env var (e.g. for Docker volumes at /models).
_SCRIPT_DIR = Path(__file__).resolve().parent
MODELS_DIR = Path(os.getenv("MODELS_DIR", str(_SCRIPT_DIR.parent / "models")))
DRY_RUN = os.getenv("DRY_RUN", "0") == "1"

# ComfyUI models directory (for --comfyui mode).
# Default: ~/.kria/comfyui/models — matches ComfyLaunchConfig.comfy_models_dir default.
KRIA_DATA_DIR = Path(os.getenv("KRIA_DATA_DIR", Path.home() / ".kria"))
COMFYUI_MODELS_DIR = KRIA_DATA_DIR / "comfyui" / "models"

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
            # Voice-pipeline v2 post-edit (Hinglish fix-pass) — Phase 5.
            "url": f"{HF_BASE}/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
            "dest": MODELS_DIR / "llm" / "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
            "desc": "Qwen2.5-3B-Instruct Q4_K_M — voice post-edit (~1.93 GB)",
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
            # Voice-pipeline v2 post-edit (Hinglish fix-pass) — Phase 5.
            # Pure-text 3B model dedicated to low-latency transcript correction;
            # NOT the VL model (vision overhead would blow the 500 ms TTFA budget).
            "url": f"{HF_BASE}/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
            "dest": MODELS_DIR / "llm" / "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
            "desc": "Qwen2.5-3B-Instruct Q4_K_M — voice post-edit (~1.93 GB)",
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
    # ── Female voice (default) ──────────────────────────────────────────────
    # en_US-ljspeech-high: trained on the LJ Speech dataset (Linda Johnson),
    # a natural-sounding US English female voice at 22 050 Hz, high quality.
    {
        "url": f"{HF_BASE}/rhasspy/piper-voices/resolve/main/en/en_US/ljspeech/high/en_US-ljspeech-high.onnx",
        "dest": MODELS_DIR / "piper" / "en_US-ljspeech-high.onnx",
        "desc": "Piper TTS female voice model — ljspeech high (~97 MB)",
    },
    {
        "url": f"{HF_BASE}/rhasspy/piper-voices/resolve/main/en/en_US/ljspeech/high/en_US-ljspeech-high.onnx.json",
        "dest": MODELS_DIR / "piper" / "en_US-ljspeech-high.onnx.json",
        "desc": "Piper TTS female voice config — ljspeech high",
    },
    # ── Male voice (alternative) ─────────────────────────────────────────────
    {
        "url": f"{HF_BASE}/rhasspy/piper-voices/resolve/main/en/en_US/ryan/high/en_US-ryan-high.onnx",
        "dest": MODELS_DIR / "piper" / "en_US-ryan-high.onnx",
        "desc": "Piper TTS male voice model — ryan high (~65 MB)",
    },
    {
        "url": f"{HF_BASE}/rhasspy/piper-voices/resolve/main/en/en_US/ryan/high/en_US-ryan-high.onnx.json",
        "dest": MODELS_DIR / "piper" / "en_US-ryan-high.onnx.json",
        "desc": "Piper TTS male voice config — ryan high",
    },
    {
        "url": f"{HF_BASE}/snakers4/silero-vad/resolve/master/src/silero_vad/data/silero_vad.onnx",
        "dest": MODELS_DIR / "vad" / "silero_vad.onnx",
        "desc": "Silero VAD model (~2 MB)",
    },
    # ── Voice-pipeline v2 wake-word stack — Phase 4 (openWakeWord) ──
    # Phase 1 ships the generic pre-trained "hey_jarvis" model as a placeholder
    # for "Hey Ria". Custom-sample training for the actual "Hey Ria" /
    # "Hey Riya" / "Hello Ria" / "Hello Riya" aliases is a Phase 7 enhancement
    # (see plan, Phase 7 — Custom wake-word training).
    {
        "url": "https://github.com/dscripka/openWakeWord/raw/main/openwakeword/resources/models/melspectrogram.onnx",
        "dest": MODELS_DIR / "wake" / "melspectrogram.onnx",
        "desc": "openWakeWord mel-spectrogram frontend (~1 MB)",
    },
    {
        "url": "https://github.com/dscripka/openWakeWord/raw/main/openwakeword/resources/models/embedding_model.onnx",
        "dest": MODELS_DIR / "wake" / "embedding_model.onnx",
        "desc": "openWakeWord shared speech embedding (~5 MB)",
    },
    {
        "url": "https://github.com/dscripka/openWakeWord/raw/main/openwakeword/resources/models/hey_jarvis_v0.1.onnx",
        "dest": MODELS_DIR / "wake" / "hey_ria.onnx",
        "desc": "openWakeWord generic keyword head (placeholder for hey_ria, ~5 MB)",
    },
]

# ── Old files that may need cleanup ───────────────────────────────
OLD_FILES: list[Path] = [
    MODELS_DIR / "llm" / "Qwen_Qwen2.5-VL-7B-Instruct-Q4_K_M.gguf",
    MODELS_DIR / "llm" / "Qwen2.5-VL-7B-Instruct-vision.gguf",
    MODELS_DIR / "stt" / "ggml-large-v3-turbo.bin",  # replaced by q5_0 variant
]

# ── ComfyUI / Flux Tier-B image generation models ─────────────────
# These are only downloaded when --comfyui flag is passed.
# Total size: ~4.0 GB
#
# Flux.1-schnell GGUF Q4_K_S — the generation model.
# Must be placed in ComfyUI's unet/ directory and loaded via the GGUF node.
# CFG must be 1.0 (enforced by kria-core capabilities.rs — do NOT change).
#
# CLIP-L — text encoder (first half of Flux dual-encoder; T5 is optional).
# ae.safetensors — the official Flux VAE (required for decode step).
COMFYUI_DOWNLOADS: list[dict] = [
    {
        # Flux.1-schnell quantised to Q4_K_S (~3.4 GB).
        # Source: city96's GGUF conversion of black-forest-labs/FLUX.1-schnell.
        "url": f"{HF_BASE}/city96/FLUX.1-schnell-gguf/resolve/main/flux1-schnell-Q4_K_S.gguf",
        "dest": COMFYUI_MODELS_DIR / "unet" / "flux1-schnell-Q4_K_S.gguf",
        "desc": "Flux.1-schnell Q4_K_S GGUF (~3.4 GB) — Tier B generation model",
    },
    {
        # CLIP-L text encoder — required for Flux.
        "url": f"{HF_BASE}/comfyanonymous/flux_text_encoders/resolve/main/clip_l.safetensors",
        "dest": COMFYUI_MODELS_DIR / "clip" / "clip_l.safetensors",
        "desc": "CLIP-L text encoder (~246 MB) — Flux text encoder",
    },
    {
        # Official Flux VAE (ae.safetensors).
        # Primary: Comfy-Org repackaged mirror (non-gated, no token required).
        # Fallback: original black-forest-labs repo (gated — requires accepting
        #           the license at https://huggingface.co/black-forest-labs/FLUX.1-schnell).
        "url": f"{HF_BASE}/Comfy-Org/flux1-schnell/resolve/main/ae.safetensors",
        "fallback_url": f"{HF_BASE}/black-forest-labs/FLUX.1-schnell/resolve/main/ae.safetensors",
        "fallback_token_required": True,
        "fallback_license_url": "https://huggingface.co/black-forest-labs/FLUX.1-schnell",
        "dest": COMFYUI_MODELS_DIR / "vae" / "ae.safetensors",
        "desc": "Flux VAE ae.safetensors (~335 MB) — required for image decode",
    },
    {
        # T5-XXL FP8 text encoder — opt-in, only downloaded when KRIA_DOWNLOAD_T5=1.
        # Required for prompts longer than ~50 tokens (complex descriptions).
        # Without it, the orchestrator falls back to CLIP-only encoding (fine for short prompts).
        "url": f"{HF_BASE}/comfyanonymous/flux_text_encoders/resolve/main/t5xxl_fp8_e4m3fn.safetensors",
        "dest": COMFYUI_MODELS_DIR / "clip" / "t5xxl_fp8_e4m3fn.safetensors",
        "desc": "T5-XXL FP8 text encoder (~4.9 GB) — opt-in for long-prompt support (set KRIA_DOWNLOAD_T5=1)",
        "opt_in_env": "KRIA_DOWNLOAD_T5",
    },
]


MAX_RETRIES = 5
RETRY_WAIT = 10  # seconds between retries


def download_file(
    url: str,
    dest: Path,
    desc: str,
    token_required: bool = False,
    fallback_url: str = "",
    fallback_token_required: bool = False,
    fallback_license_url: str = "",
) -> None:
    if dest.exists():
        print(f"  [SKIP] {desc} — already exists at {dest}")
        return

    if DRY_RUN:
        print(f"  [DRY]  Would download: {url}")
        print(f"         → {dest}")
        return

    if token_required and not HF_TOKEN:
        print(f"  [WARN] {desc}")
        print(f"         HF_TOKEN not set — this file requires HuggingFace authentication.")
        print(f"         Set HF_TOKEN in your .env or environment and re-run.")
        print(f"         Skipping: {dest.name}")
        return

    dest.parent.mkdir(parents=True, exist_ok=True)

    # Try each URL in turn (primary, then fallback).
    urls_to_try: list[tuple[str, bool, str]] = [(url, token_required, "")]
    if fallback_url:
        urls_to_try.append((fallback_url, fallback_token_required, fallback_license_url))

    last_exc: Exception | None = None
    for url_attempt, (try_url, try_token_req, license_url) in enumerate(urls_to_try, start=1):
        if url_attempt > 1:
            print(f"  [FALLBACK] trying alternate source: {try_url}")
        _download_url(
            url=try_url,
            dest=dest,
            desc=desc,
            token_required=try_token_req,
            license_url=license_url,
        )
        if dest.exists():
            return  # success

    raise RuntimeError(f"All download sources exhausted for {desc}")


def _download_url(
    url: str,
    dest: Path,
    desc: str,
    token_required: bool,
    license_url: str,
) -> None:
    """Attempt to download *url* to *dest*; returns without raising on licence-gate (403)."""
    import time

    if token_required and not HF_TOKEN:
        print(f"  [SKIP-TOKEN] {desc}: HF_TOKEN not set, skipping this source.")
        return

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
                # 403 on a gated HF repo means license not accepted — no point retrying.
                if resp.status_code == 403:
                    tmp.unlink(missing_ok=True)
                    print(f"  [403] {desc}: access denied (gated model — licence not accepted).")
                    if license_url:
                        print(f"         Accept the model licence at: {license_url}")
                        print(f"         Then re-run this script.")
                    elif HF_TOKEN:
                        print(f"         Your HF_TOKEN is set but this model repo requires")
                        print(f"         you to accept its licence on HuggingFace first.")
                    return  # signal failure without raising; caller may try fallback

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
                for remaining in range(RETRY_WAIT, 0, -1):
                    print(f"\r         Retrying in {remaining:2d}s...  ", end="", flush=True)
                    time.sleep(1)
                print(f"\r                              \r", end="", flush=True)

    print(f"  [FAIL] {desc}: failed after {MAX_RETRIES} attempts from {url}")


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

    if COMFYUI_MODE:
        _run_comfyui_downloads()
        return

    _run_tier_downloads()


def _run_comfyui_downloads() -> None:
    """Download ComfyUI / Flux Tier-B image generation models."""
    print(f"Mode          : ComfyUI / Flux (Tier B image generation)")
    print(f"Models dir    : {COMFYUI_MODELS_DIR}")
    if not HF_TOKEN:
        print()
        print("  [NOTE] HF_TOKEN is not set.")
        print("         The Flux VAE will be downloaded from a non-gated mirror.")
        print("         If that fails, export HF_TOKEN and accept the licence at:")
        print("         https://huggingface.co/black-forest-labs/FLUX.1-schnell")
    if DRY_RUN:
        print("DRY_RUN=1 — no files will be downloaded")
    print()

    # Filter opt-in items based on their env-var gate.
    downloads = [
        item for item in COMFYUI_DOWNLOADS
        if "opt_in_env" not in item
        or os.getenv(item["opt_in_env"]) == "1"
    ]
    opt_in_skipped = [
        item for item in COMFYUI_DOWNLOADS
        if "opt_in_env" in item and os.getenv(item["opt_in_env"]) != "1"
    ]

    print("ComfyUI models to download:")
    for item in downloads:
        needs_token = item.get("token_required", False)
        token_note = "  [HF token required]" if needs_token else ""
        print(f"  • {item['desc']}{token_note}")
    if opt_in_skipped:
        print()
        print("Optional models (skipped — set env var to enable):")
        for item in opt_in_skipped:
            print(f"  • {item['desc']}")
            print(f"    → Set {item['opt_in_env']}=1 to download")
    print(f"  Total: ~4.0 GB (without opt-in T5: ~4.0 GB)")
    print()

    errors = []
    for item in downloads:
        try:
            download_file(
                item["url"],
                item["dest"],
                item["desc"],
                token_required=item.get("token_required", False),
                fallback_url=item.get("fallback_url", ""),
                fallback_token_required=item.get("fallback_token_required", False),
                fallback_license_url=item.get("fallback_license_url", ""),
            )
        except Exception as exc:
            print(f"  [FAIL] {item['desc']}: {exc}")
            errors.append(item["desc"])

    print()
    print("=" * 50)
    if errors:
        print(f"WARNING: {len(errors)} download(s) failed:")
        for e in errors:
            print(f"  - {e}")
        sys.exit(1)
    else:
        print("ComfyUI models downloaded successfully.")
        print()
        print("Next: test local generation:")
        print("  KRIA_IMAGE_MODE=local_only cargo run -p kria-desktop")


def _run_tier_downloads() -> None:
    """Download LLM / STT / TTS / Wake models for the current hardware tier."""
    print(f"Hardware tier : {TIER}")
    print(f"Models dir    : {MODELS_DIR}")
    if DRY_RUN:
        print("DRY_RUN=1 — no files will be downloaded")
    print()

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
