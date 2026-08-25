import { invoke } from "@tauri-apps/api/core";
import { createMemo, createSignal, type Accessor, type Setter } from "solid-js";
import type { BrowserSession, BrowserState } from "./BrowserPanel";
import type { GuiSnapshot } from "./types";
import { projectBrowserState, type WorkbenchSelection } from "./workbench";

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
}

export const createBrowserWorkbench = (options: BrowserWorkbenchOptions) => {
    const [state, setState] = createSignal<BrowserState>({
        sessions: [],
        maxSessions: 8,
    });
    const [opening, setOpening] = createSignal(false);
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
    const accept = (next: BrowserState) => {
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
    const open = async (url?: string) => {
        const current = state();
        if (
            !options.native ||
            opening() ||
            current.sessions.length >= current.maxSessions
        )
            return;
        setOpening(true);
        options.setError("");
        try {
            const next = await invoke<BrowserState>(
                "gui_browser_open",
                url ? { url } : undefined,
            );
            accept(next);
            if (next.activeSessionId) present(next.activeSessionId);
        } catch (reason) {
            options.setError(String(reason));
        } finally {
            setOpening(false);
        }
    };
    const close = async (sessionId: string) => {
        if (!options.native || closing.has(sessionId) || !hasSession(sessionId))
            return;
        closing.add(sessionId);
        try {
            accept(
                await invoke<BrowserState>("gui_browser_close", { sessionId }),
            );
        } finally {
            closing.delete(sessionId);
        }
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
        activeSessionId,
        activeSession,
        accept,
        open,
        close,
        navigate,
        toolbar,
        setVimKeys,
        activate,
        present,
        focus,
    };
};
