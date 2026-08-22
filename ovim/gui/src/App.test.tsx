/** @vitest-environment jsdom */

import { fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, { ChatComposer, Markdown } from "./App";

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
});
