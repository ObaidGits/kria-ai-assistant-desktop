#!/usr/bin/env python3
"""
K.R.I.A. Bridge
================
Runs on the HOST machine (outside Docker) with two roles:

  1. HTTP server (port 9000) — called by kria-core for host OS operations:
       audio playback, notifications, allowlisted exec.

  2. Voice loop (background thread) — full voice pipeline:
       mic capture → Silero VAD → record speech → noisereduce →
       Whisper STT → hallucination filter → kria-core /chat →
       Piper TTS → speaker playback.

     Uses Silero VAD for speech detection (falls back to webrtcvad → RMS).
     Applies noisereduce for audio cleanup before Whisper STT.
     Filters Whisper hallucinations ([BLANK_AUDIO], (wind blowing), etc).

     This runs on the HOST because Docker containers cannot access the
     physical microphone or speakers.

Voice dependencies (install on host machine):
    pip install sounddevice soundfile httpx numpy
    pip install silero-vad noisereduce webrtcvad   # optional but recommended

Usage:
    python scripts/kria_bridge.py             # HTTP server + voice loop
    python scripts/kria_bridge.py --no-voice  # HTTP server only
    python scripts/kria_bridge.py --debug     # show RMS + VAD diagnostics

Environment variables:
    BRIDGE_PORT             Bridge HTTP port            (default: 9000)
    KRIA_CORE_URL           kria-core REST API          (default: http://localhost:8000)
    KRIA_WHISPER_URL        whisper.cpp STT server      (default: http://localhost:8081)
    KRIA_PIPER_URL          Piper TTS server            (default: http://localhost:8082)
    KRIA_ENERGY_THRESHOLD   RMS fallback trigger level  (default: 2000)
    KRIA_SILENCE_MS         Silence to end utterance    (default: 600)
    KRIA_VOICE_SESSION      Session ID for history      (default: voice)
    KRIA_MIC_DEVICE         Microphone: numeric index, partial name, or 'auto'
                            (default: auto — first device with input channels)
                            Examples: KRIA_MIC_DEVICE=8
                                      KRIA_MIC_DEVICE=sof-hda-dsp
                                      KRIA_MIC_DEVICE=auto
    KRIA_SPEAKER_DEVICE     Speaker output: numeric index, partial name, or 'auto'
                            (default: auto — sounddevice default output)
                            Examples: KRIA_SPEAKER_DEVICE=4
                                      KRIA_SPEAKER_DEVICE=sof-hda-dsp
    KRIA_TTS_VOICE          Piper voice model filename without extension
                            (default: en_US-lessac-high — female US English)
                            Examples: KRIA_TTS_VOICE=en_US-lessac-high
                                      KRIA_TTS_VOICE=en_US-ryan-high
"""
import base64
import hashlib
import hmac
import io
import json
import logging
import os
import re
import struct
import subprocess
import sys
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s [%(name)s] %(message)s",
)
logger = logging.getLogger("kria.bridge")

# Load .env from project root (two levels up from scripts/)
_env_path = Path(__file__).parent.parent / ".env"
if _env_path.exists():
    try:
        with open(_env_path) as _ef:
            for _line in _ef:
                _line = _line.strip()
                if _line and not _line.startswith("#") and "=" in _line:
                    _k, _, _v = _line.partition("=")
                    os.environ.setdefault(_k.strip(), _v.strip())
        logger.info("Loaded .env from %s", _env_path)
    except Exception as _e:
        logger.warning("Could not load .env: %s", _e)

# Suppress noisy loggers
logging.getLogger("httpx").setLevel(logging.WARNING)
logging.getLogger("torch").setLevel(logging.WARNING)
logging.getLogger("torchaudio").setLevel(logging.WARNING)

# ── Configuration ──────────────────────────────────────────────────────────────
PORT             = int(os.getenv("BRIDGE_PORT",           "9000"))
CORE_URL         = os.getenv("KRIA_CORE_URL",             "http://localhost:8000")
WHISPER_URL      = os.getenv("KRIA_WHISPER_URL",          "http://localhost:8081")
PIPER_URL        = os.getenv("KRIA_PIPER_URL",            "http://localhost:8082")
ENERGY_THRESHOLD = float(os.getenv("KRIA_ENERGY_THRESHOLD", "2000"))
SILENCE_MS       = int(os.getenv("KRIA_SILENCE_MS",       "600"))
VOICE_SESSION    = os.getenv("KRIA_VOICE_SESSION",        "voice")
# TTS voice model name (without .onnx extension).
# Must match a file in the piper models directory on the kria-voice server.
TTS_VOICE        = os.getenv("KRIA_TTS_VOICE",            "en_US-lessac-high")
# Speaker output device — numeric index, partial name, or 'auto'.
SPEAKER_DEVICE_RAW = os.getenv("KRIA_SPEAKER_DEVICE",     "auto").strip()
# Microphone device — numeric index, partial name substring, or 'auto'.
# Run with --list-devices to find the right index/name.
# 'auto' (default) picks the first device that has input channels.
MIC_DEVICE_RAW   = os.getenv("KRIA_MIC_DEVICE", "auto").strip()


