import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { guiKeyInput } from "./guiInput";
import type { GuiAiChat, GuiKeyInput } from "./types";

const FALLBACK_CELL_WIDTH = 8.15;

const splitAtUtf8Offset = (text: string, offset: number) => {
  const limit = Math.max(0, Math.min(offset, new TextEncoder().encode(text).length));
  let bytes = 0;
  let codeUnits = 0;
  for (const character of text) {
    const next = bytes + new TextEncoder().encode(character).length;
    if (next > limit) break;
    bytes = next;
    codeUnits += character.length;
  }
  return [text.slice(0, codeUnits), text.slice(codeUnits)] as const;
};

const chatInputColumns = (root: HTMLElement) => {
  const style = getComputedStyle(root);
  const usableWidth = root.clientWidth
    - (Number.parseFloat(style.paddingLeft) || 0)
    - (Number.parseFloat(style.paddingRight) || 0);
  if (usableWidth <= 0) return 0;
  const probe = document.createElement("span");
  probe.textContent = "M".repeat(32);
  probe.style.cssText = `position:fixed;visibility:hidden;white-space:pre;font:${style.font};`;
  document.body.append(probe);
  const measured = probe.getBoundingClientRect().width / 32;
  probe.remove();
  return Math.max(1, Math.floor(usableWidth / (measured || FALLBACK_CELL_WIDTH)));
};

export type ChatInputUpdate = {
  expectedInput: string;
  expectedCursor: number;
  input: string;
  cursor: number;
  action?: GuiKeyInput;
};

export const utf8OffsetFromTextArea = (text: string, utf16Offset: number) =>
  new TextEncoder().encode(text.slice(0, utf16Offset)).length;

export const utf16OffsetFromUtf8 = (text: string, utf8Offset: number) =>
  splitAtUtf8Offset(text, utf8Offset)[0].length;

export default function ChatComposer(props: {
  chat: GuiAiChat;
  revision?: number;
  bindInput?: (input: HTMLTextAreaElement) => void;
  onUpdate?: (update: ChatInputUpdate) => Promise<void>;
  onWidth?: (columns: number) => void;
}) {
  const [draft, setDraft] = createSignal(props.chat.input);
  let input!: HTMLTextAreaElement;
  let optimisticInput = props.chat.input;
  let optimisticCursor = props.chat.inputCursor;
  let awaiting: { base: string; action: boolean; responseDone: boolean; revision: number } | undefined;
  let mutations = Promise.resolve();

  const resize = () => {
    if (!input) return;
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 220)}px`;
  };

  const applyRemote = () => {
    const remoteInput = props.chat.input;
    const remoteCursor = props.chat.inputCursor;
    if (awaiting) {
      const matchesOptimistic = remoteInput === optimisticInput && remoteCursor === optimisticCursor;
      const actionChangedInput = awaiting.action
        && awaiting.responseDone
        && (props.revision ?? 0) > awaiting.revision
        && remoteInput !== awaiting.base;
      if (!matchesOptimistic && !actionChangedInput) return;
      awaiting = undefined;
    }
    optimisticInput = remoteInput;
    optimisticCursor = remoteCursor;
    setDraft(remoteInput);
    queueMicrotask(() => {
      if (!input) return;
      const cursor = utf16OffsetFromUtf8(remoteInput, remoteCursor);
      input.setSelectionRange(cursor, cursor);
      resize();
    });
  };

  createEffect(applyRemote);

  const publish = (nextInput: string, utf16Cursor: number, action?: GuiKeyInput) => {
    const nextCursor = utf8OffsetFromTextArea(nextInput, utf16Cursor);
    const update: ChatInputUpdate = {
      expectedInput: optimisticInput,
      expectedCursor: optimisticCursor,
      input: nextInput,
      cursor: nextCursor,
      action,
    };
    const base = optimisticInput;
    optimisticInput = action?.key === "Enter" ? "" : nextInput;
    optimisticCursor = action?.key === "Enter" ? 0 : nextCursor;
    if (action?.key === "Enter") setDraft("");
    awaiting = { base, action: Boolean(action), responseDone: false, revision: props.revision ?? 0 };
    mutations = mutations
      .then(() => props.onUpdate?.(update))
      .then(() => {
        if (awaiting) awaiting.responseDone = true;
        applyRemote();
      })
      .catch(() => {
        awaiting = undefined;
        optimisticInput = props.chat.input;
        optimisticCursor = props.chat.inputCursor;
        setDraft(props.chat.input);
      });
    return mutations;
  };

  onMount(() => {
    props.bindInput?.(input);
    resize();
    if (!props.onWidth) return;
    let previous = 0;
    const report = () => {
      const columns = chatInputColumns(input);
      if (columns > 0 && columns !== previous) {
        previous = columns;
        props.onWidth?.(columns);
      }
    };
    const observer = new ResizeObserver(report);
    observer.observe(input);
    report();
    onCleanup(() => observer.disconnect());
  });

  return <div class="chat-composer" classList={{ waiting: props.chat.waiting }}>
    <Show when={props.chat.pendingImages.length}>
      <div class="chat-attachments" aria-label="Pending image attachments">
        <For each={props.chat.pendingImages}>{(name) => <span title={name}>▧ {name}</span>}</For>
      </div>
    </Show>
    <textarea
      ref={input!}
      class="chat-input"
      aria-label="AI chat input"
      value={draft()}
      placeholder="Ask Ovim about this code…"
      rows={2}
      autocomplete="off"
      autocapitalize="off"
      spellcheck={false}
      onInput={(event) => {
        const target = event.currentTarget;
        setDraft(target.value);
        resize();
        void publish(target.value, target.selectionStart);
      }}
      onSelect={(event) => {
        const target = event.currentTarget;
        const cursor = utf8OffsetFromTextArea(target.value, target.selectionStart);
        if (target.value === optimisticInput && cursor !== optimisticCursor) void publish(target.value, target.selectionStart);
      }}
      onKeyDown={(event) => {
        if (event.isComposing) return;
        const submit = event.key === "Enter" && !event.shiftKey;
        const coreAction = submit || event.key === "Tab" || event.key === "Escape";
        if (!coreAction) return;
        event.preventDefault();
        const target = event.currentTarget;
        void publish(target.value, target.selectionStart, guiKeyInput(event));
      }}
    />
    <footer><span>{props.chat.waiting ? "working" : "Enter to send · drop images to attach · Esc to return"}</span><b>{props.chat.reasoningEffort}</b></footer>
  </div>;
}
