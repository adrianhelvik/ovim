import { invoke } from "@tauri-apps/api/core";
import { createMemo, createSignal, type Accessor, type Setter } from "solid-js";
import type { BrowserSession, BrowserState } from "./BrowserPanel";
import type { GuiSnapshot } from "./types";
import { projectBrowserState, type WorkbenchSelection } from "./workbench";

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
    const focus = (sessionId = activeSessionId()) => {
        if (options.native && sessionId)
            void invoke("gui_browser_toolbar", {
                sessionId,
                action: "focus",
            }).catch(() => {});
    };
    const present = (sessionId: string) => {
        options.setSelection({ kind: "browser", sessionId });
        requestAnimationFrame(() =>
            requestAnimationFrame(() => {
                if (activeSessionId() === sessionId) focus(sessionId);
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
    const open = async () => {
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
            const next = await invoke<BrowserState>("gui_browser_open");
            accept(next);
            if (next.activeSessionId) present(next.activeSessionId);
        } catch (reason) {
            options.setError(String(reason));
        } finally {
            setOpening(false);
        }
    };
    const activate = (sessionId: string) => {
        if (
            !state().sessions.some((session) => session.sessionId === sessionId)
        )
            return;
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
        activate,
        present,
        focus,
    };
};
