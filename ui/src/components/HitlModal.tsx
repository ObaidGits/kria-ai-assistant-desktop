import { Component, Show } from "solid-js";
import { appStore } from "../stores/app";

const HitlModal: Component = () => {
  const { hitlRequest, approveAction, denyAction } = appStore;

  return (
    <Show when={hitlRequest()}>
      {(req) => (
        <div class="modal-overlay hitl-overlay">
          <div class="modal hitl-modal">
            <div class="modal-header hitl-header">
              <h2>⚠ Action Requires Approval</h2>
            </div>

            <div class="modal-body">
              <div class="hitl-risk">
                Risk Level: <span class={`risk-badge risk-${req().riskLevel.toLowerCase()}`}>
                  {req().riskLevel}
                </span>
              </div>

              <div class="hitl-tool">
                <strong>Tool:</strong> {req().toolName}
              </div>

              <div class="hitl-args">
                <strong>Arguments:</strong>
                <pre>{JSON.stringify(req().args, null, 2)}</pre>
              </div>

              <div class="hitl-reason">
                <strong>Reason:</strong> {req().reason}
              </div>
            </div>

            <div class="modal-footer hitl-actions">
              <button
                class="btn-deny"
                onClick={() => denyAction(req().requestId, "User denied")}
              >
                Deny
              </button>
              <button
                class="btn-approve"
                onClick={() => approveAction(req().requestId)}
              >
                Approve
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
};

export default HitlModal;
