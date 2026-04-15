/// Phase 5 — Voice Pipeline tests
///
/// Validates: AudioChunk, VAD state machine, VadResult, VoicePipelineState,
/// VoicePipelineEvent, SpeechToText struct, TextToSpeech struct,
/// AudioPlayer, VoicePipeline construction, VoiceConfig defaults,
/// Silero VAD fallback, WAV writing, energy RMS.

use kria_core::voice::capture::AudioChunk;
use kria_core::voice::vad::{VoiceActivityDetector, VadResult};
use kria_core::voice::stt::SpeechToText;
use kria_core::voice::tts::TextToSpeech;
use kria_core::voice::playback::AudioPlayer;
use kria_core::voice::{VoicePipeline, VoicePipelineState, VoicePipelineEvent};
use kria_core::config::VoiceConfig;
use std::path::PathBuf;

// ── 5.1: AudioChunk ─────────────────────────────────────────────

#[test]
fn phase5_audio_chunk_creation() {
    let chunk = AudioChunk {
        samples: vec![0.0f32; 1600],
        sample_rate: 16000,
        channels: 1,
    };
    assert_eq!(chunk.samples.len(), 1600);
    assert_eq!(chunk.sample_rate, 16000);
    assert_eq!(chunk.channels, 1);
}

#[test]
fn phase5_audio_chunk_empty() {
    let chunk = AudioChunk {
        samples: vec![],
        sample_rate: 16000,
        channels: 1,
    };
    assert!(chunk.samples.is_empty());
}

// ── 5.2: VadResult enum ─────────────────────────────────────────

#[test]
fn phase5_vad_result_variants() {
    let silence = VadResult::Silence;
    let start = VadResult::SpeechStart;
    let speaking = VadResult::Speaking;
    let end = VadResult::SpeechEnd;

    assert_eq!(silence, VadResult::Silence);
    assert_eq!(start, VadResult::SpeechStart);
    assert_eq!(speaking, VadResult::Speaking);
    assert_eq!(end, VadResult::SpeechEnd);
    assert_ne!(silence, start);
}

#[test]
fn phase5_vad_result_clone() {
    let r = VadResult::Speaking;
    let r2 = r;
    assert_eq!(r, r2);
}

// ── 5.3: VAD state machine (energy-based) ────────────────────────

#[test]
fn phase5_vad_silence_on_quiet_input() {
    let mut vad = VoiceActivityDetector::new(0.01);
    let chunk = AudioChunk {
        samples: vec![0.0f32; 1600],
        sample_rate: 16000,
        channels: 1,
    };
    let result = vad.process(&chunk);
    assert_eq!(result, VadResult::Silence);
    assert!(!vad.is_speaking());
}

#[test]
fn phase5_vad_speech_start_after_threshold() {
    let mut vad = VoiceActivityDetector::new(0.01);
    // Simulate loud audio (above energy threshold)
    let loud_samples: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
    let chunk = AudioChunk {
        samples: loud_samples,
        sample_rate: 16000,
        channels: 1,
    };

    // Need min_speech_chunks (3) loud chunks before SpeechStart
    let r1 = vad.process(&chunk);
    assert_eq!(r1, VadResult::Silence); // not enough chunks yet
    let r2 = vad.process(&chunk);
    assert_eq!(r2, VadResult::Silence); // still not enough
    let r3 = vad.process(&chunk);
    assert_eq!(r3, VadResult::SpeechStart); // 3rd chunk triggers
    assert!(vad.is_speaking());
}

#[test]
fn phase5_vad_speaking_continues() {
    let mut vad = VoiceActivityDetector::new(0.01);
    let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
    let chunk = AudioChunk {
        samples: loud.clone(),
        sample_rate: 16000,
        channels: 1,
    };

    // Trigger speech start
    for _ in 0..3 {
        vad.process(&chunk);
    }
    // 4th chunk should be Speaking
    let r = vad.process(&chunk);
    assert_eq!(r, VadResult::Speaking);
}

