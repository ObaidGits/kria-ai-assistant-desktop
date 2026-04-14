"""
Video & Audio Preprocessing Module
====================================
- **Audio**: FFmpeg extraction → faster-whisper transcription (fallback:
  whisper.cpp server).
- **Video**: Audio transcription + sparse keyframe extraction via OpenCV
  (1 frame per major scene change, capped at *keyframe_max*).

All heavy work is offloaded to threads / subprocesses so the async event
loop is never blocked.
"""
from __future__ import annotations

import asyncio
import logging
import shutil
import subprocess
import tempfile
from io import BytesIO
from pathlib import Path
from typing import Optional

logger = logging.getLogger("kria.preprocessing.video_audio")


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

async def preprocess_audio(
    source: str,
    *,
    content: Optional[bytes] = None,
    max_tokens: int = 3500,
) -> "PreprocessedPayload":
    """Transcribe an audio file to token-budgeted text.

    Pipeline: source → FFmpeg to 16 kHz mono WAV → faster-whisper → smart_crop.
    """
    from kria.preprocessing.dispatcher import PreprocessedPayload
    from kria.preprocessing.token_budget import estimate_tokens, smart_crop

    wav_path = await _to_wav(source, content=content)
    if not wav_path:
        return PreprocessedPayload(
            text="", source_type="audio",
            metadata={"path": source, "error": "FFmpeg conversion failed"},
        )
    try:
        transcript = await _transcribe(wav_path)
    finally:
        _safe_unlink(wav_path)

    text, truncated = smart_crop(transcript, max_tokens)
    return PreprocessedPayload(
        text=text,
        token_estimate=estimate_tokens(text),
        source_type="audio",
        metadata={
            "path": source,
            "transcript_chars": len(transcript),
            "transcriber": _transcriber_label(),
        },
        truncated=truncated,
    )


async def preprocess_video(
    source: str,
    *,
    content: Optional[bytes] = None,
    max_tokens: int = 3500,
    keyframe_max: int = 5,
    scene_threshold: float = 0.3,
    image_max_edge: int = 1280,
) -> "PreprocessedPayload":
    """Transcribe audio + extract sparse keyframes from a video.

    Pipeline:
      Audio track  → FFmpeg → faster-whisper → text
      Visual track → OpenCV scene-change detection → keyframe images
    Keyframes processed through the image module (resize, compress).
    """
    from kria.preprocessing.dispatcher import PreprocessedPayload
    from kria.preprocessing.token_budget import estimate_image_tokens, estimate_tokens, smart_crop

    # If content bytes, write to temp file (OpenCV/FFmpeg need a file path)
    tmp_video = None
    video_path = source
    if content:
        tmp_video = tempfile.NamedTemporaryFile(
            delete=False, suffix=Path(source).suffix
        )
        tmp_video.write(content)
        tmp_video.flush()
        tmp_video.close()
        video_path = tmp_video.name

    try:
        # Run audio transcription and keyframe extraction in parallel
        wav_task = asyncio.create_task(_to_wav(video_path))
        kf_task = asyncio.create_task(
            _extract_keyframes(
                video_path,
                max_frames=keyframe_max,
                threshold=scene_threshold,
                max_edge=image_max_edge,
            )
        )
        wav_path, keyframes = await asyncio.gather(wav_task, kf_task)

        # Transcribe
        transcript = ""
        if wav_path:
            try:
                transcript = await _transcribe(wav_path)
            finally:
                _safe_unlink(wav_path)

    finally:
        if tmp_video:
            _safe_unlink(tmp_video.name)

    # Budget allocation: text gets max_tokens minus keyframe visual tokens
    visual_tokens = sum(
        estimate_image_tokens(m["width"], m["height"]) for _, m in keyframes
    )
    text_budget = max(500, max_tokens - visual_tokens)

    text, truncated = smart_crop(transcript, text_budget)
    text_tokens = estimate_tokens(text)

    return PreprocessedPayload(
        text=text,
        images=[img_bytes for img_bytes, _ in keyframes],
        token_estimate=text_tokens + visual_tokens,
        source_type="video",
        metadata={
            "path": source,
            "transcript_chars": len(transcript),
            "keyframe_count": len(keyframes),
            "keyframe_details": [m for _, m in keyframes],
            "transcriber": _transcriber_label(),
        },
        truncated=truncated,
    )


