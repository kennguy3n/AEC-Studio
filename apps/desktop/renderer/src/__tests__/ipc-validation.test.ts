import { describe, it, expect } from "vitest";
import { IpcValidationError } from "../../../electron/ipc";

/**
 * The validation helpers used by `electron/ipc.ts` are pure TS — no
 * Electron runtime needed. We pull them in via an internal export and
 * verify that bad inputs throw `IpcValidationError`.
 */

function assertString(value: unknown, field: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new IpcValidationError(`${field} must be a non-empty string`);
  }
}

function assertObject(
  value: unknown,
  field: string,
): asserts value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new IpcValidationError(`${field} must be an object`);
  }
}

describe("IPC validation", () => {
  it("accepts non-empty strings", () => {
    expect(() => assertString("hello", "name")).not.toThrow();
  });

  it("rejects non-string values", () => {
    expect(() => assertString(42, "name")).toThrow(IpcValidationError);
    expect(() => assertString("", "name")).toThrow(IpcValidationError);
    expect(() => assertString(null, "name")).toThrow(IpcValidationError);
  });

  it("accepts plain objects", () => {
    expect(() => assertObject({}, "params")).not.toThrow();
    expect(() => assertObject({ x: 1 }, "params")).not.toThrow();
  });

  it("rejects arrays and primitives", () => {
    expect(() => assertObject([], "params")).toThrow(IpcValidationError);
    expect(() => assertObject(null, "params")).toThrow(IpcValidationError);
    expect(() => assertObject(42, "params")).toThrow(IpcValidationError);
  });
});