#[test]
fn phase5_vad_speech_end_after_silence() {
    let mut vad = VoiceActivityDetector::new(0.01);
    let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
    let loud_chunk = AudioChunk {
        samples: loud,
        sample_rate: 16000,
        channels: 1,
    };
    let quiet_chunk = AudioChunk {
        samples: vec![0.0f32; 1600],
        sample_rate: 16000,
        channels: 1,
    };

    // Start speaking
    for _ in 0..3 {
        vad.process(&loud_chunk);
    }
    assert!(vad.is_speaking());

    // silence_timeout_chunks is 10, so need 10 quiet chunks for SpeechEnd
    for i in 0..10 {
        let r = vad.process(&quiet_chunk);
        if i < 9 {
            assert_eq!(r, VadResult::Speaking, "chunk {i} should still be Speaking");
        } else {
            assert_eq!(r, VadResult::SpeechEnd, "chunk {i} should be SpeechEnd");
        }
    }
    assert!(!vad.is_speaking());
}

#[test]
fn phase5_vad_reset() {
    let mut vad = VoiceActivityDetector::new(0.01);
    let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.1).sin()).collect();
    let chunk = AudioChunk {
        samples: loud,
        sample_rate: 16000,
        channels: 1,
    };

    // Start speaking
    for _ in 0..3 {
        vad.process(&chunk);
    }
    assert!(vad.is_speaking());

    vad.reset();
    assert!(!vad.is_speaking());
}

// ── 5.4: Silero VAD fallback ─────────────────────────────────────

#[test]
fn phase5_vad_without_silero() {
    let vad = VoiceActivityDetector::new(0.01);
    assert!(!vad.is_using_silero());
}

#[test]
fn phase5_vad_silero_missing_model_fallback() {
    let fake_path = PathBuf::from("/nonexistent/silero_vad.onnx");
    let vad = VoiceActivityDetector::with_silero(0.01, &fake_path);
    // Should fall back to energy-based (model doesn't exist)
    assert!(!vad.is_using_silero());
}

#[test]
fn phase5_vad_silero_fallback_still_works() {
    let fake_path = PathBuf::from("/nonexistent/silero_vad.onnx");
    let mut vad = VoiceActivityDetector::with_silero(0.01, &fake_path);
    // Energy-based fallback should still detect silence
    let chunk = AudioChunk {
        samples: vec![0.0f32; 1600],
        sample_rate: 16000,
        channels: 1,
    };
    let result = vad.process(&chunk);
    assert_eq!(result, VadResult::Silence);
}

// ── 5.5: VoicePipelineState ──────────────────────────────────────

#[test]
fn phase5_pipeline_state_serialization() {
    let idle = serde_json::to_string(&VoicePipelineState::Idle).unwrap();
    let listening = serde_json::to_string(&VoicePipelineState::Listening).unwrap();
    let processing = serde_json::to_string(&VoicePipelineState::Processing).unwrap();
    let speaking = serde_json::to_string(&VoicePipelineState::Speaking).unwrap();

    assert_eq!(idle, "\"idle\"");
    assert_eq!(listening, "\"listening\"");
    assert_eq!(processing, "\"processing\"");
    assert_eq!(speaking, "\"speaking\"");
}

#[test]
fn phase5_pipeline_state_equality() {
    assert_eq!(VoicePipelineState::Idle, VoicePipelineState::Idle);
    assert_ne!(VoicePipelineState::Idle, VoicePipelineState::Listening);
    assert_ne!(VoicePipelineState::Listening, VoicePipelineState::Processing);
}

// ── 5.6: VoicePipelineEvent ──────────────────────────────────────

#[test]
fn phase5_pipeline_event_state_changed() {
    let event = VoicePipelineEvent::StateChanged(VoicePipelineState::Listening);
    match event {
        VoicePipelineEvent::StateChanged(s) => {
            assert_eq!(s, VoicePipelineState::Listening);
        }
        _ => panic!("expected StateChanged"),
    }
}

#[test]
fn phase5_pipeline_event_transcript() {
    let event = VoicePipelineEvent::Transcript("hello world".into());
    match event {
        VoicePipelineEvent::Transcript(text) => {
            assert_eq!(text, "hello world");
        }
        _ => panic!("expected Transcript"),
    }
}

#[test]
fn phase5_pipeline_event_speaking() {
    let start = VoicePipelineEvent::SpeakingStarted;
    let done = VoicePipelineEvent::SpeakingDone;
    // Just verify these variants exist and can be constructed
    match start {
        VoicePipelineEvent::SpeakingStarted => {}
        _ => panic!("expected SpeakingStarted"),
    }
    match done {
        VoicePipelineEvent::SpeakingDone => {}
        _ => panic!("expected SpeakingDone"),
    }
}

