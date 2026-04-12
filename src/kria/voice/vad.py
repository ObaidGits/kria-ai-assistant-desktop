"""
Voice Activity Detection (VAD)
================================
Records audio from the microphone until the user stops speaking (silence
longer than ``settings.vad_silence_ms`` ms), then returns the complete
PCM/WAV bytes for STT processing.

Uses WebRTC VAD (webrtcvad) when available; falls back to simple energy VAD.

Output format: 16-bit PCM, 16 kHz, mono (whisper.cpp native format).
"""
import io
import logging
import struct
import wave
from typing import Optional

from kria.infra.config import settings

logger = logging.getLogger("kria.voice.vad")

_RATE = 16000
_CHANNELS = 1
_SAMPLE_WIDTH = 2  # int16
_FRAME_DURATION_MS = 30  # WebRTC VAD frame size options: 10, 20, 30 ms
_FRAME_BYTES = int(_RATE * _FRAME_DURATION_MS / 1000) * _SAMPLE_WIDTH * _CHANNELS


def _frames_to_wav(frames: list[bytes]) -> bytes:
    """Pack raw PCM frames into a valid WAV container."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(_CHANNELS)
        wf.setsampwidth(_SAMPLE_WIDTH)
        wf.setframerate(_RATE)
        wf.writeframes(b"".join(frames))
    return buf.getvalue()


class VADRecorder:
    """
    Open the microphone, wait for speech, record until silence, return WAV.
    All I/O is synchronous (run in a thread via asyncio.to_thread).
    """

    async def record_utterance(
        self,
        max_seconds: float = 30.0,
        silence_ms: Optional[int] = None,
    ) -> bytes:
        """
        Async entry point.  Runs the blocking microphone capture on a
        thread-pool executor so the event loop stays free.
        """
        import asyncio
        silence_ms = silence_ms or settings.vad_silence_ms
        return await asyncio.to_thread(
            self._blocking_record, max_seconds, silence_ms
        )

    def _blocking_record(self, max_seconds: float, silence_ms: int) -> bytes:
        try:
            import pyaudio  # type: ignore
        except ImportError:
            logger.warning("PyAudio not installed — cannot record audio")
            return b""

        use_webrtc = False
        vad = None
        try:
            import webrtcvad  # type: ignore
            vad = webrtcvad.Vad(2)  # aggressiveness 0-3
            use_webrtc = True
        except ImportError:
            logger.debug("webrtcvad not available — using energy VAD")

        silence_frames_required = int(silence_ms / _FRAME_DURATION_MS)
        max_frames = int(max_seconds * 1000 / _FRAME_DURATION_MS)

        pa = pyaudio.PyAudio()
        stream = pa.open(
            rate=_RATE,
            channels=_CHANNELS,
            format=pyaudio.paInt16,
            input=True,
            frames_per_buffer=int(_FRAME_BYTES / _SAMPLE_WIDTH),
        )

        recorded: list[bytes] = []
        silence_count = 0
        speech_started = False
        frame_count = 0

        logger.debug("VAD: listening… (max=%.1fs, silence_ms=%d)", max_seconds, silence_ms)

        try:
            while frame_count < max_frames:
                raw = stream.read(int(_FRAME_BYTES / _SAMPLE_WIDTH), exception_on_overflow=False)
                frame_count += 1

                is_speech = self._is_speech(raw, vad, use_webrtc)

                if is_speech:
                    silence_count = 0
                    speech_started = True
                    recorded.append(raw)
                else:
                    if speech_started:
                        silence_count += 1
                        recorded.append(raw)
                        if silence_count >= silence_frames_required:
                            logger.debug("VAD: silence detected — end of utterance (%d frames)", len(recorded))
                            break
        finally:
            stream.stop_stream()
            stream.close()
            pa.terminate()

        if not recorded:
            return b""
        return _frames_to_wav(recorded)

    @staticmethod
    def _is_speech(raw: bytes, vad, use_webrtc: bool) -> bool:
        if use_webrtc and vad:
            try:
                return vad.is_speech(raw, _RATE)
            except Exception:
                pass
        # Energy fallback
        samples = struct.unpack(f"{len(raw) // 2}h", raw)
        rms = (sum(s * s for s in samples) / len(samples)) ** 0.5
        return rms > settings.wake_energy_threshold


vad_recorder = VADRecorder()
