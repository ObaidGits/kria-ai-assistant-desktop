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
});
