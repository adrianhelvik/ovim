import { createRoot, createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createBrowserWorkbench } from "./browserWorkbench";
import type { GuiSnapshot } from "./types";
import type { WorkbenchSelection } from "./workbench";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const sourceTabs: GuiSnapshot["tabs"] = [
    { id: 12, index: 0, title: "main.rs", active: true, modified: false },
];

beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
        callback(0);
        return 1;
    });
});

afterEach(() => vi.unstubAllGlobals());

describe("browser workbench controller", () => {
    it("projects a durable agent presentation and falls back after close", () =>
        createRoot((dispose) => {
            const [selection, setSelection] = createSignal<WorkbenchSelection>({
                kind: "source",
                tabId: 12,
            });
            const controller = createBrowserWorkbench({
                native: true,
                sourceTabs: () => sourceTabs,
                includeVector: () => false,
                selection,
                setSelection,
                setError: vi.fn(),
            });
            const session = {
                sessionId: "browser-3",
                url: "https://example.com/",
                title: "Example",
                visible: false,
                loading: false,
                documentId: 1,
                vimKeysEnabled: true,
                keyMode: "normal" as const,
            };
            controller.accept({
                sessions: [session],
                activeSessionId: session.sessionId,
                maxSessions: 8,
                presentationRequest: {
                    revision: 9,
                    sessionId: session.sessionId,
                },
            });
            expect(selection()).toEqual({
                kind: "browser",
                sessionId: "browser-3",
            });
            expect(invoke).toHaveBeenCalledWith(
                "gui_browser_ack_presentation",
                { revision: 9 },
            );

            controller.accept({ sessions: [], maxSessions: 8 });
            expect(selection()).toEqual({ kind: "source", tabId: 12 });
            dispose();
        }));

    it("opens one unloaded session through the native host", async () => {
        const session = {
            sessionId: "browser-1",
            url: "",
            title: "",
            visible: false,
            loading: false,
            documentId: 0,
            vimKeysEnabled: true,
            keyMode: "normal" as const,
        };
        invoke.mockResolvedValue({
            sessions: [session],
            activeSessionId: session.sessionId,
            maxSessions: 8,
        });
        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [selection, setSelection] =
                    createSignal<WorkbenchSelection>({
                        kind: "source",
                        tabId: 12,
                    });
                const controller = createBrowserWorkbench({
                    native: true,
                    sourceTabs: () => sourceTabs,
                    includeVector: () => false,
                    selection,
                    setSelection,
                    setError: vi.fn(),
                });
                void controller.open().then(() => {
                    expect(invoke).toHaveBeenCalledWith(
                        "gui_browser_open",
                        undefined,
                    );
                    expect(selection()).toEqual({
                        kind: "browser",
                        sessionId: "browser-1",
                    });
                    dispose();
                    resolve();
                });
            }),
        );
    });

    it("owns navigation, toolbar, focus, and idempotent close mutations", async () => {
        const original = {
            sessionId: "browser-1",
            url: "",
            title: "",
            visible: false,
            loading: false,
            documentId: 0,
            vimKeysEnabled: true,
            keyMode: "normal" as const,
        };
        const navigated = {
            ...original,
            url: "https://example.com/",
            title: "Example Domain",
            visible: true,
            documentId: 1,
        };
        let resolveClose!: (state: {
            sessions: (typeof original)[];
            maxSessions: number;
        }) => void;
        const closeReply = new Promise<{
            sessions: (typeof original)[];
            maxSessions: number;
        }>((resolve) => {
            resolveClose = resolve;
        });
        invoke.mockImplementation((command: string) => {
            if (command === "gui_browser_navigate")
                return Promise.resolve({
                    sessions: [navigated],
                    activeSessionId: navigated.sessionId,
                    maxSessions: 8,
                });
            if (command === "gui_browser_set_vim_keys")
                return Promise.resolve({
                    sessions: [
                        {
                            ...navigated,
                            vimKeysEnabled: false,
                            keyMode: "normal",
                        },
                    ],
                    activeSessionId: navigated.sessionId,
                    maxSessions: 8,
                });
            if (command === "gui_browser_close") return closeReply;
            return Promise.resolve();
        });

        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [selection, setSelection] =
                    createSignal<WorkbenchSelection>({
                        kind: "browser",
                        sessionId: original.sessionId,
                    });
                const controller = createBrowserWorkbench({
                    native: true,
                    sourceTabs: () => sourceTabs,
                    includeVector: () => false,
                    selection,
                    setSelection,
                    setError: vi.fn(),
                });
                controller.accept({
                    sessions: [original],
                    activeSessionId: original.sessionId,
                    maxSessions: 8,
                });

                void (async () => {
                    await controller.navigate(
                        original.sessionId,
                        navigated.url,
                    );
                    expect(invoke).toHaveBeenCalledWith(
                        "gui_browser_navigate",
                        {
                            sessionId: original.sessionId,
                            url: navigated.url,
                        },
                    );
                    expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                        sessionId: original.sessionId,
                        action: "focus",
                    });

                    await controller.toolbar(original.sessionId, "back", 2);
                    expect(invoke).toHaveBeenCalledWith("gui_browser_toolbar", {
                        sessionId: original.sessionId,
                        action: "back",
                        count: 2,
                    });

                    await controller.setVimKeys(original.sessionId, false);
                    expect(invoke).toHaveBeenCalledWith(
                        "gui_browser_set_vim_keys",
                        {
                            sessionId: original.sessionId,
                            enabled: false,
                        },
                    );
                    expect(controller.activeSession()?.vimKeysEnabled).toBe(
                        false,
                    );

                    const firstClose = controller.close(original.sessionId);
                    const duplicateClose = controller.close(original.sessionId);
                    expect(
                        invoke.mock.calls.filter(
                            ([command]) => command === "gui_browser_close",
                        ),
                    ).toHaveLength(1);
                    resolveClose({ sessions: [], maxSessions: 8 });
                    await Promise.all([firstClose, duplicateClose]);

                    dispose();
                    resolve();
                })();
            }),
        );
    });
});
