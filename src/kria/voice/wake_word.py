"""
Wake Word Detector
==================
Listens continuously on a background thread using PyAudio (or a simple
energy-based fallback) and signals the pipeline when the configured wake
phrase is detected.

Two modes:
  1. Porcupine (Picovoice) — high accuracy, requires access key env var
     KRIA_PORCUPINE_ACCESS_KEY.  If the key is set and pvporcupine is
     installed, this mode is used automatically.
  2. Energy threshold fallback — detects when microphone input exceeds a
     configurable RMS threshold and treats any audio chunk as a potential
     wake event.  Less precise but zero-dependency.

The detector exposes an asyncio.Event that the voice pipeline awaits.
It runs its I/O loop in a dedicated thread to avoid blocking the event loop.
"""
import asyncio
import logging
import threading
from typing import Optional

from kria.infra.config import settings

logger = logging.getLogger("kria.voice.wake_word")

# Wake phrase (lower-case, used for energy-mode label only)
WAKE_PHRASE = settings.wake_word.lower()


class WakeWordDetector:
    def __init__(self) -> None:
        self._event: asyncio.Event = asyncio.Event()
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._thread: Optional[threading.Thread] = None
        self._running = False
        self._mode = "none"

    # ── Public API ────────────────────────────────────────────────

    async def start(self) -> None:
        """Start the background detector thread."""
        self._loop = asyncio.get_running_loop()
        self._running = True

        if self._try_porcupine():
            self._mode = "porcupine"
            self._thread = threading.Thread(target=self._run_porcupine, daemon=True)
        else:
            self._mode = "energy"
            logger.info("Wake-word: using energy-threshold fallback mode")
            self._thread = threading.Thread(target=self._run_energy, daemon=True)

        self._thread.start()
        logger.info("Wake-word detector started (mode=%s, phrase=%r)", self._mode, WAKE_PHRASE)

    async def stop(self) -> None:
        self._running = False
        self._event.set()  # unblock any awaiter

    async def wait_for_wake(self) -> None:
        """Suspend until a wake event is signalled."""
        self._event.clear()
        await self._event.wait()

    def signal(self) -> None:
        """Manually trigger a wake event (useful for testing / hotkey)."""
        if self._loop:
            self._loop.call_soon_threadsafe(self._event.set)

    # ── Porcupine path ────────────────────────────────────────────

    def _try_porcupine(self) -> bool:
        try:
            import pvporcupine  # type: ignore
            access_key = settings.porcupine_access_key
            if not access_key:
                return False
            self._porcupine = pvporcupine.create(
                access_key=access_key,
                keywords=["computer", "hey siri", "jarvis", "ok google"],
            )
            return True
        except Exception:
            return False

    def _run_porcupine(self) -> None:
        try:
            import pyaudio  # type: ignore
            pa = pyaudio.PyAudio()
            stream = pa.open(
                rate=self._porcupine.sample_rate,
                channels=1,
                format=pyaudio.paInt16,
                input=True,
                frames_per_buffer=self._porcupine.frame_length,
            )
            while self._running:
                pcm = stream.read(self._porcupine.frame_length, exception_on_overflow=False)
                pcm_array = [int.from_bytes(pcm[i:i+2], "little", signed=True) for i in range(0, len(pcm), 2)]
                idx = self._porcupine.process(pcm_array)
                if idx >= 0:
                    logger.debug("Porcupine wake word detected (keyword_index=%d)", idx)
                    if self._loop:
                        self._loop.call_soon_threadsafe(self._event.set)
            stream.close()
            pa.terminate()
        except Exception as exc:
            logger.error("Porcupine detector crashed: %s", exc)

    # ── Energy threshold fallback ─────────────────────────────────

    def _run_energy(self) -> None:
        """Simple VAD: trigger when RMS energy crosses threshold for 0.5s."""
        try:
            import pyaudio  # type: ignore
            import struct, math
            CHUNK = 1024
            RATE = 16000
            THRESHOLD = settings.wake_energy_threshold  # default in config
            pa = pyaudio.PyAudio()
            stream = pa.open(rate=RATE, channels=1, format=pyaudio.paInt16,
                             input=True, frames_per_buffer=CHUNK)
            above_count = 0
            REQUIRED_CHUNKS = 8  # ~0.5 s at 16kHz/1024
            while self._running:
                raw = stream.read(CHUNK, exception_on_overflow=False)
                samples = struct.unpack(f"{CHUNK}h", raw)
                rms = math.sqrt(sum(s*s for s in samples) / CHUNK)
                if rms > THRESHOLD:
                    above_count += 1
                    if above_count >= REQUIRED_CHUNKS:
                        logger.debug("Energy threshold exceeded — wake triggered (rms=%.1f)", rms)
                        if self._loop:
                            self._loop.call_soon_threadsafe(self._event.set)
                        above_count = 0
                else:
                    above_count = max(0, above_count - 1)
            stream.close()
            pa.terminate()
        except ImportError:
            logger.warning("PyAudio not installed — wake word detection disabled. "
                           "Voice pipeline will run in push-to-talk mode only.")
        except Exception as exc:
            logger.error("Energy detector crashed: %s", exc)


wake_word_detector = WakeWordDetector()
