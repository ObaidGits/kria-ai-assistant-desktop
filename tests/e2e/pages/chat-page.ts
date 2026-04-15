/**
 * Page Object for the KRIA Chat UI.
 *
 * Maps SolidJS component selectors to reusable actions.
 * All waits are encapsulated here so tests stay declarative.
 */
import { Page, Locator, expect } from "@playwright/test";

export class ChatPage {
  readonly page: Page;

  // Locators
  readonly chatInput: Locator;
  readonly sendButton: Locator;
  readonly voiceButton: Locator;
  readonly messages: Locator;
  readonly thinkingIndicator: Locator;
  readonly settingsButton: Locator;

  constructor(page: Page) {
    this.page = page;
    this.chatInput = page.locator(".chat-input");
    this.sendButton = page.locator(".send-btn");
    this.voiceButton = page.locator(".voice-btn");
    this.messages = page.locator(".chat-messages .message-bubble");
    this.thinkingIndicator = page.locator(".thinking-indicator");
    this.settingsButton = page.locator("text=Settings").or(
      page.locator('[title="Settings"]')
    );
  }

  /** Navigate to the app root. */
  async goto() {
    await this.page.goto("/");
    await this.page.waitForLoadState("networkidle");
  }

  /** Type a message and send it. */
  async sendMessage(text: string) {
    await this.chatInput.fill(text);
    await this.sendButton.click();
  }

  /** Wait until the thinking indicator disappears. */
  async waitForResponse(timeoutMs = 10_000) {
    // Wait for thinking to appear then vanish
    try {
      await this.thinkingIndicator.waitFor({ state: "visible", timeout: 2000 });
    } catch {
      // May have already resolved
    }
    await this.thinkingIndicator.waitFor({ state: "hidden", timeout: timeoutMs });
  }

  /** Get the text content of the last message bubble. */
  async getLastMessage(): Promise<string> {
    const all = await this.messages.all();
    const last = all[all.length - 1];
    return (await last.textContent()) ?? "";
  }

  /** Get the total number of displayed messages. */
  async messageCount(): Promise<number> {
    return this.messages.count();
  }

  /** Toggle voice input. */
  async toggleVoice() {
    await this.voiceButton.click();
  }

  /** Assert the voice button has the "active" class. */
  async expectVoiceActive(active: boolean) {
    if (active) {
      await expect(this.voiceButton).toHaveClass(/active/);
    } else {
      await expect(this.voiceButton).not.toHaveClass(/active/);
    }
  }
}
