/**
 * Page Object for the KRIA Settings Modal.
 *
 * Encapsulates interactions with the settings overlay.
 */
import { Page, Locator, expect } from "@playwright/test";

export class SettingsPage {
  readonly page: Page;

  // Locators
  readonly overlay: Locator;
  readonly modal: Locator;
  readonly closeButton: Locator;
  readonly llmModeSelect: Locator;
  readonly ttsVoiceSelect: Locator;
  readonly approvalCheckbox: Locator;
  readonly auditCheckbox: Locator;

  constructor(page: Page) {
    this.page = page;
    this.overlay = page.locator(".modal-overlay");
    this.modal = page.locator(".modal");
    this.closeButton = page.locator(".close-btn");
    this.llmModeSelect = page.locator(".settings-section").filter({ hasText: "LLM" }).locator("select");
    this.ttsVoiceSelect = page.locator(".settings-section").filter({ hasText: "Voice" }).locator("select");
    this.approvalCheckbox = page.locator(".settings-section").filter({ hasText: "Safety" }).locator('input[type="checkbox"]').first();
    this.auditCheckbox = page.locator(".settings-section").filter({ hasText: "Safety" }).locator('input[type="checkbox"]').nth(1);
  }

  /** Assert the modal is visible. */
  async expectOpen() {
    await expect(this.modal).toBeVisible();
  }

  /** Assert the modal is hidden. */
  async expectClosed() {
    await expect(this.modal).not.toBeVisible();
  }

  /** Close the settings modal via the X button. */
  async close() {
    await this.closeButton.click();
    await this.expectClosed();
  }

  /** Close by clicking on the overlay backdrop. */
  async closeByOverlay() {
    await this.overlay.click({ position: { x: 5, y: 5 } });
    await this.expectClosed();
  }

  /** Select LLM mode. */
  async selectLlmMode(value: "local" | "cloud") {
    await this.llmModeSelect.selectOption(value);
  }

  /** Select TTS voice. */
  async selectTtsVoice(value: string) {
    await this.ttsVoiceSelect.selectOption(value);
  }
}
