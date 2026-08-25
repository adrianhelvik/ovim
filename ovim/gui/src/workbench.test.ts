import { describe, expect, it } from "vitest";
import type { BrowserState } from "./BrowserPanel";
import {
    activeSourceSelection,
    projectBrowserState,
    reconcileWorkbenchSelection,
    type WorkbenchSelection,
} from "./workbench";

const sourceTabs = [
    { id: 41, index: 0, title: "one.rs", active: false, modified: false },
    { id: 84, index: 1, title: "two.rs", active: true, modified: false },
];

const browserState = (
    sessionIds: string[],
    activeSessionId?: string,
): BrowserState => ({
    sessions: sessionIds.map((sessionId) => ({
        sessionId,
        url: `https://${sessionId}.example/`,
        title: sessionId,
        visible: false,
        loading: false,
        documentId: 1,
    })),
    activeSessionId,
    maxSessions: 8,
});

describe("workbench selection", () => {
    it("tracks source identity independently of tab position", () => {
        expect(activeSourceSelection(sourceTabs)).toEqual({
            kind: "source",
            tabId: 84,
        });
        expect(
            reconcileWorkbenchSelection(
                { kind: "source", tabId: 41 },
                sourceTabs,
                false,
                browserState([]),
            ),
        ).toEqual({ kind: "source", tabId: 84 });
    });

    it("binds vector presentation to its source tab", () => {
        const selection: WorkbenchSelection = {
            kind: "vector",
            sourceTabId: 84,
        };
        expect(
            reconcileWorkbenchSelection(
                selection,
                sourceTabs,
                true,
                browserState([]),
            ),
        ).toBe(selection);
        expect(
            reconcileWorkbenchSelection(
                selection,
                sourceTabs,
                false,
                browserState([]),
            ),
        ).toEqual({ kind: "source", tabId: 84 });
    });

    it("selects an agent-presented browser once and falls back after close", () => {
        const state = {
            ...browserState(["browser-7"], "browser-7"),
            presentationRequest: {
                revision: 5,
                sessionId: "browser-7",
            },
        };
        const presented = projectBrowserState(
            { kind: "source", tabId: 84 },
            0,
            sourceTabs,
            false,
            state,
        );
        expect(presented).toEqual({
            selection: { kind: "browser", sessionId: "browser-7" },
            presentationRevision: 5,
            acknowledgeRevision: 5,
        });

        const closed = projectBrowserState(
            presented.selection,
            presented.presentationRevision,
            sourceTabs,
            false,
            browserState([]),
        );
        expect(closed.selection).toEqual({ kind: "source", tabId: 84 });
        expect(closed.acknowledgeRevision).toBeUndefined();
    });
});
