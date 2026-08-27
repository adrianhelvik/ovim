import { createRoot, createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createBrowserNavigation } from "./browserNavigation";
import type { BrowserSession, BrowserState } from "./browserProtocol";
import type { BrowserWorkbenchController } from "./browserWorkbench";
import type { WorkbenchSelection, WorkbenchTabReference } from "./workbench";

const session: BrowserSession = {
    sessionId: "browser-1",
    url: "https://example.com/",
    title: "Example",
    visible: true,
    loading: false,
    documentId: 1,
    vimKeysEnabled: true,
    keyMode: "normal",
};

const fakeWorkbench = (
    state: () => BrowserState,
    setState: (state: BrowserState) => void,
) => {
    const close = vi.fn(async (sessionId: string) => {
        setState({
            ...state(),
            sessions: state().sessions.filter(
                (candidate) => candidate.sessionId !== sessionId,
            ),
            activeSessionId: undefined,
        });
    });
    return {
        state,
        opening: () => false,
        canRestore: () => false,
        activeSessionId: () => state().activeSessionId,
        activeSession: () => state().sessions[0],
        accept: setState,
        open: vi.fn(async () => undefined),
        close,
        restore: vi.fn(async () => {}),
        navigate: vi.fn(async () => {}),
        toolbar: vi.fn(async () => {}),
        setVimKeys: vi.fn(async () => {}),
        activate: vi.fn(),
        present: vi.fn(),
        focus: vi.fn(async () => {}),
    } as unknown as BrowserWorkbenchController;
};

afterEach(() => vi.unstubAllGlobals());

describe("browser navigation controller", () => {
    it("closes a selected tab and selects its workbench neighbor", async () => {
        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [state, setState] = createSignal<BrowserState>({
                    revision: 1,
                    sessions: [session],
                    activeSessionId: session.sessionId,
                    maxSessions: 8,
                });
                const workbench = fakeWorkbench(state, setState);
                const [selection] = createSignal<WorkbenchSelection>({
                    kind: "browser",
                    sessionId: session.sessionId,
                });
                const tabs: WorkbenchTabReference[] = [
                    {
                        id: "source:1",
                        kind: "source",
                        index: 0,
                        tabId: 1,
                    },
                    {
                        id: "browser:browser-1",
                        kind: "browser",
                        sessionId: session.sessionId,
                    },
                    {
                        id: "source:2",
                        kind: "source",
                        index: 1,
                        tabId: 2,
                    },
                ];
                const selectReference = vi.fn();
                const navigation = createBrowserNavigation({
                    workbench,
                    tabs: () => tabs,
                    selection,
                    selectTab: vi.fn(() => true),
                    selectReference,
                    focusSource: vi.fn(),
                    setError: vi.fn(),
                });

                void navigation.closeTab(session.sessionId).then(() => {
                    expect(workbench.close).toHaveBeenCalledWith(
                        session.sessionId,
                        1,
                    );
                    expect(selectReference).toHaveBeenCalledWith(tabs[2]);
                    dispose();
                    resolve();
                });
            }),
        );
    });

    it("owns command focus and tab selection state", async () => {
        vi.stubGlobal(
            "requestAnimationFrame",
            (callback: FrameRequestCallback) => {
                callback(0);
                return 1;
            },
        );
        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [state, setState] = createSignal<BrowserState>({
                    revision: 1,
                    sessions: [session],
                    activeSessionId: session.sessionId,
                    maxSessions: 8,
                });
                const workbench = fakeWorkbench(state, setState);
                const [selection] = createSignal<WorkbenchSelection>({
                    kind: "browser",
                    sessionId: session.sessionId,
                });
                const tabs: WorkbenchTabReference[] = [
                    {
                        id: "source:1",
                        kind: "source",
                        index: 0,
                        tabId: 1,
                    },
                    {
                        id: "browser:browser-1",
                        kind: "browser",
                        sessionId: session.sessionId,
                    },
                ];
                const selectTab = vi.fn(() => true);
                const navigation = createBrowserNavigation({
                    workbench,
                    tabs: () => tabs,
                    selection,
                    selectTab,
                    selectReference: vi.fn(),
                    focusSource: vi.fn(),
                    setError: vi.fn(),
                });

                navigation.openCommand(session.sessionId);
                expect(workbench.present).toHaveBeenCalledWith(
                    session.sessionId,
                );
                expect(navigation.commandRequest()).toEqual({
                    serial: 1,
                    sessionId: session.sessionId,
                });

                void navigation.executeCommand("tabprev").then((result) => {
                    expect(result).toEqual({ ok: true });
                    expect(selectTab).toHaveBeenCalledWith(0);
                    navigation.dismissCommand();
                    expect(workbench.focus).not.toHaveBeenCalled();
                    dispose();
                    resolve();
                });
            }),
        );
    });

    it("opens a fresh browser tab from the browser command context", async () => {
        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [state, setState] = createSignal<BrowserState>({
                    revision: 1,
                    sessions: [session],
                    activeSessionId: session.sessionId,
                    maxSessions: 8,
                });
                const workbench = fakeWorkbench(state, setState);
                vi.mocked(workbench.open).mockResolvedValue("browser-2");
                const [selection] = createSignal<WorkbenchSelection>({
                    kind: "browser",
                    sessionId: session.sessionId,
                });
                const navigation = createBrowserNavigation({
                    workbench,
                    tabs: () => [
                        {
                            id: "browser:browser-1",
                            kind: "browser",
                            sessionId: session.sessionId,
                        },
                    ],
                    selection,
                    selectTab: vi.fn(() => true),
                    selectReference: vi.fn(),
                    focusSource: vi.fn(),
                    setError: vi.fn(),
                });

                navigation.openCommand(session.sessionId);
                void navigation.executeCommand("browser").then((result) => {
                    expect(result).toEqual({ ok: true });
                    expect(workbench.open).toHaveBeenCalledWith();
                    navigation.dismissCommand();
                    expect(workbench.focus).not.toHaveBeenCalled();
                    dispose();
                    resolve();
                });
            }),
        );
    });

    it("reports the browser tab limit without attempting another open", async () => {
        await new Promise<void>((resolve) =>
            createRoot((dispose) => {
                const [state, setState] = createSignal<BrowserState>({
                    revision: 1,
                    sessions: [session],
                    activeSessionId: session.sessionId,
                    maxSessions: 1,
                });
                const workbench = fakeWorkbench(state, setState);
                const [selection] = createSignal<WorkbenchSelection>({
                    kind: "browser",
                    sessionId: session.sessionId,
                });
                const navigation = createBrowserNavigation({
                    workbench,
                    tabs: () => [
                        {
                            id: "browser:browser-1",
                            kind: "browser",
                            sessionId: session.sessionId,
                        },
                    ],
                    selection,
                    selectTab: vi.fn(() => true),
                    selectReference: vi.fn(),
                    focusSource: vi.fn(),
                    setError: vi.fn(),
                });

                navigation.openCommand(session.sessionId);
                void navigation.executeCommand("browser").then((result) => {
                    expect(result).toEqual({
                        ok: false,
                        message: "Browser tab limit (1) reached",
                    });
                    expect(workbench.open).not.toHaveBeenCalled();
                    dispose();
                    resolve();
                });
            }),
        );
    });
});
