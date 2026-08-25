import { For, Show, createSignal, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { GuiDiffReview } from "./types";
import { Icon } from "./Icon";

type Props = {
    native: boolean;
    workspace?: string;
    onReview?: (review: GuiDiffReview | undefined) => void;
};

const messageFor = (error: unknown) =>
    error instanceof Error ? error.message : String(error);

export default function DiffPanel(props: Props) {
    const storageKey = () =>
        `ovim.gui.diff-spec.v1.${encodeURIComponent(props.workspace || "workspace")}`;
    const initialSpec = () => {
        try {
            return (
                window.localStorage.getItem(storageKey()) || "HEAD...WORKTREE"
            );
        } catch {
            return "HEAD...WORKTREE";
        }
    };
    const [review, setReview] = createSignal<GuiDiffReview>();
    const [spec, setSpec] = createSignal(initialSpec());
    const [error, setError] = createSignal("");
    const [loading, setLoading] = createSignal(false);

    const publish = (next: GuiDiffReview | undefined) => {
        setReview(next);
        props.onReview?.(next);
    };

    const refresh = async () => {
        if (!props.native || loading()) return;
        setLoading(true);
        try {
            window.localStorage?.setItem(storageKey(), spec().trim());
        } catch {
            // Persistence is optional in restricted webviews and tests.
        }
        try {
            const next = await invoke<GuiDiffReview>("gui_diff_state", {
                spec: spec().trim() || undefined,
            });
            publish(next);
            setError("");
        } catch (reason) {
            publish(undefined);
            setError(messageFor(reason));
        } finally {
            setLoading(false);
        }
    };

    const openFile = async (path: string) => {
        try {
            await invoke("gui_diff_open_file", {
                spec: spec().trim() || undefined,
                path,
            });
            setError("");
        } catch (reason) {
            setError(messageFor(reason));
        }
    };

    onMount(() => {
        if (props.native) void refresh();
    });

    return (
        <section class="side-panel diff-panel" aria-label="Diff review">
            <header class="side-panel-header">
                <div>
                    <b>Changes</b>
                    <small>{review()?.files.length ?? 0} files</small>
                </div>
                <button
                    type="button"
                    class="diff-refresh"
                    disabled={loading()}
                    aria-label="Refresh diff"
                    onClick={() => void refresh()}
                >
                    <Icon name="source-control" size={16} />
                </button>
            </header>

            <div class="diff-content">
                <label class="diff-comparison">
                    <span>Comparison</span>
                    <input
                        value={spec()}
                        spellcheck={false}
                        onInput={(event) => setSpec(event.currentTarget.value)}
                        onKeyDown={(event) => {
                            if (event.key !== "Enter") return;
                            event.preventDefault();
                            void refresh();
                        }}
                    />
                    <small>Examples: HEAD...WORKTREE, main...WORKTREE</small>
                </label>

                <Show when={error()}>
                    <p class="diff-error" role="alert">
                        {error()}
                    </p>
                </Show>

                <div class="diff-files" role="list" aria-label="Changed files">
                    <For
                        each={review()?.files ?? []}
                        fallback={
                            <p class="output-empty">
                                {loading() ? "Reading changes…" : "No changes"}
                            </p>
                        }
                    >
                        {(file) => (
                            <button
                                type="button"
                                role="listitem"
                                title={`Open ${file.path} as a read-only diff`}
                                onClick={() => void openFile(file.path)}
                            >
                                <span class={`diff-status ${file.status}`}>
                                    {file.status.slice(0, 1).toUpperCase()}
                                </span>
                                <span>{file.path}</span>
                                <small>
                                    <em>+{file.additions}</em>
                                    <i>−{file.deletions}</i>
                                </small>
                            </button>
                        )}
                    </For>
                </div>
            </div>
        </section>
    );
}