#[test]
fn phase5_pipeline_event_error() {
    let event = VoicePipelineEvent::Error("mic failed".into());
    match event {
        VoicePipelineEvent::Error(msg) => {
            assert!(msg.contains("mic failed"));
        }
        _ => panic!("expected Error"),
    }
}

// ── 5.7: VoicePipeline construction ──────────────────────────────

#[tokio::test]
async fn phase5_pipeline_construction() {
    let config = VoiceConfig::default();
    let stt = SpeechToText::new(PathBuf::from("/tmp/test.bin"), None);
    let tts = TextToSpeech::new(PathBuf::from("/tmp/test.onnx"), None);
    let pipeline = VoicePipeline::new(config, stt, tts);
    assert_eq!(pipeline.state().await, VoicePipelineState::Idle);
    assert!(!pipeline.is_active().await);
}

#[tokio::test]
async fn phase5_pipeline_with_vad_model() {
    let config = VoiceConfig::default();
    let stt = SpeechToText::new(PathBuf::from("/tmp/test.bin"), None);
    let tts = TextToSpeech::new(PathBuf::from("/tmp/test.onnx"), None);
    let pipeline = VoicePipeline::new(config, stt, tts)
        .with_vad_model(PathBuf::from("/nonexistent/silero_vad.onnx"));
    assert_eq!(pipeline.state().await, VoicePipelineState::Idle);
}

#[tokio::test]
async fn phase5_pipeline_state_watch() {
    let config = VoiceConfig::default();
    let stt = SpeechToText::new(PathBuf::from("/tmp/test.bin"), None);
    let tts = TextToSpeech::new(PathBuf::from("/tmp/test.onnx"), None);
    let pipeline = VoicePipeline::new(config, stt, tts);
    let rx = pipeline.state_watch();
    assert_eq!(*rx.borrow(), VoicePipelineState::Idle);
}

// ── 5.8: SpeechToText struct ─────────────────────────────────────

#[test]
fn phase5_stt_construction() {
    let stt = SpeechToText::new(
        PathBuf::from("/models/stt/ggml-base.en.bin"),
        Some(PathBuf::from("/usr/bin/whisper-cpp")),
    );
    assert_eq!(stt.model_path(), &PathBuf::from("/models/stt/ggml-base.en.bin"));
}

#[test]
fn phase5_stt_no_binary() {
    let stt = SpeechToText::new(PathBuf::from("/tmp/model.bin"), None);
    assert_eq!(stt.model_path(), &PathBuf::from("/tmp/model.bin"));
}

// ── 5.9: TextToSpeech struct ─────────────────────────────────────

#[test]
fn phase5_tts_construction() {
    let tts = TextToSpeech::new(
        PathBuf::from("/models/piper/en_US-lessac-high.onnx"),
        Some(PathBuf::from("/usr/bin/piper")),
    );
    assert_eq!(tts.sample_rate(), 22050);
}

#[test]
fn phase5_tts_no_binary() {
    let tts = TextToSpeech::new(PathBuf::from("/tmp/voice.onnx"), None);
    assert_eq!(tts.sample_rate(), 22050);
}

// ── 5.10: AudioPlayer ────────────────────────────────────────────

#[test]
fn phase5_audio_player_creation() {
    let _player = AudioPlayer::new();
    // Just verify it can be created without panicking
}

// ── 5.11: VoiceConfig defaults ───────────────────────────────────

#[test]
fn phase5_voice_config_defaults() {
    let config = VoiceConfig::default();
    assert!(!config.enabled);
    assert_eq!(config.mode, "push_to_talk");
    assert_eq!(config.stt_model, "ggml-base.en.bin");
    assert_eq!(config.tts_voice, "en_US-lessac-high");
    assert_eq!(config.vad_silence_ms, 1000);
    assert!(config.energy_threshold > 0.0);
    assert_eq!(config.mic_device, "auto");
    assert_eq!(config.speaker_device, "auto");
    assert_eq!(config.push_to_talk_key, "ctrl+space");
}

#[test]
fn phase5_voice_config_serialization() {
    let config = VoiceConfig::default();
    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("push_to_talk"));
    assert!(json.contains("ggml-base.en.bin"));
    let deserialized: VoiceConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.mode, config.mode);
    assert_eq!(deserialized.energy_threshold, config.energy_threshold);
}