# ---------------------------------------------------------------------------
# FFmpeg: extract audio → 16 kHz mono WAV
# ---------------------------------------------------------------------------

async def _to_wav(
    source: str,
    *,
    content: Optional[bytes] = None,
    sample_rate: int = 16000,
) -> Optional[str]:
    """Convert *source* to a 16 kHz mono WAV file. Returns temp path or None."""
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        logger.warning("ffmpeg not found on PATH — audio extraction skipped")
        return None

    # If raw bytes, write to temp file first
    tmp_in = None
    in_path = source
    if content:
        tmp_in = tempfile.NamedTemporaryFile(delete=False, suffix=Path(source).suffix)
        tmp_in.write(content)
        tmp_in.flush()
        tmp_in.close()
        in_path = tmp_in.name

    out_path = tempfile.mktemp(suffix=".wav")
    cmd = [
        ffmpeg, "-y", "-i", in_path,
        "-vn",                          # drop video
        "-ac", "1",                     # mono
        "-ar", str(sample_rate),        # 16 kHz
        "-acodec", "pcm_s16le",         # 16-bit PCM
        out_path,
    ]

    try:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.PIPE,
        )
        _, stderr = await asyncio.wait_for(proc.communicate(), timeout=120)
        if proc.returncode != 0:
            logger.warning("ffmpeg failed (rc=%d): %s", proc.returncode, stderr.decode(errors="replace")[:300])
            _safe_unlink(out_path)
            return None
        return out_path
    except asyncio.TimeoutError:
        logger.warning("ffmpeg timed out for %s", source)
        _safe_unlink(out_path)
        return None
    except Exception as exc:
        logger.warning("ffmpeg error for %s: %s", source, exc)
        _safe_unlink(out_path)
        return None
    finally:
        if tmp_in:
            _safe_unlink(tmp_in.name)


# ---------------------------------------------------------------------------
# Transcription: faster-whisper (local) → whisper.cpp server (fallback)
# ---------------------------------------------------------------------------

_FASTER_WHISPER_AVAILABLE: Optional[bool] = None


def _transcriber_label() -> str:
    if _check_faster_whisper():
        return "faster-whisper"
    return "whisper-server"


def _check_faster_whisper() -> bool:
    global _FASTER_WHISPER_AVAILABLE
    if _FASTER_WHISPER_AVAILABLE is None:
        try:
            from faster_whisper import WhisperModel  # noqa: F401
            _FASTER_WHISPER_AVAILABLE = True
        except ImportError:
            _FASTER_WHISPER_AVAILABLE = False
    return _FASTER_WHISPER_AVAILABLE


async def _transcribe(wav_path: str) -> str:
    """Transcribe a WAV file. Tries faster-whisper locally, then whisper.cpp server."""
    if _check_faster_whisper():
        return await _transcribe_faster_whisper(wav_path)
    return await _transcribe_whisper_server(wav_path)


async def _transcribe_faster_whisper(wav_path: str) -> str:
    """Local transcription via faster-whisper (small model, CPU)."""
    def _run():
        from faster_whisper import WhisperModel
        model = WhisperModel("small", device="cpu", compute_type="int8")
        segments, _info = model.transcribe(wav_path, beam_size=1, language="en")
        return " ".join(seg.text.strip() for seg in segments)

    try:
        return await asyncio.to_thread(_run)
    except Exception as exc:
        logger.warning("faster-whisper failed, trying server: %s", exc)
        return await _transcribe_whisper_server(wav_path)


async def _transcribe_whisper_server(wav_path: str) -> str:
    """Fallback: send WAV to the whisper.cpp HTTP server."""
    import httpx
    from kria.infra.config import settings

    url = f"{settings.whisper_api_url}/inference"
    try:
        audio_bytes = await asyncio.to_thread(Path(wav_path).read_bytes)
        async with httpx.AsyncClient(timeout=60.0) as client:
            resp = await client.post(
                url,
                files={"file": ("audio.wav", audio_bytes, "audio/wav")},
                data={"response_format": "json"},
            )
            resp.raise_for_status()
            data = resp.json()
            return data.get("text", "").strip()
    except Exception as exc:
        logger.warning("whisper.cpp server transcription failed: %s", exc)
        return ""


