import { Component, Show } from "solid-js";
import { appStore } from "../stores/app";

const VoiceOverlay: Component = () => {
  const {
    toggleVoice,
    voiceState,
    voiceLiveTranscript,
    voiceLiveConfidence,
    voiceLiveLanguage,
    voiceLiveStability,
  } = appStore;

  const stateLabel = () => {
    switch (voiceState()) {
      case "listening": return "Listening...";
      case "processing": return "Processing...";
      case "speaking": return "Speaking...";
      default: return "Voice";
    }
  };

  const stateClass = () => `voice-overlay voice-state-${voiceState()}`;

  return (
    <div class={stateClass()}>
      <div class="voice-indicator">
        <div class="pulse-ring" />
        <div class="mic-icon">
          <Show when={voiceState() === "speaking"} fallback="🎤">
            🔊
          </Show>
        </div>
      </div>
      <p class="voice-label">{stateLabel()}</p>
      <Show when={voiceState() === "listening" || voiceState() === "processing"}>
        <div class="voice-volume-bar">
          <div class="voice-volume-fill" />
        </div>
      </Show>
      <Show when={voiceLiveTranscript().length > 0}>
        <p class="voice-live-transcript">{voiceLiveTranscript()}</p>
      </Show>
      <Show when={voiceLiveConfidence() !== null || voiceLiveStability() !== null}>
        <p class="voice-live-meta">
          {`lang: ${voiceLiveLanguage()} | conf: ${voiceLiveConfidence() !== null ? (voiceLiveConfidence()! * 100).toFixed(0) : "--"}% | stable: ${voiceLiveStability() !== null ? (voiceLiveStability()! * 100).toFixed(0) : "--"}%`}
        </p>
      </Show>
      <button class="voice-stop-btn" onClick={() => toggleVoice()}>
        Stop
      </button>
    </div>
  );
};

export default VoiceOverlay;
