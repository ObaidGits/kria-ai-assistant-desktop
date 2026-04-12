"""
Piper TTS HTTP Server
=====================
Minimal FastAPI wrapper around Piper TTS for use in the kria-voice container.
Exposes POST /synthesize → WAV audio bytes.
"""
import io
import os
import wave
from pathlib import Path

import uvicorn
from fastapi import FastAPI, HTTPException
from fastapi.responses import Response
from pydantic import BaseModel

app = FastAPI(title="Piper TTS Server")

MODEL_PATH = Path(os.getenv("PIPER_MODEL_PATH", "/models/piper"))
DEFAULT_VOICE = os.getenv("PIPER_DEFAULT_VOICE", "")  # e.g. "en_US-lessac-high"
PORT = int(os.getenv("PORT", "8082"))

_voice_cache: dict = {}


def _get_voice(voice_name: str = ""):
    from piper.voice import PiperVoice
    # Priority: explicit request → env default → first available
    resolved = voice_name or DEFAULT_VOICE
    key = resolved or "default"
    if key not in _voice_cache:
        if resolved:
            model_file = MODEL_PATH / f"{resolved}.onnx"
        else:
            # Use first available model
            models = list(MODEL_PATH.glob("*.onnx"))
            if not models:
                raise RuntimeError(f"No .onnx model files found in {MODEL_PATH}")
            model_file = models[0]
        _voice_cache[key] = PiperVoice.load(str(model_file))
    return _voice_cache[key]


class SynthesizeRequest(BaseModel):
    text: str
    voice: str = ""
    speed: float = 1.0


@app.get("/health")
def health():
    return {"status": "ok"}


@app.post("/synthesize")
def synthesize(req: SynthesizeRequest):
    try:
        voice = _get_voice(req.voice)
        buf = io.BytesIO()
        with wave.open(buf, "wb") as wf:
            wf.setnchannels(1)
            wf.setsampwidth(2)  # 16-bit
            wf.setframerate(voice.config.sample_rate)
            for chunk in voice.synthesize(req.text):
                wf.writeframes(chunk.audio_int16_bytes)
        return Response(content=buf.getvalue(), media_type="audio/wav")
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc))


@app.post("/api/tts")
def api_tts(req: SynthesizeRequest):
    """Alias for compatibility."""
    return synthesize(req)


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=PORT)