def _resolve_mic_device(spec: str) -> int | None:
    """Return a sounddevice device index from an index str, name substring, or 'auto'.

    Returns None when spec is 'auto' so sounddevice uses its own default.
    Raises ValueError with a helpful message when the spec matches nothing
    usable or matches an output-only device.
    """
    import sounddevice as _sd

    devices = _sd.query_devices()
    input_devices = [(i, d) for i, d in enumerate(devices) if d["max_input_channels"] >= 1]

    if not input_devices:
        raise ValueError("No input audio devices found on this system.")

    if spec.lower() == "auto":
        # Pick the first device that has input channels
        idx, info = input_devices[0]
        logger.info("MIC auto-selected: [%d] %s", idx, info["name"])
        return idx

    # Try numeric index first
    try:
        idx = int(spec)
        info = devices[idx]
        if info["max_input_channels"] < 1:
            avail = ", ".join(f"[{i}] {d['name']}" for i, d in input_devices)
            raise ValueError(
                f"Device [{idx}] '{info['name']}' has no input channels.\n"
                f"Available input devices: {avail}")
        return idx
    except (ValueError, IndexError) as exc:
        if "no input channels" in str(exc).lower() or "available input" in str(exc).lower():
            raise
        pass  # not numeric — try name match

    # Match by partial name (case-insensitive)
    spec_lower = spec.lower()
    matches = [(i, d) for i, d in input_devices if spec_lower in d["name"].lower()]
    if not matches:
        avail = ", ".join(f"[{i}] {d['name']}" for i, d in input_devices)
        raise ValueError(
            f"No input device found matching '{spec}'.\n"
            f"Available input devices: {avail}")
    if len(matches) > 1:
        logger.warning("MIC spec '%s' matches %d devices, using first: [%d] %s",
                       spec, len(matches), matches[0][0], matches[0][1]["name"])
    idx, info = matches[0]
    return idx


# Resolved at startup (before the voice thread): None = sounddevice default
try:
    MIC_DEVICE = _resolve_mic_device(MIC_DEVICE_RAW)
except Exception as _mic_err:
    # Non-fatal at import time — voice thread will re-raise with context
    MIC_DEVICE = None
    logger.warning("MIC_DEVICE resolution deferred: %s", _mic_err)


def _resolve_speaker_device(spec: str) -> int | None:
    """Return a sounddevice output device index from index str, name, or 'auto'.

    Returns None to let sounddevice use its own default.
    """
    import sounddevice as _sd

    if spec.lower() == "auto":
        return None  # sounddevice default

    devices = _sd.query_devices()
    output_devices = [(i, d) for i, d in enumerate(devices) if d["max_output_channels"] >= 1]

    try:
        idx = int(spec)
        info = devices[idx]
        if info["max_output_channels"] < 1:
            avail = ", ".join(f"[{i}] {d['name']}" for i, d in output_devices)
            raise ValueError(f"Device [{idx}] '{info['name']}' has no output channels.\nAvailable: {avail}")
        logger.info("Speaker resolved: [%d] %s", idx, info["name"])
        return idx
    except (ValueError, IndexError) as exc:
        if "no output channels" in str(exc).lower() or "available" in str(exc).lower():
            raise
        pass

    spec_lower = spec.lower()
    matches = [(i, d) for i, d in output_devices if spec_lower in d["name"].lower()]
    if not matches:
        avail = ", ".join(f"[{i}] {d['name']}" for i, d in output_devices)
        raise ValueError(f"No output device found matching '{spec}'.\nAvailable: {avail}")
    idx, info = matches[0]
    logger.info("Speaker resolved: [%d] %s", idx, info["name"])
    return idx


try:
    SPEAKER_DEVICE = _resolve_speaker_device(SPEAKER_DEVICE_RAW)
except Exception as _spk_err:
    SPEAKER_DEVICE = None
    logger.warning("SPEAKER_DEVICE resolution deferred: %s", _spk_err)

# Wake word — any transcription containing this string (case-insensitive) is processed
WAKE_KEYWORD     = os.getenv("KRIA_WAKE_KEYWORD",         "riya")

# ── Bridge secret ─────────────────────────────────────────────────────────────
SECRET = os.getenv("BRIDGE_SECRET", "")
if not SECRET:
    _default_path = Path.home() / ".kria" / "bridge_secret.txt"
    secret_path = Path(os.getenv("BRIDGE_SECRET_FILE", str(_default_path)))
    if secret_path.exists():
        SECRET = secret_path.read_text().strip()
    else:
        import secrets as _secrets
        SECRET = _secrets.token_hex(32)
        secret_path.parent.mkdir(parents=True, exist_ok=True)
        secret_path.write_text(SECRET)
        logger.info("Generated new bridge secret → %s", secret_path)
        logger.info("Set KRIA_BRIDGE_SECRET=%s in your .env to match", SECRET)


