import type { BrowserSession, BrowserState } from "./BrowserPanel";
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

export const composeWorkbenchTabs = (
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
