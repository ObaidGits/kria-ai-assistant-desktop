import { Component, Show } from "solid-js";
import type { Message } from "../stores/app";

interface Props {
  message: Message;
}

const MessageBubble: Component<Props> = (props) => {
  const roleClass = () => `message message-${props.message.role}`;

  return (
    <div class={roleClass()}>
      <div class="message-header">
        <span class="message-role">
          {props.message.role === "assistant" ? "KRIA" : props.message.role}
        </span>
        <span class="message-time">
          {new Date(props.message.timestamp).toLocaleTimeString()}
        </span>
      </div>

      <div class="message-content">
        {props.message.content}
      </div>

      <Show when={props.message.toolCalls?.length}>
        <div class="tool-calls">
          {props.message.toolCalls!.map((tc) => (
            <div class={`tool-call tool-call-${tc.status}`}>
              <span class="tool-name">{tc.name}</span>
              <span class="tool-status">{tc.status}</span>
              <Show when={tc.result}>
                <pre class="tool-result">{tc.result}</pre>
              </Show>
            </div>
          ))}
        </div>
      </Show>
    </div>
  );
};

export default MessageBubble;
