const FOCUSABLE = [
    "button:not(:disabled)",
    "[href]",
    "input:not(:disabled)",
    "select:not(:disabled)",
    "textarea:not(:disabled)",
    "[tabindex]:not([tabindex='-1'])",
].join(",");

const GUI_NATIVE_CONTROL = [
    "a[href]",
    "button",
    "input",
    "select",
    "textarea",
    "[contenteditable='true']",
    "[data-gui-native-control]",
].join(",");

export const isGuiNativeControl = (
    target: Element | null,
    inputSink?: Element,
) => target !== inputSink && Boolean(target?.closest(GUI_NATIVE_CONTROL));

export const trapDialogFocus = (
    event: KeyboardEvent,
    container: HTMLElement,
) => {
    if (event.key !== "Tab") return false;
    const focusable = Array.from(
        container.querySelectorAll<HTMLElement>(FOCUSABLE),
    ).filter((element) => {
        const style = getComputedStyle(element);
        return (
            !element.hidden &&
            !element.closest("[hidden], [aria-hidden='true']") &&
            style.display !== "none" &&
            style.visibility !== "hidden"
        );
    });
    const first = focusable[0];
    const last = focusable.at(-1);
    if (!first || !last) {
        event.preventDefault();
        container.focus();
        return true;
    }

    if (
        event.shiftKey &&
        (document.activeElement === first ||
            !container.contains(document.activeElement))
    ) {
        event.preventDefault();
        last.focus();
        return true;
    }
    if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
        return true;
    }
    return false;
};
