/**
 * End-to-End tests for the KRIA desktop UI.
 *
 * Full user journeys exercised through the browser:
 *   1. Open app → Send message → Receive response
 *   2. Open Settings → Modify → Close
 *   3. Toggle voice input on/off
 *
 * Uses Page Object Model for all interactions.
 */
import { test, expect } from "@playwright/test";
import { ChatPage } from "../pages/chat-page";
import { SettingsPage } from "../pages/settings-page";

// The UI dev server URL (Vite on port 5173 or Tauri webview)
const UI_URL = process.env.KRIA_UI_URL || "http://localhost:5173";

// ── Full chat journey ─────────────────────────────────────────────

test.describe("Chat Journey", () => {
  let chatPage: ChatPage;

  test.beforeEach(async ({ page }) => {
    chatPage = new ChatPage(page);
    await page.goto(UI_URL);
  });

  test("user can send a message and see it appear", async () => {
    await chatPage.sendMessage("Hello KRIA!");

    // User message should appear in the chat
    const count = await chatPage.messageCount();
    expect(count).toBeGreaterThanOrEqual(1);

    const lastMsg = await chatPage.getLastMessage();
    expect(lastMsg).toContain("Hello KRIA!");
  });

  test("send button is disabled when input is empty", async () => {
    await expect(chatPage.sendButton).toBeDisabled();
  });

  test("pressing Enter sends the message", async ({ page }) => {
    await chatPage.chatInput.fill("Enter test");
    await chatPage.chatInput.press("Enter");

    const count = await chatPage.messageCount();
    expect(count).toBeGreaterThanOrEqual(1);
  });

  test("Shift+Enter does not send (allows newline)", async ({ page }) => {
    const countBefore = await chatPage.messageCount();
    await chatPage.chatInput.fill("line one");
    await chatPage.chatInput.press("Shift+Enter");

    // Message count should not have increased
    const countAfter = await chatPage.messageCount();
    expect(countAfter).toBe(countBefore);
  });
});

// ── Settings journey ──────────────────────────────────────────────

test.describe("Settings Journey", () => {
  test("user can open and close settings modal", async ({ page }) => {
    await page.goto(UI_URL);
    const chatPage = new ChatPage(page);

    // Open settings (the exact trigger depends on the UI — adapt selector)
    await chatPage.settingsButton.click();

    const settings = new SettingsPage(page);
    await settings.expectOpen();

    // Close via X button
    await settings.close();
  });

  test("settings modal closes when clicking overlay", async ({ page }) => {
    await page.goto(UI_URL);
    const chatPage = new ChatPage(page);

    await chatPage.settingsButton.click();
    const settings = new SettingsPage(page);
    await settings.expectOpen();

    await settings.closeByOverlay();
  });

  test("LLM mode dropdown has local and cloud options", async ({ page }) => {
    await page.goto(UI_URL);
    const chatPage = new ChatPage(page);
    await chatPage.settingsButton.click();

    const settings = new SettingsPage(page);
    const options = settings.llmModeSelect.locator("option");
    const texts = await options.allTextContents();

    expect(texts.some((t) => t.toLowerCase().includes("local"))).toBeTruthy();
    expect(texts.some((t) => t.toLowerCase().includes("cloud"))).toBeTruthy();
  });
});

// ── Voice toggle journey ──────────────────────────────────────────

test.describe("Voice Toggle", () => {
  test("toggling voice activates and deactivates", async ({ page }) => {
    await page.goto(UI_URL);
    const chatPage = new ChatPage(page);

    // Voice should start inactive
    await chatPage.expectVoiceActive(false);

    // Activate
    await chatPage.toggleVoice();
    await chatPage.expectVoiceActive(true);

    // Deactivate
    await chatPage.toggleVoice();
    await chatPage.expectVoiceActive(false);
  });
});
