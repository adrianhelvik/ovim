import { invoke } from "@tauri-apps/api/core";
import { createMemo, createSignal, type Accessor, type Setter } from "solid-js";
import type { BrowserSession, BrowserState } from "./BrowserPanel";
import type { GuiSnapshot } from "./types";
import {
    projectBrowserState,
    workbenchSelectionId,
    type WorkbenchSelection,
    type WorkbenchTabPlacement,
} from "./workbench";

export type BrowserToolbarAction =
    "back" | "forward" | "reload" | "stop" | "focus" | "find";

interface BrowserWorkbenchOptions {
    native: boolean;
    sourceTabs: Accessor<GuiSnapshot["tabs"]>;
    includeVector: Accessor<boolean>;
    selection: Accessor<WorkbenchSelection>;
    setSelection: Setter<WorkbenchSelection>;
    setError: Setter<string>;
    onSessionsChanged?: (state: BrowserState) => void;
    onSessionCreated?: (
        sessionId: string,
        placement: WorkbenchTabPlacement,
    ) => void;
}

interface ClosedBrowserTab {
    url?: string;
    position: number;
    vimKeysEnabled: boolean;
}

const MAX_CLOSED_BROWSER_TABS = 20;

export const createBrowserWorkbench = (options: BrowserWorkbenchOptions) => {
    const [state, setState] = createSignal<BrowserState>({
        sessions: [],
        maxSessions: 8,
    });
    const [opening, setOpening] = createSignal(false);
    const [closedTabs, setClosedTabs] = createSignal<ClosedBrowserTab[]>([]);
    let latestPresentationRevision = 0;
    const closing = new Set<string>();

    const activeSessionId = () => {
        const selection = options.selection();
        return selection.kind === "browser" ? selection.sessionId : undefined;
    };
    const activeSession = createMemo<BrowserSession | undefined>(() => {
        const sessionId = activeSessionId();
        return state().sessions.find(
            (session) => session.sessionId === sessionId,
        );
    });
    const hasSession = (sessionId: string) =>
        state().sessions.some((session) => session.sessionId === sessionId);
    const requireSession = (sessionId: string) => {
        if (!options.native || !hasSession(sessionId))
            throw new Error("Browser session is no longer available");
    };
    const focus = async (sessionId = activeSessionId()) => {
        if (options.native && sessionId)
            await invoke("gui_browser_toolbar", {
                sessionId,
                action: "focus",
            });
    };
    const present = (sessionId: string) => {
        options.setSelection({ kind: "browser", sessionId });
        requestAnimationFrame(() =>
            requestAnimationFrame(() => {
                if (activeSessionId() === sessionId)
                    void focus(sessionId).catch(() => {});
            }),
        );
    };
    const accept = (next: BrowserState, placement?: WorkbenchTabPlacement) => {
        const knownSessions = new Set(
            state().sessions.map((session) => session.sessionId),
        );
        const defaultPlacement = {
            afterId: workbenchSelectionId(options.selection()),
        };
        for (const session of next.sessions) {
            if (!knownSessions.has(session.sessionId))
                options.onSessionCreated?.(
                    session.sessionId,
                    placement ?? defaultPlacement,
                );
        }
        setState(next);
        options.onSessionsChanged?.(next);
        const projection = projectBrowserState(
            options.selection(),
            latestPresentationRevision,
            options.sourceTabs(),
            options.includeVector(),
            next,
        );
        latestPresentationRevision = projection.presentationRevision;
        if (projection.acknowledgeRevision !== undefined && options.native)
            void invoke("gui_browser_ack_presentation", {
                revision: projection.acknowledgeRevision,
            }).catch((reason) => options.setError(String(reason)));

        const selection = projection.selection;
        if (
            selection.kind === "browser" &&
            (options.selection().kind !== "browser" ||
                selection.sessionId !== activeSessionId())
        )
            present(selection.sessionId);
        else options.setSelection(selection);
    };
    const open = async (url?: string, placement?: WorkbenchTabPlacement) => {
        const current = state();
        if (
            !options.native ||
            opening() ||
            current.sessions.length >= current.maxSessions
        )
            return undefined;
        setOpening(true);
        options.setError("");
        try {
            const next = await invoke<BrowserState>(
                "gui_browser_open",
                url ? { url } : undefined,
            );
            const createdSession = next.sessions.find(
                (session) =>
                    !current.sessions.some(
                        (existing) => existing.sessionId === session.sessionId,
                    ),
            );
            accept(next, placement);
            if (next.activeSessionId) present(next.activeSessionId);
            return createdSession?.sessionId ?? next.activeSessionId;
        } catch (reason) {
            options.setError(String(reason));
            return undefined;
        } finally {
            setOpening(false);
        }
    };
    const close = async (
        sessionId: string,
        position = state().sessions.length,
    ) => {
        if (!options.native || closing.has(sessionId) || !hasSession(sessionId))
            return;
        const closingSession = state().sessions.find(
            (session) => session.sessionId === sessionId,
        );
        closing.add(sessionId);
        try {
            accept(
                await invoke<BrowserState>("gui_browser_close", { sessionId }),
            );
            if (closingSession)
                setClosedTabs((previous) =>
                    [
                        {
                            url: closingSession.url || undefined,
                            position,
                            vimKeysEnabled: closingSession.vimKeysEnabled,
                        },
                        ...previous,
                    ].slice(0, MAX_CLOSED_BROWSER_TABS),
                );
        } finally {
            closing.delete(sessionId);
        }
    };
    const restore = async () => {
        const closed = closedTabs()[0];
        if (!closed) return;
        const sessionId = await open(closed.url, {
            position: closed.position,
        });
        if (!sessionId) return;
        setClosedTabs((previous) => previous.slice(1));
        if (!closed.vimKeysEnabled) await setVimKeys(sessionId, false);
    };
    const navigate = async (sessionId: string, url: string) => {
        requireSession(sessionId);
        accept(
            await invoke<BrowserState>("gui_browser_navigate", {
                sessionId,
                url,
            }),
        );
        await focus(sessionId);
    };
    const toolbar = async (
        sessionId: string,
        action: BrowserToolbarAction,
        count = 1,
    ) => {
        requireSession(sessionId);
        await invoke("gui_browser_toolbar", { sessionId, action, count });
    };
    const setVimKeys = async (sessionId: string, enabled: boolean) => {
        requireSession(sessionId);
        accept(
            await invoke<BrowserState>("gui_browser_set_vim_keys", {
                sessionId,
                enabled,
            }),
        );
    };
    const activate = (sessionId: string) => {
        if (!hasSession(sessionId)) return;
        if (!options.native) {
            present(sessionId);
            return;
        }
        void invoke<BrowserState>("gui_browser_activate", { sessionId })
            .then((next) => {
                accept(next);
                present(sessionId);
            })
            .catch((reason) => options.setError(String(reason)));
    };

    return {
        state,
        opening,
        canRestore: () => closedTabs().length > 0,
        activeSessionId,
        activeSession,
        accept,
        open,
        close,
        restore,
        navigate,
        toolbar,
        setVimKeys,
        activate,
        present,
        focus,
    };
};
