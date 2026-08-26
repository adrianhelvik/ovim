import type { BrowserSession, BrowserState } from "./browserProtocol";
import type { GuiSnapshot } from "./types";

type SourceTab = GuiSnapshot["tabs"][number];

export type WorkbenchTabReference =
    | { id: string; kind: "source"; index: number; tabId: number }
    | { id: string; kind: "vector"; sourceTabId: number }
    | { id: string; kind: "browser"; sessionId: string };

export type WorkbenchSelection =
    | { kind: "source"; tabId: number }
    | { kind: "vector"; sourceTabId: number }
    | { kind: "browser"; sessionId: string };

export const activeSourceSelection = (
    sourceTabs: SourceTab[],
): Extract<WorkbenchSelection, { kind: "source" }> => ({
    kind: "source",
    tabId: sourceTabs.find((tab) => tab.active)?.id ?? sourceTabs[0]?.id ?? 0,
});

export const workbenchSelectionId = (selection: WorkbenchSelection) => {
    switch (selection.kind) {
        case "source":
            return `source:${selection.tabId}`;
        case "vector":
            return `vector:${selection.sourceTabId}`;
        case "browser":
            return `browser:${selection.sessionId}`;
    }
};

const availableWorkbenchTabs = (
    sourceTabs: SourceTab[],
    includeVector: boolean,
    browserSessions: BrowserSession[],
): WorkbenchTabReference[] => {
    const activeSource = activeSourceSelection(sourceTabs);
    return [
        ...sourceTabs.map((tab) => ({
            id: `source:${tab.id}`,
            kind: "source" as const,
            index: tab.index,
            tabId: tab.id,
        })),
        ...(includeVector
            ? [
                  {
                      id: `vector:${activeSource.tabId}`,
                      kind: "vector" as const,
                      sourceTabId: activeSource.tabId,
                  },
              ]
            : []),
        ...browserSessions.map((session) => ({
            id: `browser:${session.sessionId}`,
            kind: "browser" as const,
            sessionId: session.sessionId,
        })),
    ];
};

export interface WorkbenchTabPlacement {
    afterId?: string;
    position?: number;
}

const sameTabReference = (
    left: WorkbenchTabReference,
    right: WorkbenchTabReference,
) => {
    if (left.kind !== right.kind || left.id !== right.id) return false;
    switch (left.kind) {
        case "source":
            return right.kind === "source" && left.index === right.index;
        case "vector":
            return (
                right.kind === "vector" &&
                left.sourceTabId === right.sourceTabId
            );
        case "browser":
            return (
                right.kind === "browser" && left.sessionId === right.sessionId
            );
    }
};

const insertAfter = (
    tabs: WorkbenchTabReference[],
    tab: WorkbenchTabReference,
    afterId?: string,
) => {
    const anchor = afterId
        ? tabs.findIndex((candidate) => candidate.id === afterId)
        : -1;
    tabs.splice(anchor >= 0 ? anchor + 1 : tabs.length, 0, tab);
};

