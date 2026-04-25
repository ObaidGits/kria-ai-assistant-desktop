import { Component, Show, For } from "solid-js";
import { appStore } from "../stores/app";

const VoiceOverlay: Component = () => {
  const {
    toggleVoice,
    voiceState,
    voiceLiveTranscript,
    voiceLiveConfidence,
    voiceLiveLanguage,
  } = appStore;

  const stateLabel = () => {
    switch (voiceState()) {
      case "listening":  return "Listening";
      case "processing": return "Thinking…";
      case "speaking":   return "Speaking";
      default:           return "Voice";
    }
  };

  const stateClass = () => `voice-overlay voice-state-${voiceState()}`;
  const bars = [0, 1, 2, 3, 4];

  return (
    <div class={stateClass()} role="status" aria-live="polite">
      <div class="voice-overlay__card">
        <button
          class="voice-overlay__close"
          onClick={() => toggleVoice()}
          aria-label="Stop voice"
          title="Stop voice"
        >
          ×
        </button>

        <div class="voice-overlay__icon">
          <Show
            when={voiceState() === "listening" || voiceState() === "processing"}
            fallback={
              <Show when={voiceState() === "speaking"} fallback={<MicGlyph />}>
                <SpeakerGlyph />
              </Show>
            }
          >
            <div class="voice-overlay__waveform" aria-hidden="true">
              <For each={bars}>{(i) => <span style={{ "animation-delay": `${i * 0.12}s` }} />}</For>
            </div>
          </Show>
        </div>

        <div class="voice-overlay__text">
          <span class="voice-overlay__label">{stateLabel()}</span>
          <Show when={voiceLiveTranscript().length > 0}>
            <span class="voice-overlay__transcript">{voiceLiveTranscript()}</span>
          </Show>
          <Show when={voiceLiveConfidence() !== null}>
            <span class="voice-overlay__meta">
              {voiceLiveLanguage()}
              {` · ${Math.round((voiceLiveConfidence() ?? 0) * 100)}%`}
            </span>
          </Show>
        </div>
      </div>
    </div>
  );
};

const MicGlyph: Component = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <rect x="9" y="2" width="6" height="12" rx="3" />
    <path d="M5 10v2a7 7 0 0 0 14 0v-2" />
    <line x1="12" y1="19" x2="12" y2="22" />
  </svg>
);

const SpeakerGlyph: Component = () => (
  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
    <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
    <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
  </svg>
);

export default VoiceOverlay;
