import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import {
  CommandLine,
  CommandLineLogEntry,
} from "../components/draft/CommandLine";

describe("CommandLine", () => {
  it("emits LINE prompt when L typed", async () => {
    const spy = vi.fn<[log: CommandLineLogEntry[]], void>();
    render(<CommandLine log={[]} onLog={spy} />);
    const input = screen.getByTestId("command-line-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "L" } });
    fireEvent.submit(input.form!);
    await waitFor(() => expect(spy).toHaveBeenCalled());
    const lastEntries = spy.mock.calls.at(-1)?.[0] ?? [];
    expect(lastEntries.some((e: CommandLineLogEntry) => e.text.includes("Specify first point"))).toBe(true);
  });

  it("flags unknown commands as errors", async () => {
    const spy = vi.fn<[log: CommandLineLogEntry[]], void>();
    render(<CommandLine log={[]} onLog={spy} />);
    const input = screen.getByTestId("command-line-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "ZZZZ" } });
    fireEvent.submit(input.form!);
    await waitFor(() => expect(spy).toHaveBeenCalled());
    const lastEntries = spy.mock.calls.at(-1)?.[0] ?? [];
    expect(lastEntries.some((e: CommandLineLogEntry) => e.kind === "error" && e.text.includes("Unknown command"))).toBe(true);
  });

  it("ignores empty input", () => {
    const spy = vi.fn<[log: CommandLineLogEntry[]], void>();
    render(<CommandLine log={[]} onLog={spy} />);
    const input = screen.getByTestId("command-line-input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.submit(input.form!);
    expect(spy).not.toHaveBeenCalled();
  });

  it("renders supplied log lines", () => {
    const log: CommandLineLogEntry[] = [
      { text: "L", kind: "input" },
      { text: "Specify first point:", kind: "prompt" },
    ];
    render(<CommandLine log={log} onLog={() => {}} />);
    const logEl = screen.getByTestId("command-line-log");
    expect(logEl.textContent).toContain("Specify first point");
  });
});
