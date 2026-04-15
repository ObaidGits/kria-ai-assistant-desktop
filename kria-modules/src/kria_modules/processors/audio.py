"""
Audio processor — Pre-Cognitive audio preprocessing.

Applies noise reduction, silence trimming, and normalization
to audio before STT processing. Tier-aware processing depth.
"""

import logging
import os
from pathlib import Path
from typing import Any

logger = logging.getLogger("kria.processors.audio")

METHODS = ["preprocess", "get_info"]


def preprocess(params: dict) -> dict:
    """
    Preprocess an audio file: noise reduction, silence trimming, normalization.

    Params:
        file_path: str — path to audio file
        output_path: str — path for processed output (optional, uses temp file)
        sample_rate: int — target sample rate (default 16000)
    """
    file_path = params.get("file_path", "")
    if not file_path or not os.path.isfile(file_path):
        raise FileNotFoundError(file_path)

    tier = params.get("_tier", "standard")
    target_sr = params.get("sample_rate", 16000)

    import librosa
    import numpy as np
    import soundfile as sf

    # Load audio
    y, sr = librosa.load(file_path, sr=target_sr, mono=True)

    result: dict[str, Any] = {
        "file_path": file_path,
        "original_duration_s": round(len(y) / sr, 2),
        "sample_rate": sr,
    }

    # Noise reduction (standard+)
    if tier not in ("lite",):
        try:
            import noisereduce as nr
            y = nr.reduce_noise(y=y, sr=sr, prop_decrease=0.8)
            result["noise_reduced"] = True
        except Exception as e:
            logger.warning("Noise reduction failed: %s", e)
            result["noise_reduced"] = False
    else:
        result["noise_reduced"] = False

    # Silence trimming
    y_trimmed, trim_indices = librosa.effects.trim(y, top_db=25)
    trimmed_start = round(trim_indices[0] / sr, 3)
    trimmed_end = round(trim_indices[1] / sr, 3)
    result["trimmed_range_s"] = [trimmed_start, trimmed_end]
    y = y_trimmed

    # Normalization
    peak = np.max(np.abs(y))
    if peak > 0:
        y = y / peak * 0.95
    result["normalized"] = True

    # Save processed audio
    output_path = params.get("output_path", "")
    if not output_path:
        import tempfile
        fd, output_path = tempfile.mkstemp(suffix=".wav")
        os.close(fd)

    sf.write(output_path, y, sr)

    result["output_path"] = output_path
    result["processed_duration_s"] = round(len(y) / sr, 2)
    result["output_size_kb"] = round(os.path.getsize(output_path) / 1024, 1)
    result["summary"] = (
        f"Audio {result['original_duration_s']}s → {result['processed_duration_s']}s | "
        f"noise_reduced={result['noise_reduced']} | {result['output_size_kb']}KB"
    )

    return result


def get_info(params: dict) -> dict:
    """
    Get audio file metadata without full processing.

    Params:
        file_path: str — path to audio file
    """
    file_path = params.get("file_path", "")
    if not file_path or not os.path.isfile(file_path):
        raise FileNotFoundError(file_path)

    import librosa

    y, sr = librosa.load(file_path, sr=None, mono=True)
    duration = len(y) / sr

    return {
        "file_path": file_path,
        "duration_s": round(duration, 2),
        "sample_rate": sr,
        "size_kb": round(os.path.getsize(file_path) / 1024, 1),
        "format": Path(file_path).suffix.lstrip("."),
        "summary": f"Audio: {round(duration, 2)}s @ {sr}Hz | {round(os.path.getsize(file_path) / 1024, 1)}KB",
    }