# ---------------------------------------------------------------------------
# Keyframe extraction via OpenCV
# ---------------------------------------------------------------------------

async def _extract_keyframes(
    video_path: str,
    *,
    max_frames: int = 5,
    threshold: float = 0.3,
    max_edge: int = 1280,
) -> list[tuple[bytes, dict]]:
    """Extract sparse keyframes based on scene-change detection.

    Uses histogram difference between consecutive frames.  Only frames
    whose normalised difference exceeds *threshold* are kept.

    Returns list of ``(jpeg_bytes, {"width": ..., "height": ..., "frame_idx": ...})``.
    """
    def _run() -> list[tuple[bytes, dict]]:
        try:
            import cv2
            import numpy as np
        except ImportError:
            logger.info("opencv-python not installed — keyframe extraction skipped")
            return []

        cap = cv2.VideoCapture(video_path)
        if not cap.isOpened():
            logger.warning("Cannot open video: %s", video_path)
            return []

        total_frames = int(cap.get(cv2.CAP_PROP_FRAME_COUNT))
        fps = cap.get(cv2.CAP_PROP_FPS) or 30.0

        # Sample every N frames (at least 1 fps, but skip very short intervals)
        sample_interval = max(1, int(fps))

        prev_hist = None
        keyframes: list[tuple[bytes, dict]] = []
        frame_idx = 0

        while True:
            ret, frame = cap.read()
            if not ret:
                break

            if frame_idx % sample_interval != 0:
                frame_idx += 1
                continue

            gray = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
            hist = cv2.calcHist([gray], [0], None, [64], [0, 256])
            cv2.normalize(hist, hist)

            if prev_hist is not None:
                diff = cv2.compareHist(prev_hist, hist, cv2.HISTCMP_BHATTACHARYYA)
                if diff < threshold:
                    prev_hist = hist
                    frame_idx += 1
                    continue

            prev_hist = hist

            # Resize if needed
            h, w = frame.shape[:2]
            if max(h, w) > max_edge:
                scale = max_edge / max(h, w)
                frame = cv2.resize(
                    frame,
                    (int(w * scale), int(h * scale)),
                    interpolation=cv2.INTER_AREA,
                )
                h, w = frame.shape[:2]

            # Encode to JPEG
            _, buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, 85])
            keyframes.append((
                buf.tobytes(),
                {"width": w, "height": h, "frame_idx": frame_idx, "timestamp_s": round(frame_idx / fps, 2)},
            ))

            if len(keyframes) >= max_frames:
                break

            frame_idx += 1

        cap.release()

        # If no scene change detected above threshold, take evenly-spaced frames
        if not keyframes and total_frames > 0:
            cap2 = cv2.VideoCapture(video_path)
            step = max(1, total_frames // max_frames)
            for i in range(0, total_frames, step):
                cap2.set(cv2.CAP_PROP_POS_FRAMES, i)
                ret, frame = cap2.read()
                if not ret:
                    break
                h, w = frame.shape[:2]
                if max(h, w) > max_edge:
                    scale = max_edge / max(h, w)
                    frame = cv2.resize(frame, (int(w * scale), int(h * scale)), interpolation=cv2.INTER_AREA)
                    h, w = frame.shape[:2]
                _, buf = cv2.imencode(".jpg", frame, [cv2.IMWRITE_JPEG_QUALITY, 85])
                keyframes.append((
                    buf.tobytes(),
                    {"width": w, "height": h, "frame_idx": i, "timestamp_s": round(i / fps, 2)},
                ))
                if len(keyframes) >= max_frames:
                    break
            cap2.release()

        return keyframes

    return await asyncio.to_thread(_run)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _safe_unlink(path: str) -> None:
    try:
        Path(path).unlink(missing_ok=True)
    except OSError:
        pass
