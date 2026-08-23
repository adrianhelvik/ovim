import { describe, expect, it } from "vitest";
import {
    splitAtUtf8Offset,
    utf16OffsetFromUtf8,
    utf8OffsetFromTextArea,
} from "./textEncoding";

describe("GUI UTF-8 cursor conversion", () => {
    it("never splits a multibyte character", () => {
        expect(splitAtUtf8Offset("a界b", 2)).toEqual(["a", "界b"]);
        expect(splitAtUtf8Offset("a界b", 4)).toEqual(["a界", "b"]);
    });

    it("converts between textarea and core cursor units", () => {
        expect(utf8OffsetFromTextArea("a🙂b", 3)).toBe(5);
        expect(utf16OffsetFromUtf8("a🙂b", 5)).toBe(3);
    });
});
