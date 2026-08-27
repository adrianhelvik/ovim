export type BrowserCommand =
    | { kind: "close" }
    | { kind: "navigate"; url: string }
    | { kind: "history"; direction: "back" | "forward"; count: number }
    | { kind: "reload" }
    | { kind: "stop" }
    | { kind: "select_relative_tab"; delta: number }
    | { kind: "select_tab"; position: number };

export type BrowserCommandParseResult =
    { ok: true; command: BrowserCommand } | { ok: false; message: string };

export const BROWSER_COMMAND_NAMES = [
    "back",
    "forward",
    "goto",
    "q",
    "quit",
    "reload",
    "stop",
    "tabgoto",
    "tabnext",
    "tabprev",
] as const;

export const normalizeBrowserAddress = (value: string) => {
    const address = value.trim();
    if (!address) return address;
    if (/^localhost(?=[:/?#]|$)/i.test(address)) return `http://${address}`;

    const authority = address.split(/[/?#]/, 1)[0];
    const host = authority.split(":", 1)[0];
    const looksLikeNetworkLocation =
        !/\s|@/.test(address) &&
        (host.includes(".") || host.startsWith("[") || /^\d+$/.test(host));
    if (looksLikeNetworkLocation) return `https://${address}`;
    if (/^[a-z][a-z\d+.-]*:/i.test(address)) return address;

    return `https://duckduckgo.com/?${new URLSearchParams({ q: address })}`;
};

const countArgument = (
    raw: string | undefined,
    command: string,
): BrowserCommandParseResult | number => {
    if (raw === undefined) return 1;
    if (!/^\d+$/.test(raw))
        return { ok: false, message: `:${command} expects a positive count` };
    const count = Number(raw);
    if (count < 1 || count > 100)
        return {
            ok: false,
            message: `:${command} count must be between 1 and 100`,
        };
    return count;
};

export const parseBrowserCommand = (
    input: string,
): BrowserCommandParseResult => {
    const text = input.trim().replace(/^:/, "").trim();
    if (!text) return { ok: false, message: "Enter a browser command" };
    const [rawName, ...args] = text.split(/\s+/);
    const name = rawName.toLowerCase();

    if (["q", "q!", "quit", "quit!"].includes(name)) {
        if (args.length)
            return {
                ok: false,
                message: `:${rawName} does not take arguments`,
            };
        return { ok: true, command: { kind: "close" } };
    }
    if (["goto", "go", "open"].includes(name)) {
        const url = normalizeBrowserAddress(args.join(" "));
        return url
            ? { ok: true, command: { kind: "navigate", url } }
            : { ok: false, message: `:${rawName} requires an address` };
    }
    if (name === "back" || name === "forward") {
        if (args.length > 1)
            return { ok: false, message: `:${name} accepts one count` };
        const count = countArgument(args[0], name);
        return typeof count === "number"
            ? {
                  ok: true,
                  command: { kind: "history", direction: name, count },
              }
            : count;
    }
    if (name === "reload" || name === "stop") {
        if (args.length)
            return { ok: false, message: `:${name} does not take arguments` };
        return { ok: true, command: { kind: name } };
    }
    if (["tabnext", "tabn", "tnext"].includes(name)) {
        if (args.length > 1)
            return { ok: false, message: `:${rawName} accepts one count` };
        const count = countArgument(args[0], rawName);
        return typeof count === "number"
            ? {
                  ok: true,
                  command: { kind: "select_relative_tab", delta: count },
              }
            : count;
    }
    if (["tabprev", "tabprevious", "tabp", "tprevious"].includes(name)) {
        if (args.length > 1)
            return { ok: false, message: `:${rawName} accepts one count` };
        const count = countArgument(args[0], rawName);
        return typeof count === "number"
            ? {
                  ok: true,
                  command: { kind: "select_relative_tab", delta: -count },
              }
            : count;
    }
    if (name === "tabgoto") {
        if (args.length !== 1 || !/^\d+$/.test(args[0]))
            return {
                ok: false,
                message: `:${rawName} expects a 1-based tab number`,
            };
        const position = Number(args[0]);
        return position > 0
            ? { ok: true, command: { kind: "select_tab", position } }
            : {
                  ok: false,
                  message: `:${rawName} expects a 1-based tab number`,
              };
    }
    if (["w", "write", "wq", "x"].includes(name))
        return {
            ok: false,
            message:
                ":write is unavailable in a browser tab; use :q to close it",
        };
    return { ok: false, message: `Not a browser command: ${rawName}` };
};
