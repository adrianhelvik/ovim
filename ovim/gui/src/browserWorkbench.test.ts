import { createRoot, createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { BrowserState } from "./browserProtocol";
import { createBrowserWorkbench } from "./browserWorkbench";
import type { GuiSnapshot } from "./types";
import type { WorkbenchSelection } from "./workbench";

const invoke = vi.hoisted(() => vi.fn());
const nativeChannels = vi.hoisted(
    () =>
        [] as Array<{
            onmessage?: (state: unknown) => void;
        }>,
);
vi.mock("@tauri-apps/api/core", () => ({
    invoke,
    Channel: class {
        onmessage?: (state: unknown) => void;

        constructor() {
            nativeChannels.push(this);
        }
    },
}));

const sourceTabs: GuiSnapshot["tabs"] = [
    { id: 12, index: 0, title: "main.rs", active: true, modified: false },
];

beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    nativeChannels.length = 0;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
        callback(0);
        return 1;
    });
});

afterEach(() => vi.unstubAllGlobals());

describe("browser workbench controller", () => {
    it("ignores browser states older than the latest native revision", () =>
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
            const current = {
                sessionId: "browser-1",
                url: "https://example.com/",
                title: "Current title",
                visible: true,
                loading: false,
                documentId: 2,
                vimKeysEnabled: true,
                keyMode: "normal" as const,
            };

            controller.accept({
                revision: 4,
                sessions: [current],
                activeSessionId: current.sessionId,
                maxSessions: 8,
            });
            controller.accept({
                revision: 3,
                sessions: [{ ...current, title: "Stale title", documentId: 1 }],
                activeSessionId: current.sessionId,
                maxSessions: 8,
            });

            expect(controller.state().revision).toBe(4);
            expect(controller.state().sessions[0]?.title).toBe("Current title");
            dispose();
        }));

    it("does not let a pending activation steal focus from a newer selection", async () => {
        const target = {
            sessionId: "browser-2",
            url: "https://example.com/two",
            title: "Two",
            visible: false,
            loading: false,
            documentId: 1,
            vimKeysEnabled: true,
            keyMode: "normal" as const,
        };
        let resolveActivation!: (state: BrowserState) => void;
        invoke.mockImplementation((command: string) =>
            command === "gui_browser_activate"
                ? new Promise<BrowserState>((resolve) => {
                      resolveActivation = resolve;
                  })
                : Promise.resolve(),
        );

        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [selection, setSelection] =
                    createSignal<WorkbenchSelection>({
                        kind: "browser",
                        sessionId: "browser-1",
                    });
                const controller = createBrowserWorkbench({
                    native: true,
                    sourceTabs: () => sourceTabs,
                    includeVector: () => false,
                    selection,
                    setSelection,
                    setError: vi.fn(),
                });
                const first = { ...target, sessionId: "browser-1" };
                controller.accept({
                    revision: 1,
                    sessions: [first, target],
                    activeSessionId: first.sessionId,
                    maxSessions: 8,
                });

                controller.activate(target.sessionId);
                setSelection({ kind: "source", tabId: 12 });
                resolveActivation({
                    revision: 2,
                    sessions: [first, { ...target, visible: true }],
                    activeSessionId: target.sessionId,
                    maxSessions: 8,
                });

                queueMicrotask(() => {
                    expect(selection()).toEqual({
                        kind: "source",
                        tabId: 12,
                    });
                    dispose();
                    resolve();
                });
            }),
        );
    });

    it("owns the native browser state subscription", async () => {
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

                void controller.subscribe().then(() => {
                    expect(invoke).toHaveBeenCalledWith(
                        "gui_browser_subscribe",
                        { onEvent: nativeChannels[0] },
                    );
                    nativeChannels[0]?.onmessage?.({
                        revision: 1,
                        sessions: [],
                        maxSessions: 4,
                    });
                    expect(controller.state().maxSessions).toBe(4);
                    dispose();
                    resolve();
                });
            }),
        );
    });

    it("restores a closed session at its prior workbench position", async () => {
        const original = {
            sessionId: "browser-1",
            url: "https://example.com/article",
            title: "Article",
            visible: false,
            loading: false,
            documentId: 2,
            vimKeysEnabled: false,
            keyMode: "normal" as const,
        };
        const restored = {
            ...original,
            sessionId: "browser-2",
            vimKeysEnabled: true,
        };
        invoke.mockImplementation((command: string) => {
            if (command === "gui_browser_close")
                return Promise.resolve({
                    revision: 2,
                    sessions: [],
                    maxSessions: 8,
                });
            if (command === "gui_browser_open")
                return Promise.resolve({
                    revision: 3,
                    sessions: [restored],
                    activeSessionId: restored.sessionId,
                    maxSessions: 8,
                });
            if (command === "gui_browser_set_vim_keys")
                return Promise.resolve({
                    revision: 4,
                    sessions: [{ ...restored, vimKeysEnabled: false }],
                    activeSessionId: restored.sessionId,
                    maxSessions: 8,
                });
            return Promise.resolve();
        });
        const created = vi.fn();
        let disposeRoot = () => {};
        const controller = createRoot((dispose) => {
            disposeRoot = dispose;
            const [selection, setSelection] = createSignal<WorkbenchSelection>({
                kind: "browser",
                sessionId: original.sessionId,
            });
            return createBrowserWorkbench({
                native: true,
                sourceTabs: () => sourceTabs,
                includeVector: () => false,
                selection,
                setSelection,
                setError: vi.fn(),
                onSessionCreated: created,
            });
        });
        controller.accept({
            revision: 1,
            sessions: [original],
            activeSessionId: original.sessionId,
            maxSessions: 8,
        });
        created.mockClear();

        await controller.close(original.sessionId, 1);
        expect(controller.canRestore()).toBe(true);
        await controller.restore();

        expect(invoke).toHaveBeenCalledWith("gui_browser_open", {
            url: original.url,
        });
        expect(created).toHaveBeenCalledWith(restored.sessionId, {
            position: 1,
        });
        expect(invoke).toHaveBeenCalledWith("gui_browser_set_vim_keys", {
            sessionId: restored.sessionId,
            enabled: false,
        });
        expect(controller.canRestore()).toBe(false);
        disposeRoot();
    });

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
                revision: 1,
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

            controller.accept({ revision: 2, sessions: [], maxSessions: 8 });
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
            revision: 1,
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
            revision: number;
            sessions: (typeof original)[];
            maxSessions: number;
        }) => void;
        const closeReply = new Promise<{
            revision: number;
            sessions: (typeof original)[];
            maxSessions: number;
        }>((resolve) => {
            resolveClose = resolve;
        });
        invoke.mockImplementation((command: string) => {
            if (command === "gui_browser_navigate")
                return Promise.resolve({
                    revision: 2,
                    sessions: [navigated],
                    activeSessionId: navigated.sessionId,
                    maxSessions: 8,
                });
            if (command === "gui_browser_set_vim_keys")
                return Promise.resolve({
                    revision: 3,
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
                    revision: 1,
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
                    resolveClose({
                        revision: 4,
                        sessions: [],
                        maxSessions: 8,
                    });
                    await Promise.all([firstClose, duplicateClose]);

                    dispose();
                    resolve();
                })();
            }),
        );
    });
});
