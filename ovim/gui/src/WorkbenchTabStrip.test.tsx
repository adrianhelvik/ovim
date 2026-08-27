/** @vitest-environment jsdom */

import { fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import WorkbenchTabStrip from "./WorkbenchTabStrip";

afterEach(() => document.body.replaceChildren());

describe("WorkbenchTabStrip browser recovery", () => {
    it("offers one-click closed-tab recovery when history is available", () => {
        const restore = vi.fn();
        const result = render(() => (
            <WorkbenchTabStrip
                native
                sourceTabs={[
                    {
                        id: 1,
                        index: 0,
                        title: "main.rs",
                        active: true,
                        modified: false,
                    },
                ]}
                tabs={[{ id: "source:1", kind: "source", index: 0, tabId: 1 }]}
                selection={{ kind: "source", tabId: 1 }}
                browserState={{
                    revision: 3,
                    sessions: [],
                    maxSessions: 8,
                }}
                browserOpening={false}
                canRestoreBrowser
                onSelect={vi.fn()}
                onSourceFocus={vi.fn()}
                onNewBrowser={vi.fn()}
                onRestoreBrowser={restore}
                onNavigate={vi.fn()}
            />
        ));
        try {
            fireEvent.click(
                screen.getByRole("button", {
                    name: "Restore closed browser tab",
                }),
            );
            expect(restore).toHaveBeenCalledOnce();
        } finally {
            result.unmount();
        }
    });
});