def _verify(provided: str) -> bool:
    return hmac.compare_digest(
        hmac.new(SECRET.encode(), b"kria-bridge", hashlib.sha256).hexdigest(),
        provided or "",
    )


# ══════════════════════════════════════════════════════════════════════════════
# Voice Loop
# ══════════════════════════════════════════════════════════════════════════════

class VoiceLoop:
    """
    Background thread that drives the full voice assistant pipeline on the host.

    Flow: mic -> Silero VAD -> record speech -> denoise -> Whisper STT ->
          hallucination filter -> kria-core /chat -> Piper TTS -> speaker.

    Uses Silero VAD for speech detection (replaces simple RMS energy gate).
    Falls back to webrtcvad, then to RMS energy if neither is available.

    Requirements: pip install sounddevice numpy soundfile httpx
    Optional:     pip install silero-vad noisereduce webrtcvad
    """

    _TARGET_RATE = 16_000   # Whisper requires 16 kHz
    _CHANNELS    = 1
    _FRAME_MS    = 32       # 32 ms frames (512 samples @ 16kHz — Silero VAD minimum)
    _MIN_FRAMES  = 5        # ignore bursts < 160 ms
    _COOLDOWN_S  = 0.3      # quiet period after TTS playback

    # Silero VAD thresholds
    _VAD_THRESHOLD      = 0.45  # speech probability threshold (higher = stricter)
    _VAD_NEG_THRESHOLD  = 0.15  # below this = definitely not speech
    _SILENCE_FRAMES_MAX = 28    # ~900ms of non-speech to end utterance

    # Hallucination patterns — Whisper outputs to discard
    _HALLUCINATION_RE = re.compile(
        r'^\s*[\[\(].*[\]\)]\s*$'           # [BLANK_AUDIO], (wind blowing), etc.
        r'|^\s*\.{1,3}\s*$'                  # just dots
        r'|^\s*$'                             # empty
        r'|^\.+$'                             # all dots
        r'|^\s*Thank you\.?\s*$'             # common Whisper hallucination
        r'|^\s*Thanks for watching\.?\s*$'   # YouTube training artifact
        r'|^\s*you\s*$'                       # just "you"
        r'|^\s*Bye\.?\s*$'                   # often hallucinated from silence
    , re.IGNORECASE)

    def __init__(self) -> None:
        self._thread: threading.Thread | None = None
        self._running = False
        self._muted   = False
        self._debug   = False

    # Public API

    def start(self, debug: bool = False) -> bool:
        """Start the voice loop. Returns False if sounddevice/numpy unavailable."""
        try:
            import sounddevice as sd  # noqa: F401
            import numpy as np        # noqa: F401
        except ImportError:
            logger.warning(
                "sounddevice / numpy not installed -- voice loop disabled.\n"
                "  Install: pip install sounddevice numpy"
            )
            return False
        self._debug   = debug
        self._running = True
        self._thread  = threading.Thread(
            target=self._run, daemon=True, name="kria_voice_loop"
        )
        self._thread.start()
        return True

    def stop(self) -> None:
        self._running = False
        if self._thread:
            self._thread.join(timeout=3.0)

    @property
    def running(self) -> bool:
        return self._running and bool(self._thread and self._thread.is_alive())

    # ── VAD Initialization ─────────────────────────────────────────────────

    def _init_vad(self):
        """Try to initialize Silero VAD, fall back to webrtcvad, then RMS."""
        # Try Silero VAD first (best quality)
        try:
            import torch
            model, utils = torch.hub.load(
                repo_or_dir='snakers4/silero-vad',
                model='silero_vad',
                force_reload=False,
                onnx=False,
                trust_repo=True,
            )
            logger.info("VAD: Silero VAD loaded (torch)")
            return ("silero", model)
        except Exception as e:
            logger.debug("Silero VAD unavailable: %s", e)

        # Try webrtcvad
        try:
            import webrtcvad
            vad = webrtcvad.Vad(2)  # aggressiveness 2 (0-3)
            logger.info("VAD: webrtcvad loaded (aggressiveness=2)")
            return ("webrtcvad", vad)
        except ImportError:
            logger.debug("webrtcvad not available")

        # Fallback: RMS energy gate
        logger.info("VAD: falling back to RMS energy gate (threshold=%.0f)", ENERGY_THRESHOLD)
        return ("rms", None)

    def _is_speech(self, audio_chunk, vad_type: str, vad_model, sample_rate: int) -> bool:
        """Check if an audio chunk contains speech."""
        import numpy as np

        if vad_type == "silero":
            import torch
            # Silero expects float32 tensor, 16kHz
            chunk_f32 = audio_chunk.astype(np.float32) / 32768.0
            tensor = torch.from_numpy(chunk_f32)
            # Silero needs 16kHz input
            if sample_rate != 16000:
                # Quick resample for VAD check
                new_len = int(len(chunk_f32) * 16000 / sample_rate)
                x_old = np.linspace(0, 1, len(chunk_f32))
                x_new = np.linspace(0, 1, new_len)
                chunk_f32 = np.interp(x_new, x_old, chunk_f32).astype(np.float32)
                tensor = torch.from_numpy(chunk_f32)
            prob = vad_model(tensor, sample_rate if sample_rate == 16000 else 16000).item()
            return prob > self._VAD_THRESHOLD

        elif vad_type == "webrtcvad":
            # webrtcvad needs 16-bit PCM at 8/16/32/48 kHz, 10/20/30ms frames
            raw_bytes = audio_chunk.astype(np.int16).tobytes()
            try:
                return vad_model.is_speech(raw_bytes, sample_rate)
            except Exception:
                return False

        else:  # RMS fallback
            rms = self._rms_np(audio_chunk)
            return rms >= ENERGY_THRESHOLD

    def _is_not_speech(self, audio_chunk, vad_type: str, vad_model, sample_rate: int) -> bool:
        """Stronger check that audio is definitely NOT speech (for ending recordings)."""
        import numpy as np

        if vad_type == "silero":
            import torch
            chunk_f32 = audio_chunk.astype(np.float32) / 32768.0
            if sample_rate != 16000:
                new_len = int(len(chunk_f32) * 16000 / sample_rate)
                x_old = np.linspace(0, 1, len(chunk_f32))
                x_new = np.linspace(0, 1, new_len)
                chunk_f32 = np.interp(x_new, x_old, chunk_f32).astype(np.float32)
            tensor = torch.from_numpy(chunk_f32)
            prob = vad_model(tensor, 16000).item()
            return prob < self._VAD_NEG_THRESHOLD
        else:
            return not self._is_speech(audio_chunk, vad_type, vad_model, sample_rate)

    # ── Audio Enhancement ──────────────────────────────────────────────────

    @staticmethod
    def _highpass(audio, sample_rate: int = 16000, cutoff: int = 300):
        """Apply a 4th-order Butterworth high-pass filter to remove low-freq rumble."""
        import numpy as np
        try:
            from scipy.signal import butter, sosfilt
            sos = butter(4, cutoff, btype='high', fs=sample_rate, output='sos')
            audio_f32 = audio.astype(np.float32)
            filtered = sosfilt(sos, audio_f32)
            return filtered.clip(-32768, 32767).astype(np.int16)
        except ImportError:
            logger.debug("scipy not available, skipping high-pass filter")
            return audio
        except Exception as e:
            logger.warning("High-pass filter failed: %s", e)
            return audio

    @staticmethod
    def _agc(audio, target_dbfs: float = -20.0):
        """Automatic Gain Control — normalize audio to target dBFS before STT."""
        import numpy as np
        audio_f32 = audio.astype(np.float32)
        peak = np.max(np.abs(audio_f32))
        if peak < 1.0:
            return audio  # silence — don't amplify noise
        current_dbfs = 20 * np.log10(peak / 32768.0)
        gain_db = target_dbfs - current_dbfs
        # Clamp gain to avoid extreme amplification of quiet noise
        gain_db = min(gain_db, 30.0)
        gain = 10 ** (gain_db / 20.0)
        amplified = audio_f32 * gain
        return amplified.clip(-32768, 32767).astype(np.int16)

    @staticmethod
    def _denoise(audio, sample_rate: int = 16000):
        """Apply noise reduction to audio. Returns enhanced audio."""
        import numpy as np
        try:
            import noisereduce as nr
            audio_f32 = audio.astype(np.float32) / 32768.0
            cleaned = nr.reduce_noise(
                y=audio_f32,
                sr=sample_rate,
                stationary=True,
                prop_decrease=0.3,
                n_fft=1024,
                hop_length=256,
            )
            return (cleaned * 32768).clip(-32768, 32767).astype(np.int16)
        except ImportError:
            logger.debug("noisereduce not available, skipping denoising")
            return audio
        except Exception as e:
            logger.warning("Denoising failed: %s", e)
            return audio

    # Main loop

    def _run(self) -> None:
        import numpy as np
        import sounddevice as sd

        # Initialize VAD
        vad_type, vad_model = self._init_vad()

        # Resolve device
        dev_arg = MIC_DEVICE  # int index or None (sounddevice default)
        device_info = sd.query_devices(dev_arg, kind="input")
        native_rate  = int(device_info["default_samplerate"])
        dev_idx      = device_info["index"]
        logger.info("Microphone [device %d]: %s -- native rate: %d Hz",
                    dev_idx, device_info["name"], native_rate)
        logger.info("VAD: %s | Denoise: %s | HPF: %s | AGC: on | Silence: %dms",
                    vad_type,
                    "noisereduce" if self._has_noisereduce() else "off",
                    "300Hz" if self._has_scipy() else "off",
                    SILENCE_MS)

        frame_samples = int(native_rate * self._FRAME_MS / 1000)
        silence_max   = max(int(SILENCE_MS / self._FRAME_MS), self._SILENCE_FRAMES_MAX)
        max_frames    = int(30_000 / self._FRAME_MS)   # 30-second hard cap

        try:
            with sd.InputStream(
                device     = dev_arg,
                samplerate = native_rate,
                channels   = self._CHANNELS,
                dtype      = "int16",
                blocksize  = frame_samples,
            ) as stream:
                logger.info("Microphone open at %d Hz -- listening...",
                            native_rate)

                _rms_print_counter = 0
                while self._running:
                    # Phase 1: wait for speech (VAD-based)
                    if self._muted:
                        time.sleep(0.05)
                        continue

                    raw_block, _ = stream.read(frame_samples)
                    flat = raw_block.flatten()

                    # Periodic RMS display (debug info)
                    _rms_print_counter += 1
                    if _rms_print_counter >= 10:
                        _rms_print_counter = 0
                        rms = self._rms_np(raw_block)
                        if self._debug:
                            print(f"  mic RMS: {rms:7.1f}  (VAD: {vad_type})", flush=True)

                    # VAD check — is this speech?
                    if not self._is_speech(flat, vad_type, vad_model, native_rate):
                        continue

                    # Phase 2: speech detected -- record full utterance
                    rec_start = time.monotonic()
                    print(f"\r  \033[33m● Listening...\033[0m", end="", flush=True)
                    recorded      = [raw_block.copy()]
                    silence_count = 0
                    frame_count   = 1
                    _rec_tick     = 0

                    # Reset Silero VAD state for clean recording
                    if vad_type == "silero":
                        vad_model.reset_states()

                    while frame_count < max_frames and self._running:
                        block, _ = stream.read(frame_samples)
                        frame_count += 1
                        _rec_tick += 1
                        recorded.append(block.copy())

                        # Show live recording duration every ~300ms
                        if _rec_tick % 10 == 0:
                            elapsed = time.monotonic() - rec_start
                            print(f"\r  \033[33m● Listening... {elapsed:.1f}s\033[0m  ",
                                  end="", flush=True)

                        # Check if speech ended
                        flat_block = block.flatten()
                        if self._is_not_speech(flat_block, vad_type, vad_model, native_rate):
                            silence_count += 1
                            if silence_count >= silence_max:
                                break
                        else:
                            silence_count = 0

                    rec_dur = time.monotonic() - rec_start
                    if len(recorded) < self._MIN_FRAMES or not self._running:
                        print(f"\r  (too short, discarded)           ", flush=True)
                        continue

                    # Concatenate, resample to 16 kHz
                    audio = np.concatenate(recorded, axis=0).flatten()
                    if native_rate != self._TARGET_RATE:
                        audio = self._resample(audio, native_rate, self._TARGET_RATE)

                    # Phase 2.5: Audio enhancement pipeline
                    # 1. High-pass filter (remove <300Hz rumble/hum)
                    audio = self._highpass(audio, self._TARGET_RATE, cutoff=300)
                    # 2. Noise reduction
                    audio = self._denoise(audio, self._TARGET_RATE)

                    # Post-denoise energy check — reject if denoised audio is too quiet
                    # (catches ambient noise that VAD let through)
                    # Must check BEFORE AGC — AGC can reduce levels if peaks are high
                    denoised_rms = self._rms_np(audio)
                    if denoised_rms < 150:
                        if self._debug:
                            print(f"\r  (post-denoise RMS {denoised_rms:.0f} too low, skipped)",
                                  flush=True)
                        else:
                            print(f"\r  (noise, skipped)                       ", flush=True)
                        continue

                    # 3. AGC normalization to -20 dBFS
                    audio = self._agc(audio, target_dbfs=-20.0)

                    wav_bytes = self._to_wav(audio)

                    # Phase 3: Whisper STT
                    print(f"\r  \033[36m⟳ Processing {rec_dur:.1f}s of audio...\033[0m   ",
                          end="", flush=True)
                    t0 = time.monotonic()
                    transcript = self._transcribe(wav_bytes)
                    stt_dur = time.monotonic() - t0

                    if not transcript:
                        print(f"\r  (no speech detected, STT took {stt_dur:.1f}s)          ",
                              flush=True)
                        continue

                    # Hallucination filter
                    if self._HALLUCINATION_RE.match(transcript):
                        if self._debug:
                            print(f"\r  (hallucination filtered: {transcript!r}, {stt_dur:.1f}s)",
                                  flush=True)
                        else:
                            print(f"\r  (noise filtered, {stt_dur:.1f}s)                       ",
                                  flush=True)
                        continue

                    # Show what the user said
                    print(f"\r  \033[1;32m🎤 You:\033[0m {transcript}" +
                          f"  \033[2m({stt_dur:.1f}s)\033[0m", flush=True)

                    # Phase 4: send to kria-core
                    print(f"  \033[1;35m⏳ Thinking...\033[0m", end="", flush=True)
                    t1 = time.monotonic()
                    response = self._chat(transcript)
                    llm_dur = time.monotonic() - t1
                    if not response:
                        print(f" (empty response, {llm_dur:.1f}s)", flush=True)
                        continue

                    print(f"\r  \033[1;34m🤖 KRIA:\033[0m {response}" +
                          f"  \033[2m({llm_dur:.1f}s)\033[0m\n", flush=True)

                    # Phase 5: TTS + playback
                    self._speak(response)
                    time.sleep(self._COOLDOWN_S)

        except Exception as exc:
            logger.error("Voice loop crashed: %s", exc, exc_info=True)
        finally:
            self._running = False
            logger.info("Voice loop stopped")

    # Helpers

    @staticmethod
    def _has_noisereduce() -> bool:
        try:
            import noisereduce  # noqa: F401
            return True
        except ImportError:
            return False

    @staticmethod
    def _has_scipy() -> bool:
        try:
            from scipy.signal import butter  # noqa: F401
            return True
        except ImportError:
            return False

    # Helpers

    @staticmethod
    def _rms_np(block) -> float:
        import numpy as np
        arr = np.asarray(block, dtype=np.float32).flatten()
        if len(arr) == 0:
            return 0.0
        return float(np.sqrt(np.mean(arr ** 2)))

    @staticmethod
    def _resample(audio, src_rate: int, dst_rate: int):
        """Linear interpolation resample -- no scipy needed."""
        import numpy as np
        if src_rate == dst_rate:
            return audio
        new_length = int(len(audio) * dst_rate / src_rate)
        x_old = np.linspace(0, 1, len(audio))
        x_new = np.linspace(0, 1, new_length)
        return np.interp(x_new, x_old, audio.astype(np.float64)).astype(np.int16)

    @staticmethod
    def _to_wav(audio) -> bytes:
        import numpy as np
        buf = io.BytesIO()
        with wave.open(buf, "wb") as wf:
            wf.setnchannels(1)
            wf.setsampwidth(2)
            wf.setframerate(VoiceLoop._TARGET_RATE)
            wf.writeframes(np.asarray(audio, dtype=np.int16).tobytes())
        return buf.getvalue()

    @staticmethod
    def _transcribe(wav_bytes: bytes) -> str:
        """Send audio to whisper.cpp and return transcript text."""
        try:
            import httpx
            with httpx.Client(timeout=90.0) as client:
                resp = client.post(
                    f"{WHISPER_URL}/inference",
                    files={"file": ("audio.wav", wav_bytes, "audio/wav")},
                    data={
                        "language": "en",
                        "response_format": "json",
                        "temperature": "0.0",
                        "beam_size": "5",
                        "initial_prompt": (
                            "KRIA is a voice assistant. The user speaks English "
                            "commands like: open Chrome, open WhatsApp, play music, "
                            "what time is it, set a timer, search for, tell me about."
                        ),
                    },
                )
                resp.raise_for_status()
                return resp.json().get("text", "").strip()
        except Exception as exc:
            logger.warning("Whisper STT failed: %s", exc)
            return ""

    @staticmethod
    def _chat(text: str) -> str:
        """Send text command to kria-core agent and return response."""
        try:
            import httpx
            with httpx.Client(timeout=120.0) as client:
                resp = client.post(
                    f"{CORE_URL}/api/v1/chat",
                    json={"message": text, "session_id": VOICE_SESSION},
                )
                resp.raise_for_status()
                return resp.json().get("response", "")
        except Exception as exc:
            logger.warning("kria-core /chat failed: %s", exc)
            return ""

    def _speak(self, text: str) -> None:
        """Synthesize via Piper TTS and play through speakers."""
        self._muted = True
        try:
            import httpx
            with httpx.Client(timeout=30.0) as client:
                resp = client.post(f"{PIPER_URL}/synthesize",
                                   json={"text": text, "voice": TTS_VOICE})
                resp.raise_for_status()
                self._play_wav(resp.content)
        except Exception as exc:
            logger.warning("TTS/playback failed: %s -- no audio output", exc)
        finally:
            self._muted = False

    @staticmethod
    def _play_wav(wav_bytes: bytes) -> None:
        """Play WAV bytes through the configured speaker device.
        Always resamples to the device's native rate before playback."""
        if not wav_bytes:
            return
        try:
            import numpy as np
            import sounddevice as sd
            import soundfile as sf

            data, src_rate = sf.read(io.BytesIO(wav_bytes))

            # Query the target device's native sample rate
            out_info = sd.query_devices(SPEAKER_DEVICE, kind='output')
            out_rate = int(out_info['default_samplerate'])

            # Resample to the device's native rate (Piper outputs 22050 Hz)
            if src_rate != out_rate:
                old_len = len(data)
                new_len = int(old_len * out_rate / src_rate)
                x_old = np.linspace(0, 1, old_len)
                x_new = np.linspace(0, 1, new_len)
                if data.ndim == 1:
                    data = np.interp(x_new, x_old, data)
                else:
                    data = np.column_stack([
                        np.interp(x_new, x_old, data[:, ch])
                        for ch in range(data.shape[1])
                    ])

            sd.play(data.astype(np.float32), out_rate, device=SPEAKER_DEVICE)
            sd.wait()
            return
        except ImportError:
            pass
        except Exception as exc:
            logger.warning("sounddevice playback error: %s", exc)
        # OS fallback
        try:
            import tempfile
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
                f.write(wav_bytes)
                tmp_path = f.name
            if sys.platform == "win32":
                subprocess.run(
                    ["powershell", "-Command",
                     f"(New-Object Media.SoundPlayer '{tmp_path}').PlaySync()"],
                    capture_output=True,
                )
            else:
                subprocess.run(["aplay", tmp_path], capture_output=True)
        except Exception as exc:
            logger.warning("Fallback audio playback failed: %s", exc)

