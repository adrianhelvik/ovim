import { describe, expect, it, vi } from "vitest";
import { browserShortcutAction, createBrowserKeyRouter } from "./browserKeys";
import type { WorkbenchTabReference } from "./workbench";

const tabs: WorkbenchTabReference[] = [
    { id: "source:1", kind: "source", index: 0, tabId: 1 },
    { id: "browser:one", kind: "browser", sessionId: "one" },
    { id: "browser:two", kind: "browser", sessionId: "two" },
];

const setup = () => {
    const actions = {
        openTab: vi.fn().mockResolvedValue(undefined),
        restoreTab: vi.fn().mockResolvedValue(undefined),
        closeTab: vi.fn().mockResolvedValue(undefined),
        focusAddress: vi.fn(),
        openCommand: vi.fn(),
        runToolbar: vi.fn().mockResolvedValue(undefined),
        selectTab: vi.fn().mockReturnValue(true),
    };
    return {
        actions,
        route: createBrowserKeyRouter({
            tabs: () => tabs,
            selection: () => ({ kind: "browser", sessionId: "one" }),
            hasSession: (sessionId) => ["one", "two"].includes(sessionId),
            ...actions,
        }),
    };
};

describe("browser key router", () => {
    it("releases Vim control keys outside the browser on non-macOS hosts", () => {
        expect(
            browserShortcutAction(
                { key: "w", code: "KeyW", shiftKey: false },
                false,
                false,
            ),
        ).toBeUndefined();
        expect(
            browserShortcutAction(
                { key: "t", code: "KeyT", shiftKey: false },
                false,
                false,
            ),
        ).toBeUndefined();
        expect(
            browserShortcutAction(
                { key: "l", code: "KeyL", shiftKey: false },
                false,
                false,
            ),
        ).toBeUndefined();
        expect(
            browserShortcutAction(
                { key: "{", code: "BracketLeft", shiftKey: true },
                true,
                false,
            ),
        ).toBe("browser.previous-tab");
    });

    it("keeps conventional tab and close shortcuts on macOS", () => {
        expect(
            browserShortcutAction(
                { key: "w", code: "KeyW", shiftKey: false },
                false,
                true,
            ),
        ).toBe("file.close");
        expect(
            browserShortcutAction(
                { key: "t", code: "KeyT", shiftKey: false },
                false,
                true,
            ),
        ).toBe("browser.new-tab");
        expect(
            browserShortcutAction(
                { key: "T", code: "KeyT", shiftKey: true },
                false,
                true,
            ),
        ).toBe("browser.restore-tab");
    });

    it("routes browser actions through the shared workbench controller", async () => {
        const { route, actions } = setup();

        await route({ sessionId: "one", intent: "command" });
        await route({ sessionId: "one", intent: "focus_address" });
        await route({ sessionId: "one", intent: "back", count: 3 });
        await route({ sessionId: "one", intent: "reload" });
        await route({ sessionId: "one", intent: "find" });
        await route({ sessionId: "one", intent: "close_tab" });

        expect(actions.openCommand).toHaveBeenCalledWith("one");
        expect(actions.focusAddress).toHaveBeenCalledWith("one");
        expect(actions.runToolbar).toHaveBeenCalledWith("one", "back", 3);
        expect(actions.runToolbar).toHaveBeenCalledWith("one", "reload");
        expect(actions.runToolbar).toHaveBeenCalledWith("one", "find");
        expect(actions.closeTab).toHaveBeenCalledWith("one");
    });

    it("opens counted tabs and navigates the integrated tab strip", async () => {
        const { route, actions } = setup();

        await route({
            sessionId: "one",
            intent: "new_tab",
            count: 2,
            url: "https://example.com/",
        });
        await route({ intent: "restore_tab" });
        await route({ sessionId: "one", intent: "previous_tab" });
        await route({ sessionId: "one", intent: "next_tab", count: 2 });
        await route({ sessionId: "one", intent: "first_tab", count: 2 });
        await route({ sessionId: "one", intent: "last_tab" });

        expect(actions.openTab).toHaveBeenCalledTimes(2);
        expect(actions.openTab).toHaveBeenCalledWith("https://example.com/");
        expect(actions.restoreTab).toHaveBeenCalledOnce();
        expect(actions.selectTab.mock.calls).toEqual([[0], [0], [1], [2]]);
    });

    it("ignores stale session events", async () => {
        const { route, actions } = setup();
        await route({ sessionId: "gone", intent: "close_tab" });
        expect(actions.closeTab).not.toHaveBeenCalled();
    });
});
