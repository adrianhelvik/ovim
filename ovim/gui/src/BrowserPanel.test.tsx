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

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

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
    vimKeysEnabled: true,
    keyMode: "normal",
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
    invoke.mockReset();
});

afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.replaceChildren();
});

describe("BrowserPanel", () => {
    it("opens, navigates, controls, and hides the shared native session", async () => {
        invoke.mockResolvedValue(undefined);
        const [active, setActive] = createSignal(true);
        const [obscured, setObscured] = createSignal(false);
        const [addressFocusRequest, setAddressFocusRequest] = createSignal<{
            serial: number;
            sessionId: string;
        }>();
        const [browserState, setBrowserState] = createSignal(state());
        const navigate = vi.fn(async (_sessionId: string, url: string) => {
            setBrowserState(state([session({ url })]));
        });
        const toolbar = vi.fn().mockResolvedValue(undefined);
        const setVimKeys = vi.fn().mockResolvedValue(undefined);
        const close = vi.fn(async () => {
            setBrowserState(state([], undefined));
        });
        const result = render(() => (
            <BrowserPanel
                native
                active={active()}
                obscured={obscured()}
                session={browserState().sessions[0]}
                addressFocusRequest={addressFocusRequest()}
                onNavigate={navigate}
                onToolbar={toolbar}
                onClose={close}
                onVimKeysChange={setVimKeys}
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
                expect(navigate).toHaveBeenCalledWith(
                    "browser-1",
                    "https://docs.rs/tauri",
                ),
            );

            fireEvent.click(screen.getByRole("button", { name: "Go back" }));
            fireEvent.click(
                screen.getByRole("button", { name: "Reload page" }),
            );
            expect(toolbar).toHaveBeenCalledWith("browser-1", "back");
            expect(toolbar).toHaveBeenCalledWith("browser-1", "reload");
            fireEvent.click(
                screen.getByRole("button", {
                    name: "Disable Vim-style page keys",
                }),
            );
            expect(setVimKeys).toHaveBeenCalledWith("browser-1", false);

            setAddressFocusRequest({ serial: 1, sessionId: "browser-1" });
            await waitFor(() =>
                expect(document.activeElement).toBe(
                    screen.getByLabelText("Browser address"),
                ),
            );
            fireEvent.keyDown(screen.getByLabelText("Browser address"), {
                key: "Escape",
            });
            expect(toolbar).toHaveBeenCalledWith("browser-1", "focus");

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
                onNavigate={vi.fn().mockResolvedValue(undefined)}
                onToolbar={vi.fn().mockResolvedValue(undefined)}
                onClose={vi.fn().mockResolvedValue(undefined)}
                onVimKeysChange={vi.fn().mockResolvedValue(undefined)}
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

    it("delegates navigation from an unloaded tab", async () => {
        invoke.mockResolvedValue(undefined);
        const [browserState] = createSignal(
            state([
                session({
                    url: "",
                    title: "",
                    visible: false,
                    documentId: 0,
                }),
            ]),
        );
        const navigate = vi.fn().mockResolvedValue(undefined);
        const result = render(() => (
            <BrowserPanel
                native
                active
                obscured={false}
                session={browserState().sessions[0]}
                onNavigate={navigate}
                onToolbar={vi.fn().mockResolvedValue(undefined)}
                onClose={vi.fn().mockResolvedValue(undefined)}
                onVimKeysChange={vi.fn().mockResolvedValue(undefined)}
            />
        ));
        try {
            expect(
                screen.getByText(/browser is created only when you navigate/i),
            ).toBeTruthy();
            const address = screen.getByLabelText("Browser address");
            await waitFor(() => expect(document.activeElement).toBe(address));
            expect(
                (
                    screen.getByRole("button", {
                        name: "Go back",
                    }) as HTMLButtonElement
                ).disabled,
            ).toBe(true);
            expect(
                (
                    screen.getByRole("button", {
                        name: "Reload page",
                    }) as HTMLButtonElement
                ).disabled,
            ).toBe(true);
            fireEvent.input(address, { target: { value: "docs.rs" } });
            fireEvent.submit(address.closest("form")!);
            await waitFor(() =>
                expect(navigate).toHaveBeenCalledWith(
                    "browser-1",
                    "https://docs.rs",
                ),
            );
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
        const toolbar = vi.fn().mockResolvedValue(undefined);
        const result = render(() => (
            <BrowserPanel
                native
                active
                obscured={false}
                session={sessions.find(
                    (candidate) => candidate.sessionId === activeSessionId(),
                )}
                onNavigate={vi.fn().mockResolvedValue(undefined)}
                onToolbar={toolbar}
                onClose={vi.fn().mockResolvedValue(undefined)}
                onVimKeysChange={vi.fn().mockResolvedValue(undefined)}
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
            expect(toolbar).toHaveBeenCalledWith("browser-2", "reload");
        } finally {
            result.unmount();
        }
    });

    it("does not reproject native bounds for state-only session updates", async () => {
        invoke.mockResolvedValue(undefined);
        const [current, setCurrent] = createSignal(session());
        const result = render(() => (
            <BrowserPanel
                native
                active
                obscured={false}
                session={current()}
                onNavigate={vi.fn().mockResolvedValue(undefined)}
                onToolbar={vi.fn().mockResolvedValue(undefined)}
                onClose={vi.fn().mockResolvedValue(undefined)}
                onVimKeysChange={vi.fn().mockResolvedValue(undefined)}
            />
        ));
        try {
            await waitFor(() =>
                expect(invoke).toHaveBeenCalledWith(
                    "gui_browser_set_bounds",
                    expect.anything(),
                ),
            );
            invoke.mockClear();

            setCurrent(
                session({
                    title: "Updated title",
                    loading: true,
                    documentId: 2,
                }),
            );
            await waitFor(() =>
                expect(screen.getByText("Updated title")).toBeTruthy(),
            );
            await Promise.resolve();

            expect(invoke).not.toHaveBeenCalledWith(
                "gui_browser_set_bounds",
                expect.anything(),
            );
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
