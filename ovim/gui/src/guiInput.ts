import type { GuiKeyInput } from "./types";

export const guiKeyInput = (
    event: Pick<
        KeyboardEvent,
        "key" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey"
    >,
): GuiKeyInput | undefined => {
    if (["Shift", "Control", "Alt", "Meta"].includes(event.key))
        return undefined;
    const optionProducedText =
        event.altKey &&
        Array.from(event.key).length === 1 &&
        !/^[\x00-\x7f]$/.test(event.key);
    return {
        key: event.key,
        shift: event.shiftKey,
        control: event.ctrlKey,
        alt: event.altKey && !optionProducedText,
        meta: event.metaKey,
    };
};
