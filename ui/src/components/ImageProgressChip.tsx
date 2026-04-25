import { Component, Show } from "solid-js";
import { appStore } from "../stores/app";

/**
 * ImageProgressChip — inline progress bar shown during image generation.
 * Reads `imageGenProgress`, `imageGenStage`, `vramBlackoutInfo`, and
 * `imageSessionDegraded` from the app store.
 * Renders nothing when no generation is in flight.
 */
const ImageProgressChip: Component = () => {
  const prog = appStore.imageGenProgress;
  const stage = appStore.imageGenStage;
  const blackout = appStore.vramBlackoutInfo;
  const degraded = appStore.imageSessionDegraded;

  return (
    <>
      {/* Session-degraded banner — shown once and persists until page reload */}
      <Show when={degraded()}>
        <div class="image-session-degraded-banner" role="alert">
          <span class="image-session-degraded-icon">⚠️</span>
          <span>Using cloud image generation this session (local GPU swap timed out)</span>
        </div>
      </Show>

      <Show when={prog() !== null}>
        <div class="image-progress-chip" role="status" aria-live="polite">
          <span class="image-progress-icon">🎨</span>
          <div class="image-progress-body">
            <div class="image-progress-label">
              {/* VRAM blackout sub-state overrides the default stage label */}
              <Show
                when={blackout() !== null}
                fallback={<span>Generating image…</span>}
              >
                <span class="image-progress-vram">
                  {blackout()?.stage === "retry_after_interrupt"
                    ? "Retrying GPU memory reclaim…"
                    : `Reclaiming GPU memory… ${blackout()?.free_mb ?? 0} / ${blackout()?.required_mb ?? 0} MB`}
                </span>
              </Show>
              <Show when={stage() && blackout() === null}>
                <span class="image-progress-stage">{stage()}</span>
              </Show>
              <Show when={prog() !== null && blackout() === null}>
                <span class="image-progress-pct">{prog()}%</span>
              </Show>
            </div>
            <div class="image-progress-bar-track">
              <div
                class="image-progress-bar-fill"
                style={{ width: `${prog() ?? 0}%` }}
              />
            </div>
          </div>
        </div>
      </Show>
    </>
  );
};

export default ImageProgressChip;