voice_loop = VoiceLoop()


# ══════════════════════════════════════════════════════════════════════════════
# HTTP Bridge Server
# ══════════════════════════════════════════════════════════════════════════════

class BridgeHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        logger.debug(fmt, *args)   # demote access logs to DEBUG

    def _send(self, code: int, body: dict) -> None:
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            self._send(200, {
                "status": "ok",
                "platform": sys.platform,
                "voice": voice_loop.running,
            })
        elif self.path == "/voice/status":
            self._send(200, {
                "running":          voice_loop.running,
                "muted":            voice_loop._muted,
                "energy_threshold": ENERGY_THRESHOLD,
                "silence_ms":       SILENCE_MS,
                "wake_keyword":     WAKE_KEYWORD,
            })
        else:
            self._send(404, {"error": "Not found"})

    def do_POST(self):
        # Voice control (no auth — localhost only, low-risk)
        if self.path == "/voice/start":
            if not voice_loop.running:
                voice_loop.start()
            self._send(200, {"running": voice_loop.running})
            return
        if self.path == "/voice/stop":
            voice_loop.stop()
            self._send(200, {"running": False})
            return

        # All other POST endpoints require the bridge secret
        auth = self.headers.get("X-Bridge-Secret", "")
        if not _verify(auth):
            self._send(401, {"error": "Unauthorized"})
            return

        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            req = json.loads(raw)
        except json.JSONDecodeError:
            self._send(400, {"error": "Invalid JSON"})
            return

        handler = {
            "/audio/play": self._play_audio,
            "/notify":     self._notify,
            "/exec":       self._exec,
        }.get(self.path)

        if handler is None:
            self._send(404, {"error": f"Unknown endpoint: {self.path}"})
            return

        self._send(200, handler(req))

    # ── Handlers ─────────────────────────────────────────────────────────────

    def _play_audio(self, req: dict) -> dict:
        wav_b64 = req.get("wav_base64", "")
        if not wav_b64:
            return {"error": "Missing wav_base64"}
        try:
            VoiceLoop._play_wav(base64.b64decode(wav_b64))
            return {"played": True}
        except Exception as exc:
            return {"played": False, "error": str(exc)}

    def _notify(self, req: dict) -> dict:
        title   = req.get("title",   "K.R.I.A.")
        message = req.get("message", "")
        try:
            if sys.platform == "win32":
                from win10toast import ToastNotifier
                ToastNotifier().show_toast(title, message, duration=5, threaded=True)
            return {"notified": True}
        except Exception as exc:
            return {"notified": False, "error": str(exc)}

    def _exec(self, req: dict) -> dict:
        """Execute a pre-approved command on the host. Restricted to allowlist."""
        cmd  = req.get("command", "")
        ALLOWED = {"explorer", "notepad", "calc", "mspaint"}
        base = cmd.split()[0].lower() if cmd else ""
        if base not in ALLOWED:
            return {"error": f"Command not in allowlist: {base!r}"}
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
        return {"exit_code": result.returncode, "output": result.stdout[:2048]}


