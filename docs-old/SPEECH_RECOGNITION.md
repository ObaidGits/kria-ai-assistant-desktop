# KRIA Speech Recognition — Road to Google/Gemini-Level Quality

## Current State Analysis

### Hardware Profile

| Component | Spec |
|---|---|
| **CPU** | Intel i7-13700HX (24 threads) |
| **GPU** | NVIDIA RTX 4050 Laptop (6 GB VRAM) |
| **RAM** | 16 GB (5 GB available) |
| **Mic** | `sof-hda-dsp hw:2,7` (built-in laptop mic, 16 kHz native) |
| **Speaker** | HDMI output at 44100 Hz |

### Current Pipeline

```
Mic (16kHz) → Silero VAD → Record → noisereduce → Whisper small.en (CPU, 487 MB, ~2s)
    → hallucination filter → kria-core → Piper TTS → resample → speaker
```

### Measured Problems

| Problem | Root Cause |
|---|---|
| "in my works out" instead of "it never works out" | Whisper small.en lacks accuracy for noisy input |
| "(wind howling)" / "[BLANK_AUDIO]" hallucinations | Ambient RMS ~1500 — laptop fan/room noise |
| "Open word cell" instead of "Open WhatsApp" | small.en has 244M params — too small for domain vocab |
| Words clipped mid-sentence | Silence window too short, VAD ends recording early |
| Whisper runs on CPU only (4 threads) | Docker container built without CUDA |
| TTS was unplayable (22050 Hz → HDMI at 44100) | Fixed — now resamples before playback |

### Why Google/Siri Sound Better

Google and Siri use:
- **2B+ parameter models** (Chirp 3, Conformer) vs KRIA's 244M (small.en)
- **Cloud GPU clusters** (TPU v5, A100) vs KRIA's single laptop CPU
- **12M+ hours** of training data with noise augmentation
- **Streaming RNN-T** architecture with <200ms latency
- **On-device neural engines** specifically designed for speech
- **Massive noise-robust training** with real-world recordings

KRIA runs **100% locally** with no cloud dependency — that's the tradeoff. But we can close the gap significantly.

---

## Implementation Plan

### Phase 0: Quick Wins (No Architecture Change)
> **Impact: High | Effort: 1 hour | Risk: None**

These are configuration changes to the existing system that yield immediate improvement.

#### 0.1 — Enable GPU for Whisper (Biggest Single Win)

The RTX 4050 has 6 GB VRAM and sits completely idle right now. Whisper inference on GPU
is **10-20x faster**, which enables using larger/better models.

**Strategy:** Build whisper.cpp with CUDA in the kria-brain container. Keep the LLM on CPU
(Phi-4-mini is fast enough on CPU). Give Whisper the entire GPU.

```bash
# Modified entrypoint.sh — add --gpu-layers 99 to whisper-server
whisper-server \
    --model "${WHISPER_MODEL_FILE}" \
    --host 0.0.0.0 \
    --port 8081 \
    --threads 4 \
    --gpu-layers 99 \
    &
```

**Docker changes needed:**
- Build whisper.cpp with `-DGGML_CUDA=ON` in the Dockerfile
- Add NVIDIA runtime to the docker-compose for kria-brain
- Keep `LLAMA_GPU_LAYERS=0` (LLM stays on CPU)

**Expected result:** Whisper small.en drops from ~2s → ~0.2s per utterance.

#### 0.2 — Upgrade to Whisper medium.en (on GPU)

Once GPU inference is enabled, VRAM budget allows `medium.en` (769M params, 1.5 GB):

| Model | Params | Size | WER (LibriSpeech) | GPU Inference |
|---|---|---|---|---|
| small.en | 244M | 487 MB | 7.7% | ~0.15s |
| **medium.en** | **769M** | **1.5 GB** | **5.8%** | **~0.3s** |
| large-v3-turbo | 809M | 1.6 GB | 5.2% | ~0.4s |

