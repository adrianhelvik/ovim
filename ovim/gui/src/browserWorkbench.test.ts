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
                    expect(invoke).toHaveBeenCalledWith("gui_browser_open");
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
});