# ══════════════════════════════════════════════════════════════════════════════
# Entrypoint
# ══════════════════════════════════════════════════════════════════════════════

def _list_devices() -> None:
    """List all input devices with their ambient RMS level."""
    import numpy as np
    import sounddevice as sd

    devices = sd.query_devices()
    print(f"{'Idx':>4}  {'Name':<50}  {'Rate':>6}  {'RMS (0.5s)':>12}")
    print("-" * 80)
    for i, d in enumerate(devices):
        if d["max_input_channels"] < 1:
            continue
        rate = int(d["default_samplerate"])
        try:
            with sd.InputStream(device=i, samplerate=rate, channels=1,
                                dtype="int16", blocksize=int(rate * 0.1)) as s:
                data, _ = s.read(int(rate * 0.5))
                arr = np.asarray(data, dtype=np.float32).flatten()
                rms = float(np.sqrt(np.mean(arr ** 2)))
            print(f"{i:>4}  {d['name']:<50}  {rate:>6}  {rms:>12.1f}")
        except Exception as e:
            print(f"{i:>4}  {d['name']:<50}  {rate:>6}  {'ERROR':>12}  ({e})")
    print()
    print("Set KRIA_MIC_DEVICE=<Idx> or use --device <Idx> to select the right device.")


def _mic_test(device_idx: int = -1) -> None:
    """Record 4 seconds, show peak RMS, then transcribe with Whisper."""
    import numpy as np
    import sounddevice as sd

    dev_arg     = device_idx if device_idx >= 0 else MIC_DEVICE
    device_info = sd.query_devices(dev_arg, kind="input")
    native_rate  = int(device_info["default_samplerate"])
    duration     = 4  # seconds
    dev_label    = f"device {dev_arg}" if dev_arg is not None else "default device"
    print(f"Mic test ({dev_label}): recording {duration}s from '{device_info['name']}' "
          f"at {native_rate} Hz...")
    print("Speak NOW!")
    audio = sd.rec(int(duration * native_rate), samplerate=native_rate,
                   channels=1, dtype="int16", device=dev_arg)
    sd.wait()
    arr   = audio.flatten().astype(np.float32)
    peak  = float(np.max(np.abs(arr)))
    rms   = float(np.sqrt(np.mean(arr ** 2)))
    print(f"  Peak: {peak:.0f}   RMS: {rms:.1f}   (KRIA_ENERGY_THRESHOLD={ENERGY_THRESHOLD}")
    if rms < ENERGY_THRESHOLD:
        print(f"  WARNING: RMS ({rms:.0f}) is below threshold ({ENERGY_THRESHOLD}).")
        suggested = max(50.0, rms * 0.5)
        print(f"  Try: KRIA_ENERGY_THRESHOLD={suggested:.0f} python3 scripts/kria_bridge.py")
    else:
        print("  Mic level OK.")
    print("  Sending to Whisper STT...")
    # Resample to 16 kHz for Whisper
    if native_rate != 16000:
        new_len = int(len(arr) * 16000 / native_rate)
        x_old = np.linspace(0, 1, len(arr))
        x_new = np.linspace(0, 1, new_len)
        arr   = np.interp(x_new, x_old, arr.astype(np.float64)).astype(np.int16)
    else:
        arr = arr.astype(np.int16)
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(1); wf.setsampwidth(2); wf.setframerate(16000)
        wf.writeframes(arr.tobytes())
    transcript = VoiceLoop._transcribe(buf.getvalue())
    print(f"  Whisper transcript: {transcript!r}")
    if not transcript:
        print("  WARNING: Whisper returned empty. Check kria-brain is running (port 8081).")
    elif "kria" not in transcript.lower():
        print(f"  NOTE: 'kria' not found. Whisper heard {transcript!r}.")
        print(f"  Try saying 'Kria', 'KRIA', 'Cree-ah', or set KRIA_WAKE_KEYWORD to a word Whisper does recognize.")
    else:
        print("  Wake word detected OK!")


