import {
    createEffect,
    createSignal,
    type Accessor,
    type Setter,
} from "solid-js";
import { parseBrowserCommand } from "./browserCommands";
import { createBrowserKeyRouter, type BrowserKeyEvent } from "./browserKeys";
import type { BrowserAddressFocusRequest } from "./browserProtocol";
import type { BrowserWorkbenchController } from "./browserWorkbench";
import {
    workbenchSelectionId,
    type WorkbenchSelection,
    type WorkbenchTabReference,
} from "./workbench";

export interface BrowserCommandRequest {
    serial: number;
    sessionId: string;
}

interface BrowserNavigationOptions {
    workbench: BrowserWorkbenchController;
    tabs: Accessor<WorkbenchTabReference[]>;
    selection: Accessor<WorkbenchSelection>;
    selectTab: (position: number) => boolean;
    selectReference: (tab: WorkbenchTabReference) => void;
    focusSource: () => void;
    setError: Setter<string>;
}

/** Coordinates browser-only navigation state without coupling it to App. */
export const createBrowserNavigation = (options: BrowserNavigationOptions) => {
    const [commandRequest, setCommandRequest] =
        createSignal<BrowserCommandRequest>();
    const [addressFocusRequest, setAddressFocusRequest] =
        createSignal<BrowserAddressFocusRequest>();
    let nextCommandSerial = 1;
    let nextAddressFocusSerial = 1;
    let commandRefocusSession: string | undefined;

    const hasSession = (sessionId: string) =>
        options.workbench
            .state()
            .sessions.some((session) => session.sessionId === sessionId);

    createEffect(() => {
        const request = commandRequest();
        if (!request || hasSession(request.sessionId)) return;
        commandRefocusSession = undefined;
        setCommandRequest(undefined);
    });

    const closeTab = async (sessionId: string) => {
        const tabs = options.tabs();
        const position = tabs.findIndex(
            (tab) => tab.kind === "browser" && tab.sessionId === sessionId,
        );
        const closingSelected =
            options.selection().kind === "browser" &&
            options.workbench.activeSessionId() === sessionId;
        const fallback =
            position >= 0
                ? (tabs[position + 1] ?? tabs[position - 1])
                : undefined;
        await options.workbench.close(
            sessionId,
            position >= 0 ? position : tabs.length,
        );
        if (closingSelected && fallback) options.selectReference(fallback);
    };

    const openCommand = (sessionId = options.workbench.activeSessionId()) => {
        if (!sessionId || !hasSession(sessionId)) return;
        options.workbench.present(sessionId);
        commandRefocusSession = sessionId;
        setCommandRequest({
            serial: nextCommandSerial++,
            sessionId,
        });
    };

    const focusAddress = (sessionId = options.workbench.activeSessionId()) => {
        if (!sessionId || !hasSession(sessionId)) return;
        options.workbench.present(sessionId);
        setAddressFocusRequest({
            serial: nextAddressFocusSerial++,
            sessionId,
        });
    };

    const dismissCommand = () => {
        const sessionId = commandRefocusSession;
        commandRefocusSession = undefined;
        setCommandRequest(undefined);
        if (sessionId)
            requestAnimationFrame(() => {
                void options.workbench.focus(sessionId).catch(() => {});
            });
        else if (options.selection().kind === "source")
            requestAnimationFrame(options.focusSource);
    };

    const routeKey = createBrowserKeyRouter({
        tabs: options.tabs,
        selection: options.selection,
        hasSession,
        openTab: async (url) => {
            await options.workbench.open(url);
        },
        restoreTab: options.workbench.restore,
        closeTab,
        focusAddress,
        openCommand,
        runToolbar: options.workbench.toolbar,
        selectTab: options.selectTab,
    });

    const performKey = (event: BrowserKeyEvent) => {
        void routeKey(event).catch((reason) =>
            options.setError(String(reason)),
        );
    };

    const executeCommand = async (input: string) => {
        const request = commandRequest();
        if (!request)
            return { ok: false, message: "No browser tab is selected" };
        const parsed = parseBrowserCommand(input);
        if (!parsed.ok) return parsed;
        try {
            switch (parsed.command.kind) {
                case "close":
                    commandRefocusSession = undefined;
                    await closeTab(request.sessionId);
                    break;
                case "open_tab": {
                    const state = options.workbench.state();
                    if (options.workbench.opening())
                        return {
                            ok: false,
                            message: "A browser tab is already opening",
                        };
                    if (state.sessions.length >= state.maxSessions)
                        return {
                            ok: false,
                            message: `Browser tab limit (${state.maxSessions}) reached`,
                        };
                    const sessionId = await options.workbench.open();
                    if (!sessionId)
                        return {
                            ok: false,
                            message: "Could not open a new browser tab",
                        };
                    commandRefocusSession = undefined;
                    break;
                }
                case "navigate":
                    await options.workbench.navigate(
                        request.sessionId,
                        parsed.command.url,
                    );
                    break;
                case "history":
                    await options.workbench.toolbar(
                        request.sessionId,
                        parsed.command.direction,
                        parsed.command.count,
                    );
                    break;
                case "reload":
                case "stop":
                    await options.workbench.toolbar(
                        request.sessionId,
                        parsed.command.kind,
                    );
                    break;
                case "select_relative_tab": {
                    const tabs = options.tabs();
                    const current = tabs.findIndex(
                        (tab) =>
                            tab.id ===
                            workbenchSelectionId(options.selection()),
                    );
                    if (!tabs.length || current < 0)
                        return {
                            ok: false,
                            message: "No workbench tab is selected",
                        };
                    const position =
                        (current +
                            (parsed.command.delta % tabs.length) +
                            tabs.length) %
                        tabs.length;
                    commandRefocusSession = undefined;
                    options.selectTab(position);
                    break;
                }
                case "select_tab":
                    if (!options.selectTab(parsed.command.position - 1))
                        return {
                            ok: false,
                            message: `Workbench tab ${parsed.command.position} does not exist`,
                        };
                    commandRefocusSession = undefined;
                    break;
            }
            return { ok: true };
        } catch (reason) {
            return { ok: false, message: String(reason) };
        }
    };

    return {
        commandRequest,
        addressFocusRequest,
        closeTab,
        openCommand,
        focusAddress,
        dismissCommand,
        executeCommand,
        performKey,
    };
};
