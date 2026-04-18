import { test, expect } from "@playwright/test";
import {
  clearTauriMockCommands,
  getTauriMockCommands,
  installTauriMockBridge,
  tauriMockEmit,
} from "../pages/tauri-mock-bridge";

const UI_URL = process.env.KRIA_UI_URL || "http://127.0.0.1:1420";

test.describe("Tauri mock bridge E2E", () => {
  test.beforeEach(async ({ page }) => {
    await installTauriMockBridge(page);
    await page.goto(UI_URL);
    await clearTauriMockCommands(page);
  });

  test("low-confidence modal selection sends forced-tool continuation", async ({ page }) => {
    await tauriMockEmit(page, "agent:tool_choice_required", {
      query: "check unread emails",
      confidence: 0.46,
      minConfidence: 0.55,
      candidates: [
        {
          name: "gw_gmail_inbox",
          label: "Gmail",
          reason: "Primary match from intent classifier",
          confidence: 0.46,
        },
        {
          name: "web_search",
          label: "Web Search",
          reason: "Best for broad web lookups",
          confidence: 0.6,
        },
      ],
    });

    await expect(page.getByRole("heading", { name: "Choose a Tool" })).toBeVisible();

    await page.getByRole("button", { name: /Gmail/ }).first().click();

    await expect(page.getByRole("heading", { name: "Choose a Tool" })).toBeHidden();

    const commands = await getTauriMockCommands(page);
    const sendMessageCalls = commands.filter((entry) => entry.cmd === "send_message");

    expect(sendMessageCalls.length).toBeGreaterThan(0);
    expect(sendMessageCalls[sendMessageCalls.length - 1].args).toMatchObject({
      message: "#tool:gw_gmail_inbox check unread emails",
    });
  });

  test("Google settings tab persists account and triggers runtime controls", async ({ page }) => {
    await page.getByRole("button", { name: "Configure Assistant" }).click();
    await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();

    await page.locator(".settings-nav-item", { hasText: "Google" }).click();

    const accountInput = page.getByPlaceholder("personal");
    await accountInput.fill("work");
    await accountInput.blur();

    await page.getByRole("button", { name: "Reconcile runtime" }).click();
    await page.getByRole("button", { name: "Restart runtime" }).click();

    await expect.poll(async () => {
      const commands = await getTauriMockCommands(page);
      return commands.length;
    }).toBeGreaterThan(0);

    const commands = await getTauriMockCommands(page);

    expect(
      commands.some(
        (entry) => entry.cmd === "set_google_workspace_account" && entry.args?.account === "work",
      ),
    ).toBeTruthy();
    expect(commands.some((entry) => entry.cmd === "reconcile_mcp_runtime")).toBeTruthy();
    expect(
      commands.some(
        (entry) => entry.cmd === "restart_mcp_server_runtime" && entry.args?.name === "gworkspace",
      ),
    ).toBeTruthy();
  });
});
