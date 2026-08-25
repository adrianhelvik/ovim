/** @vitest-environment jsdom */

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import BrowserPanel, {
    browserTabTitle,
    type BrowserSession,
    type BrowserState,
} from "./BrowserPanel";

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

const state = (
    sessions: BrowserSession[] = [session()],
    activeSessionId: string | undefined = sessions[0]?.sessionId,
): BrowserState => ({ sessions, activeSessionId, maxSessions: 8 });

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
                if (command === "gui_browser_navigate")
                    return Promise.resolve(
                        state([session({ url: String(args?.url) })]),
                    );
                if (command === "gui_browser_close")
                    return Promise.resolve(state([], undefined));
                return Promise.resolve();
            },
        );
        const [active, setActive] = createSignal(true);
        const [obscured, setObscured] = createSignal(false);
        const [browserState, setBrowserState] = createSignal(state());
        const result = render(() => (
            <BrowserPanel
                native
                active={active()}
                obscured={obscured()}
                session={browserState().sessions[0]}
                onState={setBrowserState}
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
                    sessionId: "browser-1",
                    url: "https://docs.rs/tauri",
                }),
            );

            fireEvent.click(screen.getByRole("button", { name: "Go back" }));
            fireEvent.click(
                screen.getByRole("button", { name: "Reload page" }),
            );
            expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                sessionId: "browser-1",
                action: "back",
            });
            expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                sessionId: "browser-1",
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
            fireEvent.click(
                screen.getByRole("button", { name: "Close browser session" }),
            );
            await waitFor(() =>
                expect(browserState().sessions).toHaveLength(0),
            );
            setActive(false);
        } finally {
            result.unmount();
        }
    });

    it("explains the desktop requirement in a web preview", () => {
        const result = render(() => (
            <BrowserPanel
                native={false}
                active
                obscured={false}
                session={session()}
                onState={() => {}}
            />
        ));
        try {
            expect(
                screen.getByText(
                    "The embedded browser runs in the Ovim desktop app",
                ),
            ).toBeTruthy();
            expect(invoke).not.toHaveBeenCalled();
        } finally {
            result.unmount();
        }
    });

    it("switches the toolbar and address between independent sessions", async () => {
        invoke.mockResolvedValue(undefined);
        const sessions = [
            session(),
            session({
                sessionId: "browser-2",
                url: "https://docs.rs/",
                title: "Docs.rs",
                documentId: 3,
            }),
        ];
        const [activeSessionId, setActiveSessionId] = createSignal("browser-1");
        const result = render(() => (
            <BrowserPanel
                native
                active
                obscured={false}
                session={sessions.find(
                    (candidate) => candidate.sessionId === activeSessionId(),
                )}
                onState={() => {}}
            />
        ));
        try {
            expect(
                (screen.getByLabelText("Browser address") as HTMLInputElement)
                    .value,
            ).toBe("https://example.com/");

            setActiveSessionId("browser-2");
            await waitFor(() =>
                expect(
                    (
                        screen.getByLabelText(
                            "Browser address",
                        ) as HTMLInputElement
                    ).value,
                ).toBe("https://docs.rs/"),
            );
            fireEvent.click(
                screen.getByRole("button", { name: "Reload page" }),
            );
            expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                sessionId: "browser-2",
                action: "reload",
            });
        } finally {
            result.unmount();
        }
    });

    it("derives stable tab titles before a document title is available", () => {
        expect(browserTabTitle(session())).toBe("Example Domain");
        expect(
            browserTabTitle(session({ title: "", url: "https://docs.rs/" })),
        ).toBe("docs.rs");
    });
});
