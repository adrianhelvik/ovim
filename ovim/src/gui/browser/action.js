(input => {
    const result = (ok, error = null) => ({
        ok,
        error,
        url: location.href,
        title: document.title || "",
    });
    const find = reference =>
        document.querySelector(`[data-ovim-browser-ref="${reference}"]`);
    const dispatchValue = element => {
        element.dispatchEvent(new Event("input", { bubbles: true }));
        element.dispatchEvent(new Event("change", { bubbles: true }));
    };

    try {
        if (input.kind === "scroll") {
            window.scrollBy({ top: input.delta_y, behavior: "instant" });
            return result(true);
        }
        if (input.kind === "press") {
            const allowed = new Set([
                "Escape", "Tab", "ArrowUp", "ArrowDown", "ArrowLeft",
                "ArrowRight", "PageUp", "PageDown", "Home", "End", " ",
            ]);
            if (!allowed.has(input.key)) {
                return result(false, "This key requires manual browser control");
            }
            const target = document.activeElement || document.body;
            target.dispatchEvent(new KeyboardEvent("keydown", {
                key: input.key,
                bubbles: true,
                cancelable: true,
            }));
            target.dispatchEvent(new KeyboardEvent("keyup", {
                key: input.key,
                bubbles: true,
                cancelable: true,
            }));
            return result(true);
        }

        const element = find(input.element);
        if (!element) return result(false, "Element reference is no longer present");
        if (element.disabled || element.getAttribute("aria-disabled") === "true") {
            return result(false, "Element is disabled");
        }
        if (input.kind === "click") {
            const tag = element.tagName.toLowerCase();
            if (tag === "summary") {
                element.click();
                return result(true);
            }
            if (!(element instanceof HTMLAnchorElement)) {
                return result(false, "Agent clicks are limited to navigation links; take manual control for buttons and submissions");
            }
            const destination = new URL(element.href, location.href);
            if (
                !["http:", "https:"].includes(destination.protocol) ||
                destination.username ||
                destination.password
            ) {
                return result(false, "Only credential-free HTTP and HTTPS links can be opened");
            }
            // Assigning the location directly avoids invoking page-owned click handlers,
            // which may have effects beyond following the link.
            location.assign(destination.href);
            return result(true);
        }
        if (input.kind === "type") {
            if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) {
                return result(false, "Element is not a text field");
            }
            const type = (element.getAttribute("type") || "text").toLowerCase();
            if (["password", "file", "hidden"].includes(type)) {
                return result(false, "Sensitive and file inputs require manual browser control");
            }
            element.focus();
            element.value = input.text;
            dispatchValue(element);
            return result(true);
        }
        if (input.kind === "select") {
            if (!(element instanceof HTMLSelectElement)) {
                return result(false, "Element is not a select control");
            }
            if (!Array.from(element.options).some(option => option.value === input.value)) {
                return result(false, "Select option is not present");
            }
            element.value = input.value;
            dispatchValue(element);
            return result(true);
        }
        return result(false, "Unsupported browser action");
    } catch (error) {
        return result(false, String(error));
    }
})
