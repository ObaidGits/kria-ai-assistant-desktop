import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@solidjs/testing-library";

const { sendMessageMock } = vi.hoisted(() => ({
  sendMessageMock: vi.fn(),
}));

vi.mock("../stores/app", () => ({
  appStore: {
    sendMessage: sendMessageMock,
  },
}));

import MessageBubble from "./MessageBubble";

describe("MessageBubble structured tool rendering", () => {
  beforeEach(() => {
    sendMessageMock.mockReset();
    vi.spyOn(window, "open").mockImplementation(() => null);
  });

  it("renders structured news cards with quick actions and metadata", async () => {
    const message = {
      id: "m-news-1",
      role: "assistant" as const,
      content: "",
      timestamp: 1710000000000,
      toolCalls: [
        {
          name: "search_news",
          args: { query: "ai chips" },
          status: "done" as const,
          metadata: {
            confidence: 0.88,
            sourceCount: 3,
            freshnessAgeHours: 2,
            regionMatch: true,
          },
          result: {
            count: 3,
            results: [
              {
                title: "AI Chip Supply Tightens",
                source: "Reuters",
                age: "2h ago",
                trust: "High",
                summary: "Suppliers report growing demand.",
                url: "https://example.com/chips",
                cross_referenced: "2 corroborating reports",
              },
            ],
          },
        },
      ],
    };

    const { container } = render(() => <MessageBubble message={message} />);

    expect(screen.getByText("3 sources")).toBeInTheDocument();
    expect(screen.getByText("88% confidence")).toBeInTheDocument();
    expect(screen.getByText("2h old")).toBeInTheDocument();
    expect(screen.getByText("region match")).toBeInTheDocument();

    await fireEvent.click(screen.getByText("search_news"));

    expect(screen.getByText("News Results:")).toBeInTheDocument();
    expect(screen.getByText("AI Chip Supply Tightens")).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(window.open).toHaveBeenCalledWith(
      "https://example.com/chips",
      "_blank",
      "noopener,noreferrer",
    );

    await fireEvent.click(screen.getByRole("button", { name: "Extract" }));
    expect(sendMessageMock).toHaveBeenCalledWith(
      "Fetch and summarize this article: https://example.com/chips",
    );

    const toolCalls = container.querySelector(".tool-calls");
    expect(toolCalls).toBeTruthy();
    expect(toolCalls?.innerHTML).toMatchSnapshot();
  });

  it("renders structured web cards and refresh quick action", async () => {
    const message = {
      id: "m-web-1",
      role: "assistant" as const,
      content: "",
      timestamp: 1710000000000,
      toolCalls: [
        {
          name: "web_search",
          args: { query: "robotics funding" },
          status: "done" as const,
          metadata: {
            confidence: 0.67,
            sourceCount: 2,
          },
          result: {
            count: 2,
            results: [
              {
                title: "Robotics startup raises series B",
                url: "https://example.com/robotics",
                snippet: "Funding round led by major VC firms.",
              },
            ],
          },
        },
      ],
    };

    render(() => <MessageBubble message={message} />);

    await fireEvent.click(screen.getByText("web_search"));

    expect(screen.getByText("Web Results:")).toBeInTheDocument();
    expect(screen.getByText("Robotics startup raises series B")).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(sendMessageMock).toHaveBeenCalledWith(
      "Get the latest live updates and key developments about: Robotics startup raises series B",
    );
  });

  it("renders structured Google calendar cards with meet-link actions", async () => {
    const message = {
      id: "m-gw-1",
      role: "assistant" as const,
      content: "",
      timestamp: 1710000000000,
      toolCalls: [
        {
          name: "gw_calendar_create",
          args: { summary: "Weekly Sync" },
          status: "done" as const,
          metadata: {
            confidence: 0.74,
            sourceCount: 1,
          },
          result: {
            provider: "google_workspace",
            kind: "calendar",
            tool: "createCalendarEvent",
            data: {
              events: [
                {
                  summary: "Weekly Sync",
                  organizer: "owner@example.com",
                  start: "2026-04-18T10:00:00Z",
                  hangoutLink: "https://meet.google.com/abc-defg-hij",
                },
              ],
            },
          },
        },
      ],
    };

    render(() => <MessageBubble message={message} />);

    await fireEvent.click(screen.getByText("gw_calendar_create"));

    expect(screen.getByText("Google calendar:")).toBeInTheDocument();
    expect(screen.getByText("Meet link available")).toBeInTheDocument();
    expect(screen.getByText("Weekly Sync")).toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Join Meet" }));

    expect(window.open).toHaveBeenCalledWith(
      "https://meet.google.com/abc-defg-hij",
      "_blank",
      "noopener,noreferrer",
    );
  });

  it("renders mocked Google matrix cards for gmail/drive/docs/sheets/slides/forms", async () => {
    const cases = [
      {
        tool: "gw_gmail_search",
        kind: "gmail",
        title: "Inbox digest",
        payload: {
          messages: [
            {
              subject: "Inbox digest",
              from: "owner@example.com",
              date: "2026-04-18",
              url: "https://mail.google.com/mail/u/0/#inbox",
            },
          ],
        },
        link: "https://mail.google.com/mail/u/0/#inbox",
      },
      {
        tool: "gw_drive_search",
        kind: "drive",
        title: "Quarterly Plan",
        payload: {
          files: [
            {
              name: "Quarterly Plan",
              updated: "2026-04-18T09:00:00Z",
              webViewLink: "https://drive.google.com/file/d/123/view",
            },
          ],
        },
        link: "https://drive.google.com/file/d/123/view",
      },
      {
        tool: "gw_docs_read",
        kind: "docs",
        title: "Roadmap Doc",
        payload: {
          items: [
            {
              title: "Roadmap Doc",
              htmlLink: "https://docs.google.com/document/d/123/edit",
              snippet: "Roadmap summary",
            },
          ],
        },
        link: "https://docs.google.com/document/d/123/edit",
      },
      {
        tool: "gw_sheets_read",
        kind: "sheets",
        title: "Revenue Sheet",
        payload: {
          rows: [
            {
              title: "Revenue Sheet",
              url: "https://docs.google.com/spreadsheets/d/123/edit",
              snippet: "Q2 numbers",
            },
          ],
        },
        link: "https://docs.google.com/spreadsheets/d/123/edit",
      },
      {
        tool: "gw_slides_read",
        kind: "slides",
        title: "Launch Deck",
        payload: {
          items: [
            {
              title: "Launch Deck",
              url: "https://docs.google.com/presentation/d/123/edit",
            },
          ],
        },
        link: "https://docs.google.com/presentation/d/123/edit",
      },
      {
        tool: "gw_forms_list",
        kind: "forms",
        title: "Hiring Survey",
        payload: {
          forms: [
            {
              title: "Hiring Survey",
              url: "https://docs.google.com/forms/d/123/edit",
            },
          ],
        },
        link: "https://docs.google.com/forms/d/123/edit",
      },
    ];

    for (const item of cases) {
      const message = {
        id: `m-${item.kind}`,
        role: "assistant" as const,
        content: "",
        timestamp: 1710000000000,
        toolCalls: [
          {
            name: item.tool,
            args: {},
            status: "done" as const,
            result: {
              provider: "google_workspace",
              kind: item.kind,
              tool: item.tool,
              data: item.payload,
            },
          },
        ],
      };

      const mounted = render(() => <MessageBubble message={message} />);
      await fireEvent.click(screen.getByText(item.tool));

      expect(screen.getByText(`Google ${item.kind}:`)).toBeInTheDocument();
      expect(screen.getByText(item.title)).toBeInTheDocument();

      await fireEvent.click(screen.getByRole("button", { name: "Open" }));
      expect(window.open).toHaveBeenLastCalledWith(item.link, "_blank", "noopener,noreferrer");

      mounted.unmount();
    }
  });

  it("shows actionable open link only for verified create results", async () => {
    const message = {
      id: "m-gw-create-verified",
      role: "assistant" as const,
      content: "",
      timestamp: 1710000000000,
      toolCalls: [
        {
          name: "gw_docs_create",
          args: { title: "Quarterly Plan" },
          status: "done" as const,
          result: {
            provider: "google_workspace",
            kind: "docs",
            tool: "createDocument",
            data: {
              resource: "document",
              title: "Quarterly Plan",
              status: "created_verified",
              verified: true,
              document_id: "doc_verified_123",
              url: "https://docs.google.com/document/d/doc_verified_123/edit",
            },
          },
        },
      ],
    };

    render(() => <MessageBubble message={message} />);

    await fireEvent.click(screen.getByText("gw_docs_create"));

    expect(screen.getByText("Verified create")).toBeInTheDocument();
    expect(screen.queryByText("Create unverified")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(window.open).toHaveBeenCalledWith(
      "https://docs.google.com/document/d/doc_verified_123/edit",
      "_blank",
      "noopener,noreferrer",
    );
  });

  it("shows recovery guidance and hides open action for created_unverified", async () => {
    const message = {
      id: "m-gw-create-unverified",
      role: "assistant" as const,
      content: "",
      timestamp: 1710000000000,
      toolCalls: [
        {
          name: "gw_sheets_create",
          args: { title: "Monthly Budget" },
          status: "done" as const,
          result: {
            provider: "google_workspace",
            kind: "sheets",
            tool: "createSpreadsheet",
            data: {
              resource: "spreadsheet",
              title: "Monthly Budget",
              status: "created_unverified",
              verified: false,
              spreadsheet_id: "sheet_unverified_123",
              url: "https://docs.google.com/spreadsheets/d/sheet_unverified_123/edit",
              verification_error: "Could not verify spreadsheet read after create",
            },
          },
        },
      ],
    };

    render(() => <MessageBubble message={message} />);

    await fireEvent.click(screen.getByText("gw_sheets_create"));

    expect(screen.getByText("Create unverified")).toBeInTheDocument();
    expect(screen.getByText("Recovery guidance:")).toBeInTheDocument();
    expect(screen.getByText("Could not verify spreadsheet read after create")).toBeInTheDocument();
    expect(screen.getByText("Open hidden until verification")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open" })).not.toBeInTheDocument();
  });

  it("does not render gmail category labels from metadata", async () => {
    const message = {
      id: "m-gw-gmail-category",
      role: "assistant" as const,
      content: "",
      timestamp: 1710000000000,
      toolCalls: [
        {
          name: "gw_gmail_inbox",
          args: { query: "is:unread" },
          status: "done" as const,
          result: {
            provider: "google_workspace",
            kind: "gmail",
            tool: "searchEmails",
            data: {
              messages: [
                {
                  subject: "Welcome offer",
                  from: "Make <info@make.com>",
                  labels: ["CATEGORY_PROMOTIONS", "UNREAD", "INBOX"],
                  url: "https://mail.google.com/mail/u/0/#inbox/abc123",
                },
              ],
            },
          },
        },
      ],
    };

    render(() => <MessageBubble message={message} />);

    await fireEvent.click(screen.getByText("gw_gmail_inbox"));

    expect(screen.queryByText("Promotional")).not.toBeInTheDocument();
    expect(screen.getByText("Make <info@make.com>")).toBeInTheDocument();
  });
});
