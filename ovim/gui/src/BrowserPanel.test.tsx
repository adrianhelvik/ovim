/** @vitest-environment jsdom */

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import BrowserPanel, { type BrowserSession } from "./BrowserPanel";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

class ResizeObserverMock {
    observe() {}
    disconnect() {}
}

const session = (overrides: Partial<BrowserSession> = {}): BrowserSession => ({
    sessionId: "browser-1",
    url: "https://example.com/",
    title: "Example Domain",
    visible: true,
    loading: false,
    documentId: 1,
    ...overrides,
});

beforeEach(() => {
    let frame = 0;
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
        const id = ++frame;
        queueMicrotask(() => callback(0));
        return id;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
        x: 12,
        y: 84,
        left: 12,
        top: 84,
        right: 812,
        bottom: 684,
        width: 800,
        height: 600,
        toJSON: () => ({}),
    });
    listen.mockReset();
    listen.mockResolvedValue(() => {});
    invoke.mockReset();
});

afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.replaceChildren();
});

describe("BrowserPanel", () => {
    it("opens, navigates, controls, and hides the shared native session", async () => {
        invoke.mockImplementation(
            (command: string, args?: Record<string, unknown>) => {
                if (command === "gui_browser_open")
                    return Promise.resolve({ session: session() });
                if (command === "gui_browser_state") return Promise.resolve({});
                if (command === "gui_browser_navigate")
                    return Promise.resolve({
                        session: session({ url: String(args?.url) }),
                    });
                return Promise.resolve();
            },
        );
        const [active, setActive] = createSignal(true);
        const [obscured, setObscured] = createSignal(false);
        const onClose = vi.fn();
        const result = render(() => (
            <BrowserPanel
                native
                active={active()}
                obscured={obscured()}
                onClose={onClose}
            />
        ));
        try {
            expect(
                await screen.findByDisplayValue("https://example.com/"),
            ).toBeTruthy();
            await waitFor(() =>
                expect(invoke).toHaveBeenCalledWith("gui_browser_set_bounds", {
                    bounds: {
                        x: 12,
                        y: 84,
                        width: 800,
                        height: 600,
                        visible: true,
                    },
                }),
            );

            fireEvent.input(screen.getByLabelText("Browser address"), {
                target: { value: "docs.rs/tauri" },
            });
            fireEvent.submit(
                screen.getByLabelText("Browser address").closest("form")!,
            );
            await waitFor(() =>
                expect(invoke).toHaveBeenCalledWith("gui_browser_navigate", {
                    url: "https://docs.rs/tauri",
                }),
            );

            fireEvent.click(screen.getByRole("button", { name: "Go back" }));
            fireEvent.click(
                screen.getByRole("button", { name: "Reload page" }),
            );
            expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                action: "back",
            });
            expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                action: "reload",
            });

            setObscured(true);
            await waitFor(() =>
                expect(invoke).toHaveBeenCalledWith(
                    "gui_browser_set_bounds",
                    expect.objectContaining({
                        bounds: expect.objectContaining({ visible: false }),
                    }),
                ),
            );
            setActive(false);
        } finally {
            result.unmount();
        }
    });

    it("explains the desktop requirement in a web preview", () => {
        const onClose = vi.fn();
        const result = render(() => (
            <BrowserPanel
                native={false}
                active
                obscured={false}
                onClose={onClose}
            />
        ));
        try {
            expect(
                screen.getByText(
                    "The embedded browser runs in the Ovim desktop app",
                ),
            ).toBeTruthy();
            fireEvent.click(
                screen.getByRole("button", {
                    name: "Close browser session",
                }),
            );
            expect(onClose).toHaveBeenCalledOnce();
            expect(invoke).not.toHaveBeenCalled();
        } finally {
            result.unmount();
        }
    });
});
