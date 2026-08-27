import { describe, expect, it } from "vitest";
import type { BrowserState } from "./browserProtocol";
import {
    activeSourceSelection,
    createWorkbenchTabOrder,
    projectBrowserState,
    reconcileWorkbenchSelection,
    reconcileWorkbenchTabs,
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
        vimKeysEnabled: true,
        keyMode: "normal",
    })),
    activeSessionId,
    maxSessions: 8,
});

describe("workbench selection", () => {
    it("preserves integrated order and inserts new browsers beside their creator", () => {
        const first = reconcileWorkbenchTabs(
            [],
            sourceTabs,
            false,
            browserState(["browser-1"]).sessions,
            { kind: "source", tabId: 84 },
        );
        expect(first.map((tab) => tab.id)).toEqual([
            "source:41",
            "source:84",
            "browser:browser-1",
        ]);

        const second = reconcileWorkbenchTabs(
            first,
            sourceTabs,
            false,
            browserState(["browser-1", "browser-2"]).sessions,
            { kind: "browser", sessionId: "browser-1" },
        );
        expect(second.map((tab) => tab.id)).toEqual([
            "source:41",
            "source:84",
            "browser:browser-1",
            "browser:browser-2",
        ]);

        expect(
            reconcileWorkbenchTabs(
                second,
                sourceTabs,
                false,
                browserState(["browser-2"]).sessions,
                { kind: "browser", sessionId: "browser-2" },
            ).map((tab) => tab.id),
        ).toEqual(["source:41", "source:84", "browser:browser-2"]);
    });

    it("restores a browser at its remembered integrated position", () => {
        const order = createWorkbenchTabOrder();
        order.placeBrowser("browser-restored", { position: 1 });
        const restored = order.reconcile(
            [],
            sourceTabs,
            false,
            browserState(["browser-restored"]).sessions,
            { kind: "source", tabId: 84 },
        );
        expect(restored.map((tab) => tab.id)).toEqual([
            "source:41",
            "browser:browser-restored",
            "source:84",
        ]);
    });

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
