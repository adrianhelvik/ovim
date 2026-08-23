import { describe, expect, it } from "vitest";
import {
    colorSchemeFor,
    contrastRatio,
    normalizeMutedColor,
    themeVariables,
} from "./theme";
import { mockSnapshot } from "./mock";

describe("GUI semantic theme projection", () => {
    it("normalizes low-contrast secondary text to WCAG AA", () => {
        const normalized = normalizeMutedColor("#111522", "#c8d3f5", "#59647e");
        expect(normalized).not.toBe("#59647e");
        expect(contrastRatio("#111522", normalized)).toBeGreaterThanOrEqual(
            4.5,
        );
    });

    it("preserves colors that already pass and identifies appearance", () => {
        expect(normalizeMutedColor("#ffffff", "#111111", "#555555")).toBe(
            "#555555",
        );
        expect(colorSchemeFor("#090b12")).toBe("dark");
        expect(colorSchemeFor("#f7f8fb")).toBe("light");
    });

    it("separates semantic GUI roles from the core theme contract", () => {
        const variables = themeVariables(mockSnapshot.theme);
        expect(variables["--canvas"]).toBe(mockSnapshot.theme.background);
        expect(variables["--surface-1"]).toBe(mockSnapshot.theme.surface);
        expect(variables["--text-secondary"]).not.toBe(
            mockSnapshot.theme.muted,
        );
        expect(variables["--color-scheme"]).toBe("dark");
    });
});
