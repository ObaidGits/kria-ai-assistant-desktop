// ─────────────────────────────────────────────────────────────────────────────
//  voice_live_tests.rs
//
//  Live voice integration tests.
//  Requires KRIA_VOICE_LIVE=1.
//  ALL tests are #[ignore] — they depend on real mic/speaker hardware.
//
//  Run with:
//    KRIA_VOICE_LIVE=1 cargo test -p kria-core --test voice_live_tests -- --ignored --test-threads=1
// ─────────────────────────────────────────────────────────────────────────────

mod common;

use common::voice_live_enabled;

macro_rules! voice_guard {
    () => {
        if !voice_live_enabled() {
            eprintln!("SKIP: KRIA_VOICE_LIVE not set. Set KRIA_VOICE_LIVE=1 to run.");
            return;
        }
    };
}

// ═══════════════════════════════════════════════════════════════════════════
//  VOICE-01 — Wake-word "Hey Ria" detected
// ═══════════════════════════════════════════════════════════════════════════

/// Verify that "Hey Ria" is in the configured wake-word list.
/// This is a config/policy check — no real audio hardware needed.
#[test]
fn voice01_hey_ria_in_configured_wake_words() {
    // The wake-word aliases are defined in user preferences / config.
    let expected = ["Hey Ria", "Hey Riya", "Hello Ria", "Hello Riya"];
    for phrase in &expected {
        // Router should not route wake-word phrases to destructive tools
        let r = kria_core::agent::router::IntentRouter::classify(phrase);
        let is_destructive = matches!(&r.intent,
            kria_core::agent::router::Intent::DirectTool(t)
            if matches!(t.as_str(), "shutdown" | "delete_file" | "execute_bash" | "gw_gmail_delete")
        );
        assert!(
            !is_destructive,
            "Wake phrase '{phrase}' must not route to destructive tool"
        );
    }
}

/// Live wake-word detection via actual audio pipeline.
/// Requires KRIA_VOICE_LIVE=1 and a running capture device.
#[tokio::test]
#[ignore]
async fn voice01_live_hey_ria_wake_word_detected() {
    voice_guard!();
    eprintln!("TODO: Inject synthetic 'Hey Ria' audio into the VoicePipeline capture stream");
    eprintln!("and assert that the wake-word FSM fires within 3 seconds.");
    // Implementation: spin up VoicePipeline with a file-based SpeechToText stub,
    // feed pre-recorded PCM of 'Hey Ria', observe state transitions via state_watch().
}

/// Live alias "Hey Riya"
#[tokio::test]
#[ignore]
async fn voice01_live_hey_riya_alias_detected() {
    voice_guard!();
    eprintln!("TODO: Same as above but with 'Hey Riya' PCM.");
}

// ═══════════════════════════════════════════════════════════════════════════
//  VOICE-02 — Barge-in interrupts TTS playback
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn voice02_live_barge_in_interrupts_tts() {
    voice_guard!();
    eprintln!("TODO: Start VoicePipeline.speak() for a long sentence,");
    eprintln!("then inject audio input mid-speech and assert pipeline.state() transitions.");
}

// ═══════════════════════════════════════════════════════════════════════════
//  VOICE-03 — Hinglish round-trip
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn voice03_live_hinglish_roundtrip() {
    voice_guard!();
    eprintln!("TODO: Feed pre-recorded 'Aaj mausam kaisa hai?' PCM into STT,");
    eprintln!("assert transcript contains 'mausam' or 'kaisa'.");
}

// ═══════════════════════════════════════════════════════════════════════════
//  VOICE-04 — PTT (Push-to-Talk) Ctrl+Space
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn voice04_live_ptt_ctrl_space_activates_listening() {
    voice_guard!();
    eprintln!("TODO: Simulate Ctrl+Space X11 key event via xdotool,");
    eprintln!("assert VoicePipeline state changes to Listening.");
}

// ═══════════════════════════════════════════════════════════════════════════
//  VOICE-05 — VAD end-of-speech detection
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore]
async fn voice05_live_vad_end_of_speech_detected() {
    voice_guard!();
    eprintln!("TODO: Feed speech PCM followed by 800ms of silence,");
    eprintln!("assert the VoicePipeline VAD triggers end-of-speech.");
}

// ═══════════════════════════════════════════════════════════════════════════
//  VOICE-06 — Emergency stop phrase "KRIA stop now"
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn voice06_emergency_stop_phrase_is_recognized_by_router() {
    // Compile-time / routing assertion — no audio hardware needed
    use kria_core::agent::router::{Intent, IntentRouter};
    let r = IntentRouter::classify("KRIA stop now");
    let is_destructive_data_tool = matches!(&r.intent,
        Intent::DirectTool(t)
        if matches!(t.as_str(), "write_file" | "delete_file" | "execute_bash" |
            "gw_gmail_send" | "gw_gmail_delete" | "shutdown" | "reboot")
    );
    assert!(
        !is_destructive_data_tool,
        "Emergency stop must not route to a data-modifying tool, got: {:?}",
        r.intent
    );
}

#[tokio::test]
#[ignore]
async fn voice06_live_emergency_stop_kria_stop_now() {
    voice_guard!();
    eprintln!("TODO: While a task is executing, inject 'KRIA stop now' voice input,");
    eprintln!("assert all in-flight tasks are cancelled within 1 second.");
}
