/** @vitest-environment jsdom */

import { render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App, { Markdown } from "./App";

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
});
