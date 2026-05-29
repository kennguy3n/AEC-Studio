/**
 * Discovery helper for `{userData}/aec-kchat-port.json`.
 *
 * AEC Studio writes this file on startup and rewrites it whenever
 * the loopback API server is bound to a fresh port. The extension
 * reads it on activation and re-reads it whenever an API call
 * fails with `AecStudioLocalApiUnavailableError`, since an AEC
 * Studio restart will pick a new random port (the OS hands us a
 * kernel-assigned ephemeral port; the bearer token rotates too).
 *
 * The implementation is pure: the caller injects the file-reading
 * function so the KChat Desktop extension SDK's filesystem
 * capability gates the actual disk access. This keeps the module
 * browser-friendly for tests and lets the SDK enforce the
 * user-data sandbox.
 */
import { MIN_TOKEN_LENGTH, type AecStudioPortFileV1 } from "./types";

export interface PortFileReadOptions {
  /**
   * Reads the raw file content for the host extension SDK.
   * Resolves with `null` when the file is missing — both because
   * AEC Studio is not running and because the path is gated by
   * the SDK's permissions.
   */
  read(): Promise<string | null>;
}

export type PortFileResult =
  | { ok: true; value: AecStudioPortFileV1 }
  | { ok: false; reason: PortFileFailureReason; detail?: string };

export type PortFileFailureReason =
  | "missing"
  | "malformed"
  | "unsupported-version"
  | "invalid-host"
  | "invalid-port"
  | "missing-token";

export async function readPortFile(
  options: PortFileReadOptions,
): Promise<PortFileResult> {
  const raw = await options.read();
  if (raw === null) {
    return { ok: false, reason: "missing" };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    return {
      ok: false,
      reason: "malformed",
      detail: err instanceof Error ? err.message : String(err),
    };
  }
  return validatePortFile(parsed);
}

export function validatePortFile(parsed: unknown): PortFileResult {
  if (!parsed || typeof parsed !== "object") {
    return { ok: false, reason: "malformed", detail: "not an object" };
  }
  const obj = parsed as Record<string, unknown>;
  if (obj.version !== 1) {
    return {
      ok: false,
      reason: "unsupported-version",
      detail: `expected 1, got ${String(obj.version)}`,
    };
  }
  if (obj.host !== "127.0.0.1") {
    return {
      ok: false,
      reason: "invalid-host",
      detail: `expected 127.0.0.1, got ${String(obj.host)}`,
    };
  }
  if (
    typeof obj.port !== "number" ||
    !Number.isInteger(obj.port) ||
    obj.port <= 0 ||
    obj.port > 65535
  ) {
    return {
      ok: false,
      reason: "invalid-port",
      detail: `port=${String(obj.port)}`,
    };
  }
  if (typeof obj.token !== "string" || obj.token.length < MIN_TOKEN_LENGTH) {
    return {
      ok: false,
      reason: "missing-token",
      detail: `token must be at least ${MIN_TOKEN_LENGTH} characters`,
    };
  }
  const startedAt =
    typeof obj.startedAt === "string" ? obj.startedAt : "";
  const pid = typeof obj.pid === "number" ? obj.pid : 0;
  return {
    ok: true,
    value: {
      version: 1,
      host: "127.0.0.1",
      port: obj.port,
      token: obj.token,
      startedAt,
      pid,
    },
  };
}
