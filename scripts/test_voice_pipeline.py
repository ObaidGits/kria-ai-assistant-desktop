"""
End-to-end voice pipeline test
===============================
Records audio from mic → Whisper STT → LLM → Piper TTS → plays audio.
Run: python scripts/test_voice_pipeline.py
"""
import io
import sys
import wave
import time
import numpy as np
import sounddevice as sd
import httpx

MIC_DEVICE = 9            # sof-hda-dsp hw:2,7
SAMPLE_RATE = 16000
RECORD_SECONDS = 5
WHISPER_URL = "http://127.0.0.1:8081/inference"
LLM_URL = "http://127.0.0.1:8080/v1/chat/completions"
TTS_URL = "http://127.0.0.1:8082/synthesize"
CHAT_URL = "http://127.0.0.1:8000/api/v1/chat"


def record_audio() -> bytes:
    """Record from mic and return WAV bytes."""
    print(f"🎙️  Recording {RECORD_SECONDS}s from device {MIC_DEVICE}...")
    audio = sd.rec(
        int(SAMPLE_RATE * RECORD_SECONDS),
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="int16",
        device=MIC_DEVICE,
    )
    sd.wait()
    rms = np.sqrt(np.mean(audio.astype(np.float32) ** 2))
    print(f"   RMS level: {rms:.1f}  (should be >100 for speech)")

    buf = io.BytesIO()
    with wave.open(buf, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(audio.tobytes())
    return buf.getvalue()


def transcribe(wav_bytes: bytes) -> str:
    """Send WAV to Whisper and return transcript."""
    print("📝 Transcribing with Whisper...")
    t0 = time.time()
    resp = httpx.post(
        WHISPER_URL,
        files={"file": ("audio.wav", wav_bytes, "audio/wav")},
        data={"response_format": "json", "temperature": "0.0"},
        timeout=30.0,
    )
    resp.raise_for_status()
    elapsed = time.time() - t0
    text = resp.json().get("text", "").strip()
    print(f"   Transcript ({elapsed:.2f}s): \"{text}\"")
    return text


def ask_llm(text: str) -> str:
    """Send text to KRIA chat API and return response."""
    print(f"🤖 Asking LLM: \"{text}\"")
    t0 = time.time()
    resp = httpx.post(
        CHAT_URL,
        json={"message": text, "session_id": "voice_test"},
        timeout=30.0,
    )
    resp.raise_for_status()
    elapsed = time.time() - t0
    reply = resp.json().get("response", "")
    print(f"   Reply ({elapsed:.2f}s): \"{reply}\"")
    return reply


def synthesize(text: str) -> bytes:
    """Send text to Piper TTS and return WAV bytes."""
    print(f"🔊 Synthesizing TTS...")
    t0 = time.time()
    resp = httpx.post(
        TTS_URL,
        json={"text": text},
        timeout=30.0,
    )
    resp.raise_for_status()
    elapsed = time.time() - t0
    wav_bytes = resp.content
    print(f"   TTS audio: {len(wav_bytes)} bytes ({elapsed:.2f}s)")
    return wav_bytes


def play_wav(wav_bytes: bytes):
    """Play WAV bytes through default output, resampling if needed."""
    from scipy.signal import resample_poly
    buf = io.BytesIO(wav_bytes)
    with wave.open(buf, "rb") as wf:
        rate = wf.getframerate()
        channels = wf.getnchannels()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32)
    if channels > 1:
        audio = audio.reshape(-1, channels)[:, 0]  # mono
    # Resample to 44100Hz for HDMI output
    out_rate = 44100
    if rate != out_rate:
        from math import gcd
        g = gcd(out_rate, rate)
        audio = resample_poly(audio, out_rate // g, rate // g).astype(np.float32)
    # Normalise to int16 range
    audio = np.clip(audio, -32768, 32767).astype(np.int16)
    duration = len(audio) / out_rate
    print(f"   Playing audio ({duration:.1f}s at {out_rate}Hz)...")
    sd.play(audio, samplerate=out_rate)
    sd.wait()


def test_stt_only():
    """Test just STT without LLM/TTS."""
    wav = record_audio()
    text = transcribe(wav)
    if not text or text.lower() in {"", "you", "thank you."}:
        print("⚠️  No meaningful speech detected.")
    else:
        print(f"✅ STT working: \"{text}\"")


def test_tts_only():
    """Test just TTS."""
    wav = synthesize("Hello! I am KRIA, your desktop assistant. How can I help you today?")
    play_wav(wav)
    print("✅ TTS working")


def test_full_pipeline():
    """Full: mic → STT → LLM → TTS → speaker."""
    wav = record_audio()
    text = transcribe(wav)
    if not text or text.lower() in {"", "you", "thank you."}:
        print("⚠️  No speech detected, using fallback text.")
        text = "What can you do?"
    reply = ask_llm(text)
    wav_out = synthesize(reply)
    play_wav(wav_out)
    print("✅ Full pipeline complete!")


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "full"
    print(f"=== Voice Pipeline Test ({mode}) ===\n")

    if mode == "stt":
        test_stt_only()
    elif mode == "tts":
        test_tts_only()
    elif mode == "full":
        test_full_pipeline()
    else:
        print(f"Usage: {sys.argv[0]} [stt|tts|full]")
