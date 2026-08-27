(() => {
    const MAX_TEXT = 48 * 1024;
    const MAX_ELEMENTS = 200;
    const refAttribute = "data-ovim-browser-ref";
    document.querySelectorAll(`[${refAttribute}]`).forEach((element) => {
        element.removeAttribute(refAttribute);
    });

    const visible = (element) => {
        const style = getComputedStyle(element);
        if (
            style.display === "none" ||
            style.visibility === "hidden" ||
            Number(style.opacity) === 0
        ) {
            return false;
        }
        const rect = element.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
    };

    const roleFor = (element) => {
        const explicit = element.getAttribute("role");
        if (explicit) return explicit;
        const tag = element.tagName.toLowerCase();
        if (tag === "a" && element.hasAttribute("href")) return "link";
        if (tag === "button") return "button";
        if (tag === "select") return "combobox";
        if (tag === "textarea") return "textbox";
        if (tag === "summary") return "button";
        if (tag === "input") {
            const type = (element.getAttribute("type") || "text").toLowerCase();
            if (["button", "submit", "reset"].includes(type)) return "button";
            if (type === "checkbox") return "checkbox";
            if (type === "radio") return "radio";
            if (type === "range") return "slider";
            return "textbox";
        }
        return tag;
    };

    const nameFor = (element) => {
        const labelledBy = element.getAttribute("aria-labelledby");
        const labelled = labelledBy
            ? labelledBy
                  .split(/\s+/)
                  .map((id) => document.getElementById(id)?.textContent || "")
                  .join(" ")
            : "";
        return (
            element.getAttribute("aria-label") ||
            labelled ||
            element.getAttribute("alt") ||
            element.getAttribute("placeholder") ||
            element.getAttribute("title") ||
            element.textContent ||
            element.getAttribute("name") ||
            ""
        )
            .replace(/\s+/g, " ")
            .trim()
            .slice(0, 512);
    };

    const selector = [
        "a[href]",
        "button",
        "input",
        "textarea",
        "select",
        "summary",
        "[role]",
        "[tabindex]",
        "[contenteditable='true']",
    ].join(",");
    const candidates = Array.from(document.querySelectorAll(selector)).filter(
        visible,
    );
    const elements = candidates.slice(0, MAX_ELEMENTS).map((element, index) => {
        const reference = `e${index + 1}`;
        element.setAttribute(refAttribute, reference);
        const inputType =
            element instanceof HTMLInputElement
                ? (element.type || "text").toLowerCase()
                : null;
        const sensitive = inputType === "password" || inputType === "file";
        let value = null;
        if (!sensitive) {
            if (
                element instanceof HTMLInputElement ||
                element instanceof HTMLTextAreaElement ||
                element instanceof HTMLSelectElement
            ) {
                value = String(element.value || "").slice(0, 1024);
            }
        }
        return {
            reference,
            role: roleFor(element),
            name: nameFor(element),
            value,
            description: (
                element.getAttribute("aria-description") ||
                element.getAttribute("title") ||
                ""
            )
                .replace(/\s+/g, " ")
                .trim()
                .slice(0, 512) || null,
            href:
                element instanceof HTMLAnchorElement && element.href
                    ? element.href.slice(0, 2048)
                    : null,
            inputType,
            disabled: Boolean(element.disabled) || element.getAttribute("aria-disabled") === "true",
            sensitive,
        };
    });

    const rawText = (document.body?.innerText || "")
        .replace(/\u0000/g, "")
        .trim();
    const text = rawText.slice(0, MAX_TEXT);
    const root = document.documentElement;
    return {
        text,
        elements,
        viewport: {
            width: Math.max(0, Math.round(window.innerWidth)),
            height: Math.max(0, Math.round(window.innerHeight)),
            scrollX: Math.round(window.scrollX),
            scrollY: Math.round(window.scrollY),
            documentWidth: Math.max(root?.scrollWidth || 0, document.body?.scrollWidth || 0),
            documentHeight: Math.max(root?.scrollHeight || 0, document.body?.scrollHeight || 0),
        },
        truncated: rawText.length > MAX_TEXT || candidates.length > MAX_ELEMENTS,
    };
})()
