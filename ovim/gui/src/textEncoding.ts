const utf8Encoder = new TextEncoder();

export const splitAtUtf8Offset = (text: string, offset: number) => {
    const limit = Math.max(
        0,
        Math.min(offset, utf8Encoder.encode(text).length),
    );
    let bytes = 0;
    let codeUnits = 0;
    for (const character of text) {
        const next = bytes + utf8Encoder.encode(character).length;
        if (next > limit) break;
        bytes = next;
        codeUnits += character.length;
    }
    return [text.slice(0, codeUnits), text.slice(codeUnits)] as const;
};

export const utf8OffsetFromTextArea = (text: string, utf16Offset: number) =>
    utf8Encoder.encode(text.slice(0, utf16Offset)).length;

export const utf16OffsetFromUtf8 = (text: string, utf8Offset: number) =>
    splitAtUtf8Offset(text, utf8Offset)[0].length;
