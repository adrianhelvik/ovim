import type { GuiTheme } from "./types";

type Rgb = readonly [number, number, number];

const parseHex = (value: string): Rgb | undefined => {
    const hex = value.trim().replace(/^#/, "");
    if (!/^(?:[0-9a-f]{3}|[0-9a-f]{6})$/i.test(hex)) return undefined;
    const expanded =
        hex.length === 3
            ? Array.from(hex, (character) => character.repeat(2)).join("")
            : hex;
    return [0, 2, 4].map((offset) =>
        Number.parseInt(expanded.slice(offset, offset + 2), 16),
    ) as unknown as Rgb;
};

const channelLuminance = (channel: number) => {
    const normalized = channel / 255;
    return normalized <= 0.04045
        ? normalized / 12.92
        : ((normalized + 0.055) / 1.055) ** 2.4;
};

const luminance = (color: Rgb) =>
    0.2126 * channelLuminance(color[0]) +
    0.7152 * channelLuminance(color[1]) +
    0.0722 * channelLuminance(color[2]);

export const contrastRatio = (first: string, second: string) => {
    const a = parseHex(first);
    const b = parseHex(second);
    if (!a || !b) return undefined;
    const lighter = Math.max(luminance(a), luminance(b));
    const darker = Math.min(luminance(a), luminance(b));
    return (lighter + 0.05) / (darker + 0.05);
};

const toHex = (color: Rgb) =>
    `#${color
        .map((channel) => Math.round(channel).toString(16).padStart(2, "0"))
        .join("")}`;

const mix = (from: Rgb, to: Rgb, amount: number): Rgb =>
    from.map(
        (channel, index) => channel + (to[index] - channel) * amount,
    ) as unknown as Rgb;

export const normalizeMutedColor = (
    background: string,
    foreground: string,
    muted: string,
    minimumRatio = 4.5,
) => {
    const existingRatio = contrastRatio(background, muted);
    if (existingRatio === undefined || existingRatio >= minimumRatio)
        return muted;

    const base = parseHex(muted);
    const target = parseHex(foreground);
    if (!base || !target) return foreground;
    if ((contrastRatio(background, foreground) ?? 0) < minimumRatio)
        return foreground;

    let low = 0;
    let high = 1;
    for (let iteration = 0; iteration < 20; iteration += 1) {
        const amount = (low + high) / 2;
        const candidate = toHex(mix(base, target, amount));
        if ((contrastRatio(background, candidate) ?? 0) >= minimumRatio)
            high = amount;
        else low = amount;
    }
    return toHex(mix(base, target, high));
};

export const colorSchemeFor = (background: string): "dark" | "light" => {
    const parsed = parseHex(background);
    return parsed && luminance(parsed) > 0.35 ? "light" : "dark";
};

export const themeVariables = (theme: GuiTheme) => ({
    "--color-scheme": colorSchemeFor(theme.background),
    "--canvas": theme.background,
    "--text-primary": theme.foreground,
    "--surface-1": theme.surface,
    "--surface-2": `color-mix(in srgb, ${theme.surface} 78%, ${theme.foreground})`,
    "--surface-selected": theme.surfaceSelected,
    "--border": theme.border,
    "--accent": theme.accent,
    "--accent-foreground": theme.accentForeground,
    "--text-secondary": normalizeMutedColor(
        theme.surface,
        theme.foreground,
        theme.muted,
    ),
    "--cursor-line": theme.cursorLine,
    "--selection": theme.selection,
    "--search": theme.search,
    "--error": theme.error,
    "--warning": theme.warning,
    "--info": theme.info,
    "--success": theme.success,
});