if __name__ == "__main__":
    # Parse --device N  (overrides KRIA_MIC_DEVICE for this run — numeric index only)
    _cli_device = -1
    if "--device" in sys.argv:
        _d_idx = sys.argv.index("--device")
        if _d_idx + 1 < len(sys.argv):
            try:
                _cli_device = int(sys.argv[_d_idx + 1])
                MIC_DEVICE  = _cli_device
            except ValueError:
                pass

    if "--list-devices" in sys.argv:
        _list_devices()
        sys.exit(0)

    if "--mic-test" in sys.argv:
        _mic_test(_cli_device)
        sys.exit(0)

    no_voice = "--no-voice" in sys.argv
    debug    = "--debug"    in sys.argv

    if not no_voice:
        started = voice_loop.start(debug=debug)
        if not started:
            logger.warning("Bridge starting without voice loop (sounddevice unavailable).")
            logger.warning("  Install: pip install sounddevice numpy")
        elif debug:
            logger.info("DEBUG mode: live RMS printed, all Whisper transcripts shown")
    else:
        logger.info("Voice loop disabled via --no-voice flag")

    logger.info("K.R.I.A. Bridge starting on http://127.0.0.1:%d", PORT)
    server = HTTPServer(("127.0.0.1", PORT), BridgeHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        logger.info("Bridge stopping...")
    finally:
        voice_loop.stop()
        logger.info("Bridge stopped")
