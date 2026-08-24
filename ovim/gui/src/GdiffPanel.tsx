import {
    For,
    Show,
    createEffect,
    createMemo,
    createSignal,
    onCleanup,
    untrack,
} from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Icon } from "./Icon";
import type { GuiGdiffReview } from "./types";

interface GdiffPanelProps {
    native: boolean;
    workspace: string;
    onReview: (review: GuiGdiffReview | undefined) => void;
}

const workspaceLabel = (workspace: string) =>
    workspace.split(/[\\/]/).filter(Boolean).at(-1) || "Git workspace";

export default function GdiffPanel(props: GdiffPanelProps) {
    const [review, setReview] = createSignal<GuiGdiffReview>();
    const [error, setError] = createSignal("");
    const [loading, setLoading] = createSignal(false);
    const [saving, setSaving] = createSignal(false);
    const [path, setPath] = createSignal("");
    const [line, setLine] = createSignal("1");
    const [text, setText] = createSignal("");
    let refreshId = 0;

    const files = () => review()?.files ?? [];
    const commentCounts = createMemo(() => {
        const counts = new Map<string, number>();
        for (const comment of review()?.comments ?? []) {
            counts.set(comment.path, (counts.get(comment.path) ?? 0) + 1);
        }
        return counts;
    });

    const refresh = async (showLoading = true, workspace = props.workspace) => {
        if (
            !props.native ||
            !workspace ||
            saving() ||
            (!showLoading && loading())
        )
            return;
        const id = ++refreshId;
        if (showLoading) setLoading(true);
        try {
            const next = await invoke<GuiGdiffReview>("gui_gdiff_state");
            if (id !== refreshId || workspace !== props.workspace) return;
            setReview(next);
            props.onReview(next);
            setError("");
        } catch (reason) {
            if (id === refreshId && workspace === props.workspace) {
                setError(String(reason));
            }
        } finally {
            if (id === refreshId) setLoading(false);
        }
    };

    createEffect(() => {
        const workspace = props.workspace;
        refreshId += 1;
        setReview(undefined);
        props.onReview(undefined);
        setError("");
        setPath("");
        if (!props.native || !workspace) return;
        void untrack(() => refresh(true, workspace));
        const timer = window.setInterval(
            () => void refresh(false, workspace),
            2_000,
        );
        onCleanup(() => {
            refreshId += 1;
            window.clearInterval(timer);
        });
    });

    createEffect(() => {
        if (!path() || !files().includes(path())) setPath(files()[0] ?? "");
    });

    const start = async () => {
        const workspace = props.workspace;
        if (!props.native || !workspace) return;
        refreshId += 1;
        setLoading(true);
        setError("");
        try {
            await invoke("gui_gdiff_start");
            await refresh(true, workspace);
        } catch (reason) {
            setError(String(reason));
        } finally {
            setLoading(false);
        }
    };

    const updateComment = async (
        action: "add" | "remove",
        commentPath = path(),
        commentLine = Number(line()),
        commentText = text(),
    ) => {
        const workspace = props.workspace;
        if (
            !props.native ||
            !workspace ||
            !commentPath ||
            !Number.isInteger(commentLine) ||
            commentLine < 1 ||
            commentLine > 0xffff_ffff
        ) {
            setError("Choose a changed file and a one-based new-side line.");
            return;
        }
        if (action === "add" && !commentText.trim()) {
            setError("Write a comment before adding it to the review.");
            return;
        }
        const id = ++refreshId;
        setSaving(true);
        setError("");
        try {
            const comments = await invoke<GuiGdiffReview["comments"]>(
                "gui_gdiff_comment",
                {
                    action,
                    path: commentPath,
                    line: commentLine,
                    text: commentText,
                },
            );
            if (id !== refreshId || workspace !== props.workspace) return;
            setReview((current) => {
                const next = current ? { ...current, comments } : current;
                props.onReview(next);
                return next;
            });
            if (action === "add") setText("");
        } catch (reason) {
            setError(String(reason));
        } finally {
            setSaving(false);
        }
    };

    return (
        <section class="side-panel gdiff-panel" aria-label="Diff collaboration">
            <header class="side-panel-header">
                <div>
                    <b>Gdiff review</b>
                    <small>
                        {review()?.running
                            ? review()?.displaySpec || review()?.spec
                            : workspaceLabel(props.workspace)}
                    </small>
                </div>
                <button
                    type="button"
                    class="gdiff-refresh"
                    disabled={loading() || saving()}
                    onClick={() => void refresh()}
                >
                    {loading() ? "Checking…" : "Refresh"}
                </button>
            </header>

            <Show
                when={props.workspace}
                fallback={
                    <p class="panel-empty">
                        Open a file in a Git worktree to start a shared diff
                        review.
                    </p>
                }
            >
                <Show
                    when={review()}
                    fallback={
                        <p class="panel-empty">
                            {loading()
                                ? "Discovering the Gdiff review…"
                                : "Gdiff state is available in the native Ovim GUI."}
                        </p>
                    }
                >
                    {(state) => (
                        <Show
                            when={state().installed}
                            fallback={
                                <div class="gdiff-setup">
                                    <b>Gdiff is unavailable</b>
                                    <p>
                                        Install or link <code>gdiff</code> on
                                        Ovim's PATH, then refresh this tab.
                                    </p>
                                </div>
                            }
                        >
                            <Show
                                when={state().running}
                                fallback={
                                    <div class="gdiff-setup">
                                        <b>No review is running</b>
                                        <p>
                                            Start Gdiff to share its active
                                            comparison and review notes.
                                        </p>
                                        <button
                                            type="button"
                                            disabled={loading()}
                                            onClick={() => void start()}
                                        >
                                            Open Gdiff
                                        </button>
                                    </div>
                                }
                            >
                                <div class="gdiff-content">
                                    <section
                                        class="gdiff-context"
                                        aria-label="Active comparison"
                                    >
                                        <span>Comparison</span>
                                        <b>
                                            {state().displaySpec ||
                                                state().spec}
                                        </b>
                                        <small>{state().repo}</small>
                                        <em>
                                            {state().files.length} changed file
                                            {state().files.length === 1
                                                ? ""
                                                : "s"}
                                        </em>
                                    </section>
                                    <div
                                        class="gdiff-files"
                                        aria-label="Changed files"
                                    >
                                        <For
                                            each={state().files}
                                            fallback={
                                                <p class="panel-empty compact">
                                                    The comparison has no
                                                    changed files.
                                                </p>
                                            }
                                        >
                                            {(file) => (
                                                <button
                                                    type="button"
                                                    classList={{
                                                        selected:
                                                            path() === file,
                                                    }}
                                                    title={file}
                                                    onClick={() =>
                                                        setPath(file)
                                                    }
                                                >
                                                    <Icon
                                                        name="file"
                                                        size={16}
                                                        tone="muted"
                                                    />
                                                    <span>{file}</span>
                                                    <small>
                                                        {commentCounts().get(
                                                            file,
                                                        ) || ""}
                                                    </small>
                                                </button>
                                            )}
                                        </For>
                                    </div>
                                    <form
                                        class="gdiff-comment-form"
                                        onSubmit={(event) => {
                                            event.preventDefault();
                                            void updateComment("add");
                                        }}
                                    >
                                        <b>Add shared comment</b>
                                        <label>
                                            Changed file
                                            <select
                                                aria-label="Changed file"
                                                value={path()}
                                                onChange={(event) =>
                                                    setPath(
                                                        event.currentTarget
                                                            .value,
                                                    )
                                                }
                                            >
                                                <For each={state().files}>
                                                    {(file) => (
                                                        <option value={file}>
                                                            {file}
                                                        </option>
                                                    )}
                                                </For>
                                            </select>
                                        </label>
                                        <label>
                                            New-side line
                                            <input
                                                aria-label="New-side line"
                                                type="number"
                                                min="1"
                                                max="4294967295"
                                                step="1"
                                                value={line()}
                                                onInput={(event) =>
                                                    setLine(
                                                        event.currentTarget
                                                            .value,
                                                    )
                                                }
                                            />
                                        </label>
                                        <label>
                                            Review note
                                            <textarea
                                                aria-label="Review note"
                                                rows="3"
                                                maxLength={64 * 1024}
                                                value={text()}
                                                placeholder="What should the user or agent revisit?"
                                                onInput={(event) =>
                                                    setText(
                                                        event.currentTarget
                                                            .value,
                                                    )
                                                }
                                            />
                                        </label>
                                        <button
                                            type="submit"
                                            disabled={
                                                saving() ||
                                                !state().files.length
                                            }
                                        >
                                            {saving()
                                                ? "Sharing…"
                                                : "Share with Gdiff"}
                                        </button>
                                        <small>
                                            Agent tools share these review
                                            notes.
                                        </small>
                                    </form>
                                    <section
                                        class="gdiff-comments"
                                        aria-label="Shared comments"
                                    >
                                        <header>
                                            <b>Shared comments</b>
                                            <span>
                                                {state().comments.length}
                                            </span>
                                        </header>
                                        <For
                                            each={state().comments}
                                            fallback={
                                                <p class="panel-empty compact">
                                                    No comments yet.
                                                </p>
                                            }
                                        >
                                            {(comment) => (
                                                <article>
                                                    <header>
                                                        <button
                                                            type="button"
                                                            title={`${comment.path}:${comment.line}`}
                                                            onClick={() => {
                                                                setPath(
                                                                    comment.path,
                                                                );
                                                                setLine(
                                                                    String(
                                                                        comment.line,
                                                                    ),
                                                                );
                                                            }}
                                                        >
                                                            {comment.path}:
                                                            {comment.line}
                                                        </button>
                                                        <button
                                                            type="button"
                                                            aria-label={`Remove comment at ${comment.path}:${comment.line}`}
                                                            disabled={saving()}
                                                            onClick={() =>
                                                                void updateComment(
                                                                    "remove",
                                                                    comment.path,
                                                                    comment.line,
                                                                    "",
                                                                )
                                                            }
                                                        >
                                                            Remove
                                                        </button>
                                                    </header>
                                                    <p>{comment.text}</p>
                                                </article>
                                            )}
                                        </For>
                                    </section>
                                </div>
                            </Show>
                        </Show>
                    )}
                </Show>
            </Show>
            <Show when={error()}>
                <p class="gdiff-error" role="alert">
                    {error()}
                </p>
            </Show>
        </section>
    );
}
