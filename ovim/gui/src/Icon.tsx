import { splitProps, type JSX } from "solid-js";
import type { IconName } from "./icons.generated";

const spriteUrl = new URL(
    "../../../gui-design-guide/icons/dist/ovim-icons.svg",
    import.meta.url,
).href;

export type IconTone =
    | "inherit"
    | "muted"
    | "accent"
    | "error"
    | "warning"
    | "information"
    | "success";

export function Icon(props: {
    name: IconName;
    size?: 16 | 20 | 24;
    label?: string;
    tone?: IconTone;
}) {
    const size = () => props.size ?? 16;
    const decorative = () => !props.label;

    return (
        <svg
            class={`icon icon-${props.tone ?? "inherit"}`}
            width={size()}
            height={size()}
            viewBox="0 0 24 24"
            role={decorative() ? undefined : "img"}
            aria-hidden={decorative() ? "true" : undefined}
            aria-label={props.label}
        >
            <use href={`${spriteUrl}#${props.name}`} />
        </svg>
    );
}

type IconButtonProps = Omit<
    JSX.ButtonHTMLAttributes<HTMLButtonElement>,
    "children" | "aria-label" | "title"
> & {
    icon: IconName;
    label: string;
    shortcut?: string;
    size?: 16 | 20 | 24;
    selected?: boolean;
};

export function IconButton(props: IconButtonProps) {
    const [local, buttonProps] = splitProps(props, [
        "icon",
        "label",
        "shortcut",
        "size",
        "selected",
        "class",
    ]);
    const title = () =>
        local.shortcut ? `${local.label} · ${local.shortcut}` : local.label;

    return (
        <button
            {...buttonProps}
            type={buttonProps.type ?? "button"}
            class={`icon-button${local.class ? ` ${local.class}` : ""}`}
            classList={{ selected: local.selected }}
            aria-label={local.label}
            aria-pressed={local.selected}
            title={title()}
        >
            <Icon name={local.icon} size={local.size} />
        </button>
    );
}
