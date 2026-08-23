import { describe, expect, it } from "vitest";
import { shouldAcceptRevision } from "./stateProjection";

describe("GUI snapshot ordering", () => {
    it("accepts the first and current revisions but rejects stale state", () => {
        expect(shouldAcceptRevision(undefined, 1)).toBe(true);
        expect(shouldAcceptRevision(8, 8)).toBe(true);
        expect(shouldAcceptRevision(8, 9)).toBe(true);
        expect(shouldAcceptRevision(8, 7)).toBe(false);
    });
});
