import { describe, expect, it } from "vitest";
import { anchoredOverlayPosition } from "./overlayPosition";

describe("anchored overlay placement", () => {
    it("follows an anchor while staying inside the editor", () => {
        expect(
            anchoredOverlayPosition({
                anchorX: 320,
                anchorY: 180,
                containerWidth: 900,
                containerHeight: 600,
                preferredWidth: 430,
                preferredHeight: 250,
            }),
        ).toEqual({ left: 320, top: 184, width: 430, height: 250 });
    });

    it("flips above and clamps horizontally near the lower-right edge", () => {
        expect(
            anchoredOverlayPosition({
                anchorX: 860,
                anchorY: 570,
                containerWidth: 900,
                containerHeight: 600,
                preferredWidth: 430,
                preferredHeight: 250,
            }),
        ).toEqual({ left: 462, top: 316, width: 430, height: 250 });
    });

    it("shrinks safely when the editor is smaller than the surface", () => {
        expect(
            anchoredOverlayPosition({
                anchorX: 10,
                anchorY: 10,
                containerWidth: 220,
                containerHeight: 140,
                preferredWidth: 430,
                preferredHeight: 290,
            }),
        ).toEqual({ left: 8, top: 8, width: 204, height: 124 });
    });
});
