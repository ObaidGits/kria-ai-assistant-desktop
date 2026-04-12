"""
TTS (Text-to-Speech) Client
============================
HTTP client for Piper TTS server on port 8082.
Returns raw WAV bytes that are played back by the pipeline.

The Piper server endpoint is POST /synthesize (or /api/tts depending
on the wrapper used). We support both style endpoints with auto-detection.
Falls back gracefully — never raises into the pipeline.
"""
import asyncio
import logging
from typing import Optional

import httpx

from kria.infra.circuit_breaker import CircuitBreaker
from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.voice.tts")

_breaker = CircuitBreaker(name="piper_tts", failure_threshold=3, recovery_timeout=30.0)


class TTSClient:
    def __init__(self) -> None:
        self._base_url = settings.piper_url
        # Probe result cached: None = not yet probed, str = working endpoint path
        self._endpoint: Optional[str] = None

    async def _probe_endpoint(self) -> str:
        """Auto-detect whether the server uses /synthesize or /api/tts."""
        for path in ["/synthesize", "/api/tts"]:
            try:
                async with httpx.AsyncClient(timeout=3.0) as client:
                    resp = await client.get(f"{self._base_url}{path}")
                    # 405 Method Not Allowed also confirms the path exists
                    if resp.status_code in (200, 405, 400):
                        return path
            except Exception:
                pass
        return "/synthesize"  # default

    async def synthesize(self, text: str, voice: str = "", speed: float = 1.0) -> bytes:
        """
        Convert *text* to speech. Returns WAV bytes or b"" on failure.
        """
        if not self._endpoint:
            self._endpoint = await self._probe_endpoint()

        async def _call() -> bytes:
            payload = {
                "text": text,
                "speed": speed,
            }
            if voice:
                payload["voice"] = voice
            async with httpx.AsyncClient(timeout=20.0) as client:
                resp = await client.post(f"{self._base_url}{self._endpoint}", json=payload)
                resp.raise_for_status()
                health_registry.update("piper_tts", ServiceStatus.HEALTHY)
                return resp.content  # raw WAV bytes

        try:
            result = await _breaker.call(_call)
            return result if isinstance(result, bytes) else b""
        except Exception as exc:
            health_registry.update("piper_tts", ServiceStatus.DEGRADED, str(exc))
            logger.warning("TTS synthesis failed: %s", exc)
            return b""

    async def play_bytes(self, wav_bytes: bytes) -> None:
        """
        Play WAV bytes through the default audio device.
        Tries sounddevice then falls back to soundfile + simpleaudio.
        """
        if not wav_bytes:
            return
        try:
            import io
            import sounddevice as sd
            import soundfile as sf
            data, samplerate = sf.read(io.BytesIO(wav_bytes))
            sd.play(data, samplerate)
            sd.wait()
        except ImportError:
            # Fallback: write to temp file and use OS player
            import tempfile
            import subprocess
            import platform
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as f:
                f.write(wav_bytes)
                tmp_path = f.name
            if platform.system() == "Windows":
                subprocess.run(["powershell", "-Command", f"(New-Object Media.SoundPlayer '{tmp_path}').PlaySync()"],
                               capture_output=True)
            else:
                subprocess.run(["aplay", tmp_path], capture_output=True)
        except Exception as exc:
            logger.warning("Audio playback failed: %s", exc)

    async def speak(self, text: str, voice: str = "", speed: float = 1.0) -> None:
        """Synthesize and immediately play. Convenience method."""
        wav = await self.synthesize(text, voice=voice, speed=speed)
        await self.play_bytes(wav)

    async def health_check(self) -> bool:
        try:
            async with httpx.AsyncClient(timeout=5.0) as client:
                resp = await client.get(f"{self._base_url}/health")
                return resp.status_code == 200
        except Exception:
            return False


tts_client = TTSClient()
