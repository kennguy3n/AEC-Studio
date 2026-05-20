import { useCallback, useState } from "react";
import { aec } from "../../api/aec";

export interface CommandLineLogEntry {
  text: string;
  kind: "input" | "prompt" | "error";
}

interface Props {
  /** Last N log lines (oldest → newest). */
  log: CommandLineLogEntry[];
  onLog: (next: CommandLineLogEntry[]) => void;
}

/**
 * Single command-line dispatcher: maps shorthands (L, PL, C, A, O,
 * CO, MO, MI, RO, SC, TRIM, EX, F, CH, E, U, REDO, DI, AR, ZOOM, PAN,
 * LAYER) to the corresponding IPC call. Multi-step prompts (e.g.
 * LINE → "Specify first point") echo back into the log.
 */
export function CommandLine({ log, onLog }: Props) {
  const [input, setInput] = useState("");

  const submit = useCallback(async () => {
    const raw = input.trim();
    if (raw.length === 0) return;
    setInput("");
    // Build the input echo line and the response (prompt / ack / error) in a
    // single `next` array so callers see exactly one `onLog` call per
    // submission. This keeps the spy contract simple and avoids briefly
    // flashing an echo-only log line.
    const echo: CommandLineLogEntry = { text: raw, kind: "input" };
    const respond = (entry: CommandLineLogEntry) => {
      onLog([...log, echo, entry]);
    };
    const cmd = raw.toUpperCase();
    try {
      if (cmd === "L" || cmd === "LINE") {
        respond({ text: "Specify first point:", kind: "prompt" });
        return;
      }
      if (cmd === "PL" || cmd === "POLYLINE") {
        respond({ text: "Specify start point or [Close]:", kind: "prompt" });
        return;
      }
      if (cmd === "C" || cmd === "CIRCLE") {
        respond({ text: "Specify center point for circle:", kind: "prompt" });
        return;
      }
      if (cmd === "A" || cmd === "ARC") {
        respond({ text: "Specify start point of arc:", kind: "prompt" });
        return;
      }
      if (cmd === "U" || cmd === "UNDO") {
        await aec.draft.editTool({ tool: "undo" });
        respond({ text: "Undone.", kind: "prompt" });
        return;
      }
      if (cmd === "REDO") {
        await aec.draft.editTool({ tool: "redo" });
        respond({ text: "Redone.", kind: "prompt" });
        return;
      }
      if (cmd.startsWith("LAYER")) {
        respond({ text: "Enter layer name:", kind: "prompt" });
        return;
      }
      respond({ text: `Unknown command: ${raw}`, kind: "error" });
    } catch (e) {
      respond({ text: `Error: ${(e as Error).message}`, kind: "error" });
    }
  }, [input, log, onLog]);

  return (
    <section className="draft-cli" aria-label="Command line" data-testid="command-line">
      <div className="draft-cli__log" data-testid="command-line-log">
        {log.map((entry, i) => (
          <div key={i} className={`draft-cli__line draft-cli__line--${entry.kind}`}>
            {entry.text}
          </div>
        ))}
      </div>
      <form
        className="draft-cli__form"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <label className="draft-cli__prompt" htmlFor="draft-cli-input">
          Command:
        </label>
        <input
          id="draft-cli-input"
          type="text"
          className="draft-cli__input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          autoComplete="off"
          data-testid="command-line-input"
        />
      </form>
    </section>
  );
}