export const reconcileWorkbenchTabs = (
    previous: WorkbenchTabReference[],
    sourceTabs: SourceTab[],
    includeVector: boolean,
    browserSessions: BrowserSession[],
    selection: WorkbenchSelection,
    placements: ReadonlyMap<string, WorkbenchTabPlacement> = new Map(),
): WorkbenchTabReference[] => {
    const available = availableWorkbenchTabs(
        sourceTabs,
        includeVector,
        browserSessions,
    );
    const availableById = new Map(available.map((tab) => [tab.id, tab]));
    const ordered = previous.flatMap((tab) => {
        const next = availableById.get(tab.id);
        if (!next) return [];
        return [sameTabReference(tab, next) ? tab : next];
    });
    const retained = new Set(ordered.map((tab) => tab.id));

    for (const source of available.filter((tab) => tab.kind === "source")) {
        if (retained.has(source.id)) continue;
        const sourcePosition = sourceTabs.findIndex(
            (tab) => tab.id === source.tabId,
        );
        const precedingSource = sourceTabs
            .slice(0, sourcePosition)
            .reverse()
            .map((tab) => `source:${tab.id}`)
            .find((id) => retained.has(id));
        const followingSource = sourceTabs
            .slice(sourcePosition + 1)
            .map((tab) => `source:${tab.id}`)
            .find((id) => retained.has(id));
        if (precedingSource) insertAfter(ordered, source, precedingSource);
        else if (followingSource) {
            const position = ordered.findIndex(
                (tab) => tab.id === followingSource,
            );
            ordered.splice(position, 0, source);
        } else ordered.push(source);
        retained.add(source.id);
    }

    const vector = available.find((tab) => tab.kind === "vector");
    if (vector && !retained.has(vector.id)) {
        insertAfter(ordered, vector, `source:${vector.sourceTabId}`);
        retained.add(vector.id);
    }

    let defaultBrowserAnchor = workbenchSelectionId(selection);
    for (const browser of available.filter((tab) => tab.kind === "browser")) {
        if (retained.has(browser.id)) continue;
        const placement = placements.get(browser.id);
        if (placement?.position !== undefined) {
            ordered.splice(
                Math.max(0, Math.min(placement.position, ordered.length)),
                0,
                browser,
            );
        } else {
            const afterId = placement?.afterId ?? defaultBrowserAnchor;
            insertAfter(ordered, browser, afterId);
        }
        retained.add(browser.id);
        defaultBrowserAnchor = browser.id;
    }

    return ordered;
};

export const createWorkbenchTabOrder = () => {
    const placements = new Map<string, WorkbenchTabPlacement>();
    return {
        placeBrowser(sessionId: string, placement: WorkbenchTabPlacement = {}) {
            placements.set(`browser:${sessionId}`, placement);
        },
        reconcile(
            previous: WorkbenchTabReference[],
            sourceTabs: SourceTab[],
            includeVector: boolean,
            browserSessions: BrowserSession[],
            selection: WorkbenchSelection,
        ) {
            const next = reconcileWorkbenchTabs(
                previous,
                sourceTabs,
                includeVector,
                browserSessions,
                selection,
                placements,
            );
            for (const tab of next) placements.delete(tab.id);
            return next;
        },
    };
};

export const reconcileWorkbenchSelection = (
    selection: WorkbenchSelection,
    sourceTabs: SourceTab[],
    includeVector: boolean,
    browserState: BrowserState,
): WorkbenchSelection => {
    const source = activeSourceSelection(sourceTabs);
    switch (selection.kind) {
        case "source":
            return source;
        case "vector":
            return includeVector && selection.sourceTabId === source.tabId
                ? selection
                : source;
        case "browser": {
            const sessionId = browserState.sessions.some(
                (session) => session.sessionId === browserState.activeSessionId,
            )
                ? browserState.activeSessionId
                : undefined;
            return sessionId ? { kind: "browser", sessionId } : source;
        }
    }
};

export const requestedBrowserPresentation = (
    latestRevision: number,
    state: BrowserState,
) => {
    const request = state.presentationRequest;
    if (!request || request.revision <= latestRevision) return undefined;
    return {
        revision: request.revision,
        sessionId: state.sessions.some(
            (session) => session.sessionId === request.sessionId,
        )
            ? request.sessionId
            : undefined,
    };
};

export const projectBrowserState = (
    selection: WorkbenchSelection,
    latestPresentationRevision: number,
    sourceTabs: SourceTab[],
    includeVector: boolean,
    state: BrowserState,
) => {
    const presentation = requestedBrowserPresentation(
        latestPresentationRevision,
        state,
    );
    if (presentation) {
        return {
            selection: presentation.sessionId
                ? ({
                      kind: "browser",
                      sessionId: presentation.sessionId,
                  } satisfies WorkbenchSelection)
                : reconcileWorkbenchSelection(
                      selection,
                      sourceTabs,
                      includeVector,
                      state,
                  ),
            presentationRevision: presentation.revision,
            acknowledgeRevision: presentation.revision,
        };
    }
    return {
        selection: reconcileWorkbenchSelection(
            selection,
            sourceTabs,
            includeVector,
            state,
        ),
        presentationRevision: latestPresentationRevision,
        acknowledgeRevision: undefined,
    };
};
