import { Component, Show } from "solid-js";
import { appStore } from "./stores/app";
import ChatView from "./components/ChatView";
import SessionSidebar from "./components/SessionSidebar";
import SettingsModal from "./components/SettingsModal";
import HitlModal from "./components/HitlModal";
import VoiceOverlay from "./components/VoiceOverlay";

const App: Component = () => {
  const { showSettings, showHitl, voiceActive } = appStore;

  return (
    <div class="app">
      <div class="app-layout">
        <SessionSidebar />
        <main class="main-content">
          <ChatView />
        </main>
      </div>

      <Show when={showSettings()}>
        <SettingsModal />
      </Show>

      <Show when={showHitl()}>
        <HitlModal />
      </Show>

      <Show when={voiceActive()}>
        <VoiceOverlay />
      </Show>
    </div>
  );
};

export default App;
