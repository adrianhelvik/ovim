import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { GuiAiProfileOption } from "./types";
import { Icon } from "./Icon";

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
  let root!: HTMLDivElement;
  let trigger!: HTMLButtonElement;
  let search!: HTMLInputElement;

  const selected = createMemo(() => props.profiles.find((profile) => profile.id === props.profile));
  const filtered = createMemo(() => {
    const needle = query().trim().toLowerCase();
    if (!needle) return props.profiles;
    return props.profiles.filter((profile) =>
      `${profile.id} ${profile.provider} ${profile.model}`.toLowerCase().includes(needle));
  });

  const close = (returnToComposer = false) => {
    setOpen(false);
    setQuery("");
    queueMicrotask(returnToComposer ? props.focusInput : () => trigger.focus());
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
  return <div class="chat-run-settings" ref={root!}>
    <button
      ref={trigger!}
      type="button"
      class="chat-run-trigger"
      aria-haspopup="dialog"
      aria-expanded={open()}
      onClick={() => {
        setOpen((value) => !value);
        if (!open()) return;
        queueMicrotask(() => search.focus());
      }}
    >
      <span><b>{props.profile}</b><small>{selected()?.provider}/{selected()?.model}</small></span>
      <em>{props.reasoningEffortSelection === "default" ? `default · ${props.reasoningEffort}` : props.reasoningEffort}</em>
      <Icon name="chevron-down" size={16} />
    </button>
    <Show when={open()}>
      <section class="chat-run-popover" role="dialog" aria-label="AI run settings" onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          close();
        }
      }}>
        <label class="chat-model-search">
          <span>Model profile</span>
          <input
            ref={search!}
            type="search"
            value={query()}
            placeholder="Filter profiles…"
            autocomplete="off"
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </label>
        <div class="chat-model-options" role="listbox" aria-label="Model profiles">
          <For each={filtered()} fallback={<p>No matching profiles</p>}>{(profile) => <button
            type="button"
            role="option"
            aria-selected={profile.id === props.profile}
            onClick={() => {
              props.onProfile?.(profile.id);
              close(true);
            }}
          >
            <span><b>{profile.id}</b><small>{profile.provider}</small></span>
            <em>{profile.model}</em>
          </button>}</For>
        </div>
        <fieldset class="chat-effort-options">
          <legend>Reasoning effort</legend>
          <div><For each={props.reasoningEfforts}>{(effort) => <button
            type="button"
            aria-pressed={effort === props.reasoningEffortSelection}
            title={effort === "default" ? `Use profile default (${props.reasoningEffort})` : undefined}
            onClick={() => {
              props.onReasoningEffort?.(effort);
              close(true);
            }}
          >{effort === "default" ? "Default" : effort}</button>}</For></div>
        </fieldset>
      </section>
    </Show>
  </div>;
}
