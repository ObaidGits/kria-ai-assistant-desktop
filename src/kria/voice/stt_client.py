"""
STT (Speech-to-Text) Client
============================
HTTP client for whisper.cpp server running on port 8081.
Accepts raw PCM/WAV audio bytes and returns a transcription string.

The endpoint is POST /inference with form-data containing the audio file.
Falls back to an empty string (never raises into the voice pipeline).
"""
import logging
from pathlib import Path
from typing import Optional

import httpx

from kria.infra.circuit_breaker import CircuitBreaker
from kria.infra.config import settings
from kria.infra.health import ServiceStatus, health_registry

logger = logging.getLogger("kria.voice.stt")

_breaker = CircuitBreaker(name="whisper", failure_threshold=3, recovery_timeout=30.0)


class STTClient:
    def __init__(self) -> None:
        self._base_url = settings.whisper_url

    async def transcribe_bytes(
        self,
        audio_bytes: bytes,
        filename: str = "audio.wav",
        language: str = "auto",
    ) -> str:
        """
        Send audio bytes to whisper.cpp. Returns transcript or "" on failure.
        """
        async def _call() -> str:
            async with httpx.AsyncClient(timeout=30.0) as client:
                files = {"file": (filename, audio_bytes, "audio/wav")}
                data = {"language": language}
                resp = await client.post(f"{self._base_url}/inference", files=files, data=data)
                resp.raise_for_status()
                result = resp.json()
                text = result.get("text", "").strip()
                # Filter whisper.cpp blank-audio tokens
                if text in ("[BLANK_AUDIO]", "(blank audio)", "[silence]"):
                    text = ""
                health_registry.update("whisper", ServiceStatus.HEALTHY)
                return text

        try:
            result = await _breaker.call(_call)
            return result if isinstance(result, str) else ""
        except Exception as exc:
            health_registry.update("whisper", ServiceStatus.DEGRADED, str(exc))
            logger.warning("STT transcription failed: %s", exc)
            return ""

    async def transcribe_file(self, path: str | Path, language: str = "auto") -> str:
        """Convenience wrapper — reads a file then calls transcribe_bytes."""
        p = Path(path)
        if not p.exists():
            logger.error("Audio file not found: %s", path)
            return ""
        return await self.transcribe_bytes(p.read_bytes(), filename=p.name, language=language)

    async def health_check(self) -> bool:
        """Quick ping to the whisper.cpp server."""
        try:
            async with httpx.AsyncClient(timeout=5.0) as client:
                resp = await client.get(f"{self._base_url}/health")
                return resp.status_code == 200
        except Exception:
            return False


stt_client = STTClient()