`medium.en` is the sweet spot — 25% more accurate than small.en and fits in 6 GB VRAM alongside
the whisper-server overhead.

#### 0.3 — Increase Whisper Threads (if staying on CPU)

The current config uses only 4 of 24 available threads. If GPU isn't used:

```bash
whisper-server --threads 12  # use half the CPU cores
```

#### 0.4 — High-Pass Filter Before Noisereduce

Laptop fans produce low-frequency rumble (50-300 Hz) that confuses both VAD and Whisper.
A simple 300 Hz high-pass filter in the bridge removes this without affecting speech (300-3400 Hz band).

```python
import scipy.signal as signal
# 4th order Butterworth high-pass at 300 Hz
b, a = signal.butter(4, 300, btype='high', fs=16000)
audio = signal.lfilter(b, a, audio).astype(np.int16)
```

**Dependency:** `pip install scipy`

---

### Phase 1: GPU-Accelerated Whisper (Recommended First Step)
> **Impact: Very High | Effort: 2-3 hours | Risk: Low**

This is the single highest-impact change. It enables larger models and faster inference.

#### 1.1 — Create CUDA-Enabled Whisper Build

**New Dockerfile approach:** Build whisper.cpp with CUDA support.

```dockerfile
# In Dockerfile.gpu or modified Dockerfile
FROM nvidia/cuda:12.4.0-devel-ubuntu24.04 AS whisper-builder
ARG WHISPER_VERSION=v1.8.4
RUN apt-get update && apt-get install -y build-essential cmake git
WORKDIR /src
RUN git clone --depth 1 --branch ${WHISPER_VERSION} \
        https://github.com/ggml-org/whisper.cpp . \
    && cmake -B build \
        -DCMAKE_BUILD_TYPE=Release \
        -DGGML_CUDA=ON \
        -DWHISPER_BUILD_TESTS=OFF \
    && cmake --build build --target whisper-server -j$(nproc)
```

#### 1.2 — Split GPU Between LLM and Whisper

The RTX 4050 has 6 GB VRAM. Budget:

| Component | VRAM Usage | Strategy |
|---|---|---|
| Whisper medium.en | ~1.5 GB | Full GPU offload |
| Phi-4-mini Q4_K_M | ~2.5 GB | Could GPU offload too |
| Overhead | ~0.5 GB | CUDA context |
| **Total** | **~2.9 GB** | **Fits in 6 GB** |

Both the LLM and Whisper can run on GPU simultaneously, leaving ~3 GB headroom.

#### 1.3 — VRAM Orchestration

The existing `src/kria/infra/vram_orchestrator.py` was designed for this. The approach:
- Whisper gets permanent GPU residency (always ready for speech)
- LLM loads to GPU on demand (takes ~200ms for Phi-4-mini model)
- If using the secondary model (Qwen2.5-VL-7B), Whisper stays on GPU and LLM stays on CPU

---

### Phase 2: Faster-Whisper (Alternative to whisper.cpp)
> **Impact: High | Effort: 3-4 hours | Risk: Medium**

