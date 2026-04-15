import { Component } from "solid-js";
import { appStore } from "../stores/app";

const VoiceOverlay: Component = () => {
  const { toggleVoice } = appStore;

  return (
    <div class="voice-overlay">
      <div class="voice-indicator">
        <div class="pulse-ring" />
        <div class="mic-icon">🎤</div>
      </div>
      <p class="voice-label">Listening...</p>
      <button class="voice-stop-btn" onClick={() => toggleVoice()}>
        Stop
      </button>
    </div>
  );
};

export default VoiceOverlay;
