import { Component } from "solid-js";
import { appStore } from "../stores/app";

const SettingsModal: Component = () => {
  const { setShowSettings } = appStore;

  return (
    <div class="modal-overlay" onClick={() => setShowSettings(false)}>
      <div class="modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-header">
          <h2>Settings</h2>
          <button class="close-btn" onClick={() => setShowSettings(false)}>×</button>
        </div>

        <div class="modal-body">
          <section class="settings-section">
            <h3>LLM Configuration</h3>
            <label>
              Mode
              <select>
                <option value="local">Local (llama.cpp)</option>
                <option value="cloud">Cloud (Gemini)</option>
              </select>
            </label>
          </section>

          <section class="settings-section">
            <h3>Voice</h3>
            <label>
              TTS Voice
              <select>
                <option value="en_US-lessac-high">Lessac (High)</option>
                <option value="en_US-ryan-high">Ryan (High)</option>
              </select>
            </label>
            <label>
              <input type="checkbox" /> Enable wake word
            </label>
          </section>

          <section class="settings-section">
            <h3>Safety</h3>
            <label>
              <input type="checkbox" checked /> Require approval for system changes
            </label>
            <label>
              <input type="checkbox" checked /> Audit logging
            </label>
          </section>
        </div>

        <div class="modal-footer">
          <button onClick={() => setShowSettings(false)}>Close</button>
        </div>
      </div>
    </div>
  );
};

export default SettingsModal;
