import {
    For,
    Show,
    createEffect,
    createMemo,
    createSignal,
    onCleanup,
    onMount,
} from "solid-js";
import type { GuiAiProfileOption } from "./types";
import { Icon } from "./Icon";
import { trapDialogFocus } from "./focus";

type Props = {
    profile: string;
    profiles: GuiAiProfileOption[];
    reasoningEffort: string;
    reasoningEffortSelection: string;
    reasoningEfforts: string[];
    onProfile?: (profile: string) => void;
    onReasoningEffort?: (effort: string) => void;
    focusInput: () => void;
};

export default function ChatModelPicker(props: Props) {
    const [open, setOpen] = createSignal(false);
    const [query, setQuery] = createSignal("");
    const [activeOption, setActiveOption] = createSignal(0);
    let root!: HTMLDivElement;
    let trigger!: HTMLButtonElement;
    let search!: HTMLInputElement;

    const selected = createMemo(() =>
        props.profiles.find((profile) => profile.id === props.profile),
    );
    const filtered = createMemo(() => {
        const needle = query().trim().toLowerCase();
        if (!needle) return props.profiles;
        return props.profiles.filter((profile) =>
            `${profile.id} ${profile.provider} ${profile.model}`
                .toLowerCase()
                .includes(needle),
        );
    });

    createEffect(() => {
        const last = Math.max(0, filtered().length - 1);
        if (activeOption() > last) setActiveOption(last);
    });

    const focusOption = (index: number) => {
        if (!filtered().length) return;
        const next = Math.max(0, Math.min(index, filtered().length - 1));
        setActiveOption(next);
        queueMicrotask(() =>
            document.getElementById(`chat-model-option-${next}`)?.focus(),
        );
    };

    const moveOption = (event: KeyboardEvent, index: number) => {
        if (event.key === "ArrowDown") focusOption(index + 1);
        else if (event.key === "ArrowUp") focusOption(index - 1);
        else if (event.key === "Home") focusOption(0);
        else if (event.key === "End") focusOption(filtered().length - 1);
        else return;
        event.preventDefault();
    };

    const close = (returnToComposer = false) => {
        setOpen(false);
        setQuery("");
        queueMicrotask(
            returnToComposer ? props.focusInput : () => trigger.focus(),
        );
    };

    onMount(() => {
        const dismiss = (event: PointerEvent) => {
            if (!open() || root.contains(event.target as Node)) return;
            setOpen(false);
            setQuery("");
        };
        document.addEventListener("pointerdown", dismiss);
        onCleanup(() => document.removeEventListener("pointerdown", dismiss));
    });
    return (
        <div class="chat-run-settings" ref={root!}>
            <button
                ref={trigger!}
                type="button"
                class="chat-run-trigger"
                aria-haspopup="dialog"
                aria-expanded={open()}
                onClick={() => {
                    setOpen((value) => !value);
                    if (!open()) return;
                    const current = filtered().findIndex(
                        (profile) => profile.id === props.profile,
                    );
                    setActiveOption(Math.max(0, current));
                    queueMicrotask(() => search.focus());
                }}
            >
                <span>
                    <b>{props.profile}</b>
                    <small>
                        {selected()?.provider}/{selected()?.model}
                    </small>
                </span>
                <em>
                    {props.reasoningEffortSelection === "default"
                        ? `default · ${props.reasoningEffort}`
                        : props.reasoningEffort}
                </em>
                <Icon name="chevron-down" size={16} />
            </button>
            <Show when={open()}>
                <section
                    class="chat-run-popover"
                    role="dialog"
                    aria-label="AI run settings"
                    onKeyDown={(event) => {
                        if (trapDialogFocus(event, event.currentTarget)) return;
                        if (event.key === "Escape") {
                            event.preventDefault();
                            close();
                        }
                    }}
                >
                    <label class="chat-model-search">
                        <span>Model profile</span>
                        <input
                            ref={search!}
                            type="search"
                            value={query()}
                            placeholder="Filter profiles…"
                            autocomplete="off"
                            onInput={(event) => {
                                setQuery(event.currentTarget.value);
                                setActiveOption(0);
                            }}
                            onKeyDown={(event) => {
                                if (event.key === "ArrowDown") {
                                    event.preventDefault();
                                    focusOption(0);
                                } else if (event.key === "ArrowUp") {
                                    event.preventDefault();
                                    focusOption(filtered().length - 1);
                                }
                            }}
                        />
                    </label>
                    <div
                        class="chat-model-options"
                        role="listbox"
                        aria-label="Model profiles"
                    >
                        <For
                            each={filtered()}
                            fallback={<p>No matching profiles</p>}
                        >
                            {(profile, index) => (
                                <button
                                    id={`chat-model-option-${index()}`}
                                    type="button"
                                    role="option"
                                    aria-selected={profile.id === props.profile}
                                    tabIndex={
                                        index() === activeOption() ? 0 : -1
                                    }
                                    onFocus={() => setActiveOption(index())}
                                    onKeyDown={(event) =>
                                        moveOption(event, index())
                                    }
                                    onClick={() => {
                                        props.onProfile?.(profile.id);
                                        close(true);
                                    }}
                                >
                                    <span>
                                        <b>{profile.id}</b>
                                        <small>{profile.provider}</small>
                                    </span>
                                    <em>{profile.model}</em>
                                </button>
                            )}
                        </For>
                    </div>
                    <fieldset class="chat-effort-options">
                        <legend>Reasoning effort</legend>
                        <div>
                            <For each={props.reasoningEfforts}>
                                {(effort) => (
                                    <button
                                        type="button"
                                        aria-pressed={
                                            effort ===
                                            props.reasoningEffortSelection
                                        }
                                        title={
                                            effort === "default"
                                                ? `Use profile default (${props.reasoningEffort})`
                                                : undefined
                                        }
                                        onClick={() => {
                                            props.onReasoningEffort?.(effort);
                                            close(true);
                                        }}
                                    >
                                        {effort === "default"
                                            ? "Default"
                                            : effort}
                                    </button>
                                )}
                            </For>
                        </div>
                    </fieldset>
                </section>
            </Show>
        </div>
    );
}
