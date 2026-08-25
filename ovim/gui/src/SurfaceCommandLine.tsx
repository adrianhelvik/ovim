import { Show, createEffect, createSignal } from "solid-js";

export interface CommandExecutionResult {
    ok: boolean;
    message?: string;
}

interface SurfaceCommandLineProps {
    active: boolean;
    requestSerial: number;
    surface: string;
    completions: readonly string[];
    onExecute: (command: string) => Promise<CommandExecutionResult>;
    onDismiss: () => void;
}

export default function SurfaceCommandLine(props: SurfaceCommandLineProps) {
    const [value, setValue] = createSignal("");
    const [message, setMessage] = createSignal("");
    const [running, setRunning] = createSignal(false);
    let input: HTMLInputElement | undefined;

    createEffect(() => {
        void props.requestSerial;
        if (!props.active) return;
        setValue("");
        setMessage("");
        queueMicrotask(() => input?.focus({ preventScroll: true }));
    });

    const complete = () => {
        const prefix = value().trim().replace(/^:/, "").toLowerCase();
        if (!prefix || prefix.includes(" ")) return;
        const matches = props.completions.filter((name) =>
            name.startsWith(prefix),
        );
        if (matches.length === 1) setValue(matches[0]);
    };

    const submit = async () => {
        if (running()) return;
        setRunning(true);
        setMessage("");
        try {
            const result = await props.onExecute(value());
            if (result.ok) props.onDismiss();
            else setMessage(result.message || "Command failed");
        } finally {
            setRunning(false);
        }
    };

    return (
        <Show when={props.active}>
            <form
                class="surface-command-line"
                data-gui-native-control
                aria-label={`${props.surface} command`}
                onSubmit={(event) => {
                    event.preventDefault();
                    void submit();
                }}
                onKeyDown={(event) => {
                    if (event.key === "Escape") {
                        event.preventDefault();
                        props.onDismiss();
                    } else if (event.key === "Tab") {
                        event.preventDefault();
                        complete();
                    }
                }}
            >
                <span class="surface-command-context">
                    COMMAND · {props.surface.toUpperCase()}
                </span>
                <label>
                    <span aria-hidden="true">:</span>
                    <input
                        ref={input}
                        data-gui-native-control
                        value={value()}
                        disabled={running()}
                        autocomplete="off"
                        autocapitalize="off"
                        spellcheck={false}
                        aria-label={`${props.surface} command input`}
                        onInput={(event) => {
                            setValue(event.currentTarget.value);
                            setMessage("");
                        }}
                    />
                </label>
                <Show when={message()}>
                    <em role="alert">{message()}</em>
                </Show>
            </form>
        </Show>
    );
}
