import { describe, expect, it } from "vitest";
import { retainProjection, shouldAcceptRevision } from "./stateProjection";

describe("GUI snapshot ordering", () => {
    it("accepts the first and current revisions but rejects stale state", () => {
        expect(shouldAcceptRevision(undefined, 1)).toBe(true);
        expect(shouldAcceptRevision(8, 8)).toBe(true);
        expect(shouldAcceptRevision(8, 9)).toBe(true);
        expect(shouldAcceptRevision(8, 7)).toBe(false);
    });

    it("retains unchanged snapshot branches and replaces only changed rows", () => {
        const previous = {
            revision: 8,
            cursor: { line: 3, column: 4 },
            lines: [
                { number: 3, current: true, segments: [{ text: "old" }] },
                { number: 4, current: false, segments: [{ text: "stable" }] },
            ],
            tabs: [{ index: 0, title: "main.rs", active: true }],
        };
        const next = structuredClone(previous);
        next.revision = 9;
        next.cursor.column = 5;
        next.lines[0].segments[0].text = "new";

        const retained = retainProjection(previous, next);

        expect(retained).not.toBe(previous);
        expect(retained.cursor).not.toBe(previous.cursor);
        expect(retained.lines).not.toBe(previous.lines);
        expect(retained.lines[0]).not.toBe(previous.lines[0]);
        expect(retained.lines[1]).toBe(previous.lines[1]);
        expect(retained.tabs).toBe(previous.tabs);
    });
});
