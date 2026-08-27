import type { WorkbenchSelection, WorkbenchTabReference } from "./workbench";

export type BrowserKeyIntent =
    | "command"
    | "new_tab"
    | "restore_tab"
    | "close_tab"
    | "focus_address"
    | "reload"
    | "find"
    | "back"
    | "forward"
    | "previous_tab"
    | "next_tab"
    | "first_tab"
    | "last_tab";

export interface BrowserKeyEvent {
    sessionId?: string;
    intent: BrowserKeyIntent;
    count?: number;
    url?: string;
}

export type BrowserShortcutAction =
    | "file.close"
    | "browser.new-tab"
    | "browser.restore-tab"
    | "browser.focus-address"
    | "browser.reload"
    | "browser.back"
    | "browser.forward"
    | "browser.previous-tab"
    | "browser.next-tab";

export const browserShortcutAction = (
    event: Pick<KeyboardEvent, "key" | "code" | "shiftKey">,
    browserActive: boolean,
    macos: boolean,
): BrowserShortcutAction | undefined => {
    const key = event.key.toLowerCase();
    if (key === "w" && (browserActive || macos)) return "file.close";
    if (key === "t" && (browserActive || macos))
        return event.shiftKey ? "browser.restore-tab" : "browser.new-tab";
    if (!browserActive) return undefined;
    if (key === "l") return "browser.focus-address";
    if (key === "r") return "browser.reload";
    if (event.code === "BracketLeft")
        return event.shiftKey ? "browser.previous-tab" : "browser.back";
    if (event.code === "BracketRight")
        return event.shiftKey ? "browser.next-tab" : "browser.forward";
    return undefined;
};

interface BrowserKeyRouterOptions {
    tabs: () => WorkbenchTabReference[];
    selection: () => WorkbenchSelection;
    hasSession: (sessionId: string) => boolean;
    openTab: (url?: string) => Promise<void>;
    restoreTab: () => Promise<void>;
    closeTab: (sessionId: string) => Promise<void>;
    focusAddress: (sessionId: string) => void;
    openCommand: (sessionId: string) => void;
    runToolbar: (
        sessionId: string,
        action: "back" | "forward" | "reload" | "find",
        count?: number,
    ) => Promise<void>;
    selectTab: (position: number) => boolean;
}

const commandCount = (count: number | undefined) =>
    Math.max(1, Math.min(Math.trunc(count ?? 1) || 1, 100));

const browserTabPositions = (tabs: WorkbenchTabReference[]) =>
    tabs.flatMap((tab, position) => (tab.kind === "browser" ? [position] : []));

const currentBrowserPosition = (
    tabs: WorkbenchTabReference[],
    selection: WorkbenchSelection,
) => {
    if (selection.kind !== "browser") return -1;
    return tabs.findIndex(
        (tab) =>
            tab.kind === "browser" && tab.sessionId === selection.sessionId,
    );
};

export const createBrowserKeyRouter = (options: BrowserKeyRouterOptions) => {
    const selectRelativeTab = (delta: number) => {
        const tabs = options.tabs();
        const positions = browserTabPositions(tabs);
        const current = currentBrowserPosition(tabs, options.selection());
        const currentBrowser = positions.indexOf(current);
        if (!positions.length || currentBrowser < 0) return;
        const target =
            (currentBrowser + (delta % positions.length) + positions.length) %
            positions.length;
        options.selectTab(positions[target]);
    };
    const selectEdgeTab = (fromEnd: boolean, count = 1) => {
        const positions = browserTabPositions(options.tabs());
        if (!positions.length) return;
        options.selectTab(
            fromEnd
                ? positions[positions.length - 1]
                : positions[Math.min(count - 1, positions.length - 1)],
        );
    };

    return async (event: BrowserKeyEvent) => {
        const count = commandCount(event.count);
        if (event.intent === "new_tab") {
            for (let index = 0; index < count; index += 1)
                await options.openTab(event.url);
            return;
        }
        if (event.intent === "restore_tab") {
            await options.restoreTab();
            return;
        }

        const sessionId = event.sessionId;
        if (!sessionId || !options.hasSession(sessionId)) return;
        switch (event.intent) {
            case "command":
                options.openCommand(sessionId);
                break;
            case "close_tab":
                await options.closeTab(sessionId);
                break;
            case "focus_address":
                options.focusAddress(sessionId);
                break;
            case "reload":
                await options.runToolbar(sessionId, "reload");
                break;
            case "find":
                await options.runToolbar(sessionId, "find");
                break;
            case "back":
            case "forward":
                await options.runToolbar(sessionId, event.intent, count);
                break;
            case "previous_tab":
                selectRelativeTab(-count);
                break;
            case "next_tab":
                selectRelativeTab(count);
                break;
            case "first_tab":
                selectEdgeTab(false, count);
                break;
            case "last_tab":
                selectEdgeTab(true);
                break;
        }
    };
};