[faster-whisper](https://github.com/SYSTRAN/faster-whisper) uses CTranslate2  — a highly
optimized inference engine. On the same hardware it is typically **2-4x faster** than
whisper.cpp for GPU and **3x faster** for CPU.

#### 2.1 — Why Faster-Whisper Over whisper.cpp

| Feature | whisper.cpp | faster-whisper |
|---|---|---|
| Language | C++ | Python (CTranslate2 backend) |
| GPU support | CUDA (manual build) | CUDA + cuDNN out of box |
| Batched inference | No | Yes |
| VAD integration | No | Built-in Silero VAD |
| Word timestamps | Basic | Accurate |
| Beam search | Basic | Full implementation |
| **Speed (GPU, medium.en)** | **~0.3s/utterance** | **~0.15s/utterance** |

#### 2.2 — Implementation

Replace the whisper.cpp server in kria-brain with a Python FastAPI wrapper:

```python
# docker/brain/whisper_server.py
from faster_whisper import WhisperModel
from fastapi import FastAPI, UploadFile
import uvicorn

model = WhisperModel("medium.en", device="cuda", compute_type="float16")

app = FastAPI()

@app.post("/inference")
async def inference(file: UploadFile):
    segments, info = model.transcribe(
        file.file,
        language="en",
        beam_size=5,
        best_of=5,
        temperature=0.0,
        initial_prompt="KRIA voice assistant. Commands: open, play, search, tell me.",
        vad_filter=True,           # Built-in Silero VAD
        vad_parameters=dict(
            min_speech_duration_ms=250,
            max_speech_duration_s=30,
            min_silence_duration_ms=800,
            speech_pad_ms=300,
        ),
    )
    text = " ".join(seg.text for seg in segments).strip()
    return {"text": text}
```

**Advantages:**
- Built-in Silero VAD (can remove VAD from the bridge — cleaner architecture)
- Better beam search → fewer misrecognitions
- `float16` compute → 2x faster than float32
- Word-level timestamps (useful for future features)

#### 2.3 — Docker Changes

```dockerfile
FROM nvidia/cuda:12.4.0-runtime-ubuntu24.04
RUN pip install faster-whisper fastapi uvicorn python-multipart
# Download model at build time
RUN python3 -c "from faster_whisper import WhisperModel; WhisperModel('medium.en')"
```

---

### Phase 3: Distil-Whisper (Best Speed/Accuracy Balance)
> **Impact: High | Effort: 2 hours | Risk: Low**

[Distil-Whisper](https://github.com/huggingface/distil-whisper) is a distilled version of
Whisper large-v3 that runs **6x faster** while retaining 99% of the accuracy.

| Model | Params | Speed (GPU) | WER |
|---|---|---|---|
| small.en (current) | 244M | 0.15s | 7.7% |
| distil-large-v3 | 756M | 0.12s | 5.7% |
| large-v3 | 1.55B | 0.4s | 4.2% |

**distil-large-v3 is faster than small.en on GPU** because it has fewer decoder layers (2 vs 12)
despite being a larger model overall. This is the best option if you want near-Google accuracy
with low latency.

```python
# Using faster-whisper with distil model
model = WhisperModel("distil-large-v3", device="cuda", compute_type="float16")
# VRAM: ~1.8 GB — still fits in 6 GB
```

---

### Phase 4: Advanced Audio Preprocessing
> **Impact: Medium | Effort: 2-3 hours | Risk: Low**

#### 4.1 — Spectral Gating (Better Than noisereduce)

Replace `noisereduce` with a spectral gating approach that preserves speech harmonics:

```python
import numpy as np
from scipy import signal

def spectral_gate(audio, sr=16000, noise_floor_db=-40):
    """Remove noise below a spectral threshold while preserving speech."""
    f, t, Zxx = signal.stft(audio, fs=sr, nperseg=512)
    magnitude = np.abs(Zxx)
    phase = np.angle(Zxx)

    # Estimate noise floor from first 200ms (assumed non-speech)
    noise_frames = int(0.2 * sr / 256)
    noise_profile = np.mean(magnitude[:, :noise_frames], axis=1, keepdims=True)

    # Soft mask: attenuate where signal is close to noise floor
    mask = np.maximum(magnitude - 2 * noise_profile, 0) / (magnitude + 1e-10)
    mask = np.clip(mask, 0.05, 1.0)  # keep 5% minimum to avoid artifacts

    cleaned = magnitude * mask * np.exp(1j * phase)
    _, audio_out = signal.istft(cleaned, fs=sr)
    return audio_out.astype(np.int16)
```

#### 4.2 — High-Pass Filter Stack

```python
# Apply in sequence before Whisper:
# 1. High-pass at 80 Hz (remove DC offset and sub-bass rumble)
# 2. High-pass at 300 Hz (remove fan noise) — only if ambient RMS > 1000
# 3. Spectral gate (remove remaining stationary noise)
# 4. Normalize to -3 dB peak (consistent input level for Whisper)
```

#### 4.3 — Automatic Gain Control (AGC)

Normalize audio to a consistent level before STT. Whisper performs best with audio around
-20 dBFS peak:

```python
def agc(audio, target_db=-20):
    peak = np.max(np.abs(audio.astype(np.float32)))
    if peak < 100:  # silence
        return audio
    target_peak = 32768 * (10 ** (target_db / 20))
    gain = target_peak / peak
    return (audio.astype(np.float32) * gain).clip(-32768, 32767).astype(np.int16)
```

---

### Phase 5: NVIDIA Canary / Parakeet (State-of-the-Art)
> **Impact: Very High | Effort: 1-2 days | Risk: Medium**

NVIDIA's [Canary](https://huggingface.co/nvidia/canary-1b) and
[Parakeet](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2) models lead the Open ASR
Leaderboard in 2025-2026. They are purpose-built for on-device inference.

#### 5.1 — Parakeet TDT 0.6B (Best for KRIA)

| Feature | Value |
|---|---|
| Params | 600M |
| WER (LibriSpeech clean) | 2.9% |
| WER (LibriSpeech other) | 5.5% |
| VRAM | ~1.5 GB (float16) |
| Latency | ~0.1s per utterance (GPU) |
| Architecture | FastConformer + Token-Duration Transducer |
| Streaming | Yes (real-time) |

```python
import nemo.collections.asr as nemo_asr

model = nemo_asr.models.ASRModel.from_pretrained("nvidia/parakeet-tdt-0.6b-v2")
model = model.to("cuda")

# Transcribe
transcription = model.transcribe(["audio.wav"])
```

#### 5.2 — Why Parakeet Over Whisper

| Aspect | Whisper | Parakeet TDT |
|---|---|---|
| Architecture | Encoder-Decoder (offline) | Transducer (streaming-capable) |
| Noise robustness | Moderate | High (trained on noisy data) |
| English accuracy | 5-8% WER | 2.9-5.5% WER |
| Latency model | Process full utterance | Can stream word-by-word |
| Hallucinations | Common (trained on web) | Rare (CTC-based, no hallucination tendency) |
| VRAM (0.6B) | ~1 GB | ~1.5 GB |

The key advantage: **Transducer models don't hallucinate.** They can only output tokens that
correspond to actual audio frames. Whisper's encoder-decoder architecture can "dream up" text
that isn't in the audio — that's why you see "(wind blowing)" and "[BLANK_AUDIO]".

#### 5.3 — Docker Integration

```dockerfile
FROM nvidia/cuda:12.4.0-runtime-ubuntu24.04
RUN pip install nemo_toolkit[asr] fastapi uvicorn
# Pre-download model
RUN python3 -c "import nemo.collections.asr as asr; asr.models.ASRModel.from_pretrained('nvidia/parakeet-tdt-0.6b-v2')"
```

**Estimated VRAM budget with Parakeet:**

| Component | VRAM |
|---|---|
| Parakeet TDT 0.6B | 1.5 GB |
| Phi-4-mini (GPU) | 2.5 GB |
| Overhead | 0.5 GB |
| **Total** | **2.9 GB / 6 GB** |

#### 5.4 — Canary 1B (Higher Accuracy, More VRAM)

If maximum accuracy is needed, Canary 1B achieves ~4.0% WER on noisy data:

```python
model = nemo_asr.models.ASRModel.from_pretrained("nvidia/canary-1b")
# VRAM: ~2.5 GB — still fits alongside Phi-4-mini
```

---

### Phase 6: Fine-Tuning for Your Environment
> **Impact: Very High | Effort: 1-2 days | Risk: Medium**

Fine-tuning a model on your specific microphone, room acoustics, and voice will
dramatically improve accuracy — potentially more than any model upgrade.

#### 6.1 — Collect Training Data

Record 30-60 minutes of your own voice with the KRIA microphone:

```python
# Script: scripts/collect_training_data.py
# Records utterances, you type the correct transcript
# Saves pairs to data/voice_training/

commands = [
    "Open Chrome",
    "Open WhatsApp",
    "What time is it",
    "Play some music",
    "Tell me about the weather",
    "Search for Python tutorials",
    # ... 200+ unique utterances
]
```

#### 6.2 — Fine-Tune with LoRA

Using Hugging Face PEFT for lightweight fine-tuning:

```python
from transformers import WhisperForConditionalGeneration
from peft import LoraConfig, get_peft_model

model = WhisperForConditionalGeneration.from_pretrained("openai/whisper-medium.en")

lora_config = LoraConfig(
    r=16,
    lora_alpha=32,
    target_modules=["q_proj", "v_proj"],
    lora_dropout=0.05,
)
model = get_peft_model(model, lora_config)
# Trainable params: ~2M (vs 769M total) — trains in minutes on RTX 4050
```

#### 6.3 — Noise Augmentation

Augment training data with recorded ambient noise from your room:

```python
# Record 5 minutes of room silence → room_noise.wav
# Mix with speech at various SNR levels
snr_levels = [5, 10, 15, 20]  # dB
for snr in snr_levels:
    augmented = mix_audio(speech, noise, snr_db=snr)
    training_data.append((augmented, transcript))
```

This teaches the model to recognize speech despite your specific room noise.

---

### Phase 7: Streaming ASR (Real-Time, Sub-200ms)
> **Impact: High | Effort: 3-5 days | Risk: High**

For true Siri-level responsiveness, switch from "record → process" to real-time streaming.

#### 7.1 — Architecture Change

Current (batch mode):
```
[Record 1-3s] → [Send to Whisper] → [Wait 1-2s] → [Get text]
Total latency: 3-5 seconds from first word to response
```

Streaming mode:
```
[Audio frame] → [Transducer] → [Partial text appears instantly]
                                 → [Final text in ~200ms after speech ends]
```

#### 7.2 — Implementation with Parakeet TDT Streaming

```python
# Parakeet TDT supports streaming natively
model = nemo_asr.models.ASRModel.from_pretrained("nvidia/parakeet-tdt-0.6b-v2")

# Stream chunks as they arrive from the microphone
for audio_chunk in mic_stream:
    partial_text = model.transcribe_stream(audio_chunk)
    if partial_text:
        print(f"\r  Hearing: {partial_text}", end="")
```

#### 7.3 — WebSocket-Based Streaming

Replace the HTTP POST `/inference` endpoint with a WebSocket:

```python
# In kria-brain container
@app.websocket("/ws/transcribe")
async def ws_transcribe(ws: WebSocket):
    await ws.accept()
    while True:
        audio_chunk = await ws.receive_bytes()
        text = model.transcribe_stream(audio_chunk)
        if text:
            await ws.send_json({"text": text, "is_final": False})
```

---

## Recommended Implementation Order

Given your hardware (RTX 4050 6GB, i7-13700HX, 16GB RAM) and current setup:

### Tier 1: Immediate (Do This Week)
| # | Change | Time | Impact |
|---|---|---|---|
| 1 | **Build whisper.cpp with CUDA** → GPU inference | 2h | 10-20x faster STT |
| 2 | **Switch to medium.en or large-v3-turbo** (GPU makes it fast enough) | 30m | 25-30% fewer errors |
| 3 | **Add high-pass filter** (300 Hz) to remove fan/rumble | 30m | Fewer hallucinations |
| 4 | **Increase Whisper threads** to 12 (if staying CPU) | 5m | ~2x faster CPU |

### Tier 2: This Month
| # | Change | Time | Impact |
|---|---|---|---|
| 5 | **Replace whisper.cpp with faster-whisper** | 3h | 2-4x faster, better beam search |
| 6 | **Switch to distil-large-v3** | 30m | Near large-v3 accuracy at small.en speed |
| 7 | **Spectral gating** audio preprocessing | 2h | Better noise rejection |
| 8 | **AGC normalization** | 30m | Consistent input level |

### Tier 3: Next Sprint
| # | Change | Time | Impact |
|---|---|---|---|
| 9 | **Evaluate Parakeet TDT 0.6B** | 4h | Zero hallucinations, streaming |
| 10 | **Fine-tune on your voice/room** (LoRA) | 1d | Dramatic accuracy boost |
| 11 | **Streaming WebSocket ASR** | 2d | Sub-200ms perceived latency |

### Tier 4: Advanced (If Needed)
| # | Change | Time | Impact |
|---|---|---|---|
| 12 | **Canary 1B** evaluation | 4h | State-of-the-art accuracy |
| 13 | **TensorRT optimization** | 1d | Additional 2-3x speedup |
| 14 | **Custom vocabulary / hot-word boosting** | 4h | App names recognized correctly |

---

## VRAM Budget Planning

### Option A: Whisper + LLM Both on GPU (Recommended)
```
Whisper medium.en     : 1.5 GB
Phi-4-mini Q4_K_M     : 2.5 GB
CUDA overhead         : 0.5 GB
─────────────────────────────
Total                 : 4.5 GB / 6.0 GB  ✓ (1.5 GB headroom)
```

### Option B: Distil-Whisper + LLM on GPU
```
distil-large-v3       : 1.8 GB
Phi-4-mini Q4_K_M     : 2.5 GB
CUDA overhead         : 0.5 GB
─────────────────────────────
Total                 : 4.8 GB / 6.0 GB  ✓ (1.2 GB headroom)
```

### Option C: Parakeet + LLM on GPU
```
Parakeet TDT 0.6B    : 1.5 GB
Phi-4-mini Q4_K_M     : 2.5 GB
CUDA overhead         : 0.5 GB
─────────────────────────────
Total                 : 4.5 GB / 6.0 GB  ✓ (1.5 GB headroom)
```

### Option D: Maximum Accuracy (Canary + LLM)
```
Canary 1B             : 2.5 GB
Phi-4-mini Q4_K_M     : 2.5 GB
CUDA overhead         : 0.5 GB
─────────────────────────────
Total                 : 5.5 GB / 6.0 GB  ✓ (0.5 GB headroom)
```

All options fit within 6 GB VRAM with headroom.

---

## Accuracy Expectations

| Configuration | WER (Clean) | WER (Noisy) | Latency | Hallucinations |
|---|---|---|---|---|
| **Current** (small.en, CPU) | ~7.7% | ~15-20% | ~2s | Frequent |
| **Phase 1** (medium.en, GPU) | ~5.8% | ~10-12% | ~0.3s | Moderate |
| **Phase 2** (faster-whisper, distil-large-v3) | ~5.0% | ~8-10% | ~0.15s | Low |
| **Phase 5** (Parakeet TDT) | ~2.9% | ~6-8% | ~0.1s | **None** |
| **Phase 6** (Fine-tuned + Parakeet) | ~2% | ~4-5% | ~0.1s | **None** |
| Google/Siri (reference) | ~2% | ~3-5% | <0.2s | Very rare |

With Phase 5 + 6, KRIA would be within striking distance of Google/Siri accuracy for
English commands, running 100% locally on your laptop.

---

## Quick Reference: Key Files to Modify

| File | Change |
|---|---|
| `docker/brain/Dockerfile` | Add CUDA build for whisper.cpp |
| `docker/brain/entrypoint.sh` | Add `--gpu-layers 99` to whisper-server |
| `docker/docker-compose.yml` | Add NVIDIA runtime to kria-brain |
| `scripts/kria_bridge.py` | Add high-pass filter, AGC, improve preprocessing |
| `docker/brain/whisper_server.py` | New file if switching to faster-whisper |
| `src/kria/infra/vram_orchestrator.py` | Configure VRAM split for LLM + STT |
