/** @vitest-environment jsdom */

import { fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, { ChatComposer, ChatMessageView, ChatPanel, ChatSetupCard, CodeWalkthrough, Markdown, chatSelectionText, imageExtension, isNearChatBottom, toolResultSummary } from "./App";
import { mockSnapshot } from "./mock";
import type { GuiAiChat, GuiCodeExplanation } from "./types";

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", ResizeObserverMock);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    font: "",
    measureText: () => ({ width: 8 }),
  } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLElement.prototype, "focus").mockImplementation(() => {});
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.replaceChildren();
});

describe("Ovim Solid workbench", () => {
  it("renders an interactive concept walkthrough from projected core state", () => {
    const onKey = vi.fn();
    const walkthrough: GuiCodeExplanation = {
      current: 1,
      total: 2,
      page: { kind: "concept", title: "Two layers of history", body: "Input recall and conversation navigation are **separate** concerns." },
      discussion: { state: "navigating", questionCount: 0, latestFailed: false },
    };

    render(() => <CodeWalkthrough walkthrough={walkthrough} onKey={onKey} />);

    expect(screen.getByRole("dialog", { name: "Two layers of history" })).toBeTruthy();
    expect(screen.getByText("separate").tagName).toBe("STRONG");
    fireEvent.click(screen.getByRole("button", { name: "Next →" }));
    fireEvent.click(screen.getByRole("button", { name: "Ask a question" }));
    expect(onKey).toHaveBeenNthCalledWith(1, "ArrowRight");
    expect(onKey).toHaveBeenNthCalledWith(2, " ");
  });

  it("renders code location and live answer state", () => {
    const walkthrough: GuiCodeExplanation = {
      current: 2,
      total: 2,
      page: { kind: "code", path: "src/main.rs", startLine: 12, endLine: 14, comment: "This branch owns the handoff." },
      discussion: { state: "answering", question: "Why here?", answer: "Because it owns the boundary.", questionCount: 1 },
    };

    render(() => <CodeWalkthrough walkthrough={walkthrough} onKey={() => {}} />);

    expect(screen.getByRole("dialog", { name: "src/main.rs:12–14" })).toBeTruthy();
    expect(screen.getByLabelText("Answering")).toBeTruthy();
    expect(screen.getByText("Because it owns the boundary.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Finish" }).hasAttribute("disabled")).toBe(true);
  });

  it("renders a keyboard-accessible editor projection from the snapshot", () => {
    const result = render(() => <App />);

    expect(screen.getByRole("navigation", { name: "Primary navigation" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
    expect(screen.getByLabelText("Ovim editor input")).toBeTruthy();
    expect(result.container.querySelectorAll(".code-line").length).toBeGreaterThan(10);
    expect(result.container.querySelector(".code-segment.cursor")).toBeTruthy();
  });

  it("sanitizes rendered AI markdown", () => {
    const result = render(() => (
      <Markdown text={'**safe**<img src="x" onerror="window.__unsafe = true">'} />
    ));

    expect(screen.getByText("safe").tagName).toBe("STRONG");
    expect(result.container.querySelector("img")?.hasAttribute("onerror")).toBe(false);
    expect(result.container.querySelector("script")).toBeNull();
  });

  it("renders the chat caret at the core UTF-8 cursor and pending images", () => {
    const result = render(() => <ChatComposer chat={{
      profile: "codex",
      reasoningEffort: "high",
      activity: "idle",
      waiting: false,
      input: "a界b",
      inputCursor: 4,
      pendingImages: ["diagram.png"],
      messages: [],
    }} />);

    const caret = result.container.querySelector(".chat-caret");
    expect(caret?.previousSibling?.textContent).toBe("a界");
    expect(caret?.nextSibling?.textContent).toBe("b");
    expect(screen.getByText("▧ diagram.png")).toBeTruthy();
  });

  it("returns DOM focus to the editor input when AI chat is activated", () => {
    render(() => <App />);
    const input = screen.getByLabelText("Ovim editor input");
    const focus = vi.mocked(HTMLElement.prototype.focus);
    focus.mockClear();

    fireEvent.click(document.querySelector<HTMLButtonElement>('[title^="AI chat"]')!);

    expect(focus).toHaveBeenCalledWith({ preventScroll: true });
    expect(focus.mock.instances).toContain(input);
  });

  it("follows chat updates until the reader scrolls away from the bottom", async () => {
    const initial: GuiAiChat = {
      profile: "codex",
      reasoningEffort: "high",
      activity: "idle",
      waiting: false,
      input: "",
      inputCursor: 0,
      pendingImages: [],
      messages: [{ role: "assistant", content: "First response", model: "codex", tools: [] }],
    };
    const [chat, setChat] = createSignal(initial);
    const result = render(() => <ChatPanel chat={chat()} focusInput={() => {}} />);
    const transcript = result.container.querySelector<HTMLElement>(".chat-messages")!;
    Object.defineProperties(transcript, {
      scrollHeight: { configurable: true, value: 600, writable: true },
      clientHeight: { configurable: true, value: 200 },
      scrollTop: { configurable: true, value: 0, writable: true },
    });

    await Promise.resolve();
    expect(transcript.scrollTop).toBe(600);

    transcript.scrollTop = 360;
    fireEvent.scroll(transcript);
    Object.defineProperty(transcript, "scrollHeight", { configurable: true, value: 700 });
    setChat({ ...initial, streaming: "Streaming while pinned" });
    await Promise.resolve();
    expect(transcript.scrollTop).toBe(700);

    transcript.scrollTop = 100;
    fireEvent.scroll(transcript);
    expect(screen.getByRole("button", { name: "↓ Jump to latest" })).toBeTruthy();
    setChat({ ...initial, streaming: "More streaming content" });
    await Promise.resolve();
    expect(transcript.scrollTop).toBe(100);

    fireEvent.click(screen.getByRole("button", { name: "↓ Jump to latest" }));
    expect(transcript.scrollTop).toBe(700);
  });

  it("uses a small threshold when deciding whether chat should follow", () => {
    expect(isNearChatBottom({ scrollHeight: 500, scrollTop: 260, clientHeight: 200 })).toBe(true);
    expect(isNearChatBottom({ scrollHeight: 500, scrollTop: 200, clientHeight: 200 })).toBe(false);
  });

  it("does not let stale editor overlays cover an active AI chat", () => {
    mockSnapshot.aiChat = {
      profile: "codex", reasoningEffort: "high", activity: "idle", waiting: false,
      input: "visible draft", inputCursor: 13, pendingImages: [], messages: [],
    };
    mockSnapshot.picker = {
      title: "Stale picker", query: "", selected: 0, total: 1,
      items: [{ index: 0, display: "Result", location: "src/main.rs", matched: [] }],
    };
    mockSnapshot.lspManager = { filter: "", selected: 0, showDetail: false, items: [] };
    mockSnapshot.hover = { content: "Stale hover" };
    mockSnapshot.completion = { selected: 0, items: [{ index: 0, label: "stale" }] };

    try {
      const result = render(() => <App />);
      expect(screen.getByLabelText("AI chat input").textContent).toContain("visible draft");
      expect(result.container.querySelector(".overlay-shade")).toBeNull();
      expect(result.container.querySelector(".hover-popover")).toBeNull();
      expect(result.container.querySelector(".completion-popover")).toBeNull();
    } finally {
      delete mockSnapshot.aiChat;
      delete mockSnapshot.picker;
      delete mockSnapshot.lspManager;
      delete mockSnapshot.hover;
      delete mockSnapshot.completion;
    }
  });

  it("shows blocking chat setup inline with masked input and working actions", () => {
    const onKey = vi.fn();
    render(() => <ChatSetupCard setup={{
      kind: "exaKey",
      title: "Enable web search",
      detail: "Paste an Exa API key or skip this optional setup.",
      maskedInput: "••••",
      inputCursor: 2,
      actions: [{ label: "Save key", key: "Enter" }, { label: "Not now", key: "Escape" }],
    }} onKey={onKey} />);

    const input = screen.getByLabelText("Exa API key input");
    expect(input.textContent).toBe("••••");
    expect(input.querySelector(".chat-caret")?.previousSibling?.textContent).toBe("••");
    fireEvent.click(screen.getByRole("button", { name: "Not now" }));
    expect(onKey).toHaveBeenCalledWith("Escape");
  });

  it("recognizes a browser selection made inside the chat transcript", () => {
    const transcript = document.createElement("div");
    transcript.className = "chat-messages";
    transcript.textContent = "copy this response";
    document.body.append(transcript);
    const range = document.createRange();
    range.selectNodeContents(transcript);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);

    expect(chatSelectionText(selection)).toBe("copy this response");

    transcript.className = "editor-content";
    expect(chatSelectionText(selection)).toBe("");
  });

  it("accepts only image formats supported by the core attachment path", () => {
    expect(imageExtension("image/png")).toBe("png");
    expect(imageExtension("image/jpeg")).toBe("jpg");
    expect(imageExtension("image/gif")).toBe("gif");
    expect(imageExtension("image/webp")).toBe("webp");
    expect(imageExtension("image/svg+xml")).toBeUndefined();
  });

  it("keeps tool results collapsed until their details are requested", async () => {
    const payload = "large tool payload that should start hidden";
    const result = render(() => <ChatMessageView message={{
      role: "tool", content: payload, toolName: "search_project", tools: [],
    }} />);

    expect(screen.getByText("search_project")).toBeTruthy();
    expect(screen.getByText(toolResultSummary(payload))).toBeTruthy();
    expect(result.container.querySelector(".markdown")).toBeNull();

    const details = result.container.querySelector<HTMLDetailsElement>("details")!;
    details.open = true;
    fireEvent(details, new Event("toggle"));
    await Promise.resolve();
    expect(screen.getByText(payload)).toBeTruthy();
  });

  it("collapses assistant tool-call lists by default", () => {
    const result = render(() => <ChatMessageView message={{
      role: "assistant", content: "I will inspect this.", model: "codex",
      tools: ["search_project", "read_file_at_path"],
    }} />);

    const details = result.container.querySelector<HTMLDetailsElement>(".tool-call-list")!;
    expect(details.open).toBe(false);
    expect(screen.getByText("2 tool calls")).toBeTruthy();
  });
});
