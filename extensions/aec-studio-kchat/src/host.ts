/**
 * Typed wrapper around `globalThis.__kchatHost`, the KChat Desktop
 * extension-host bridge.
 *
 * The KChat Desktop runtime injects `__kchatHost` into every `.kcz`
 * extension's sandbox (see uneycom/uney-chat-desktop docs/proposals/
 * foundation/01-architecture.md §6.16). The host is responsible for:
 *
 *   - Validating the call against the manifest's `procedures` list
 *     and denying anything not declared.
 *   - Running the consent gateway + capability gateway.
 *   - Validating the request payload (the host owns the schema).
 *   - Returning a public-safe DTO (redactor applied).
 *
 * This module gives the extension a typed `invokeProcedure` shim
 * that crashes loudly if the bridge is missing (running outside the
 * host) and surfaces typed `HostProcedureError`s instead of silently
 * resolving `undefined`.
 *
 * We deliberately do NOT pull in `zod` or any other validator here
 * — that would bloat the `.kcz` bundle and double the extension's
 * cold-start parse cost. The wire types in `./types.ts` are
 * hand-rolled mirrors of the canonical Electron-side schema; if a
 * future host-side schema drift turns the response into something
 * unexpected, the downstream consumer will hit a typed property
 * access and surface a clean error inside the view's
 * `describeError()` path.
 */

export type HostProcedureErrorKind =
  | "EXTENSION_CAPABILITY_DENIED"
  | "EXTENSION_NOT_INSTALLED"
  | "CONSENT_REQUIRED"
  | "RATE_LIMITED"
  | "INVALID_REQUEST"
  | "HOST_INTERNAL_ERROR"
  | "HOST_PROCEDURE_NOT_FOUND";

const KNOWN_ERROR_KINDS: ReadonlySet<HostProcedureErrorKind> = new Set<
  HostProcedureErrorKind
>([
  "EXTENSION_CAPABILITY_DENIED",
  "EXTENSION_NOT_INSTALLED",
  "CONSENT_REQUIRED",
  "RATE_LIMITED",
  "INVALID_REQUEST",
  "HOST_INTERNAL_ERROR",
  "HOST_PROCEDURE_NOT_FOUND",
]);

export class HostProcedureError extends Error {
  override readonly name = "HostProcedureError";
  constructor(
    public readonly kind: HostProcedureErrorKind,
    public readonly procedureId: string,
    message: string,
  ) {
    super(`${kind} (${procedureId}): ${message}`);
  }
}

export interface RawHostBridge {
  invokeProcedure(
    id: string,
    payload: unknown,
  ): Promise<{
    ok: boolean;
    value?: unknown;
    error?: { kind: string; message: string };
  }>;
  openDeeplink(url: string): Promise<{ ok: boolean; message?: string }>;
}

declare global {
  // eslint-disable-next-line no-var
  var __kchatHost: RawHostBridge | undefined;
}

function host(): RawHostBridge {
  const bridge = globalThis.__kchatHost;
  if (!bridge) {
    throw new HostProcedureError(
      "EXTENSION_NOT_INSTALLED",
      "(host)",
      "host bridge not injected — extension is running outside KChat Desktop",
    );
  }
  return bridge;
}

function asKnownKind(kind: string): HostProcedureErrorKind {
  return (KNOWN_ERROR_KINDS as ReadonlySet<string>).has(kind)
    ? (kind as HostProcedureErrorKind)
    : "HOST_INTERNAL_ERROR";
}

/**
 * Invoke a host procedure declared in `manifest.json#permissions.procedures`.
 * Throws `HostProcedureError` on host-side failure.
 */
export async function invokeProcedure<T>(
  procedureId: string,
  payload: unknown,
): Promise<T> {
  const raw = await host().invokeProcedure(procedureId, payload);
  if (!raw.ok) {
    const err = raw.error ?? {
      kind: "HOST_INTERNAL_ERROR",
      message: "no error body returned",
    };
    throw new HostProcedureError(
      asKnownKind(err.kind),
      procedureId,
      err.message,
    );
  }
  return raw.value as T;
}

// ---------------------------------------------------------------
// Procedure-typed wrappers
//
// Mirror the canonical KChat Desktop procedure registry. Keep the
// names in sync with `manifest.json#permissions.procedures`.
// ---------------------------------------------------------------

export interface KchatMessage {
  id: string;
  channelId: string;
  authorId: string;
  authorDisplayName: string;
  bodyMarkdown: string;
  postedAt: string;
  permalink?: string | null;
}

export interface KchatQueryMessagesRequest {
  channelId: string;
  /** ISO-8601; only return messages posted strictly after this. */
  since?: string;
  /** Maximum number of messages to return (host may cap further). */
  limit?: number;
}

export interface KchatQueryMessagesResponse {
  messages: readonly KchatMessage[];
}

export interface KchatSendMessageRequest {
  channelId: string;
  bodyMarkdown: string;
  /** Optional attachment — wire format owned by the host. */
  attachment?: {
    filename: string;
    contentType: string;
    /** Base64-encoded payload. */
    dataBase64: string;
  };
}

export interface KchatSendMessageResponse {
  messageId: string;
  postedAt: string;
  permalink?: string | null;
}

export interface KchatConversation {
  id: string;
  name: string;
  /** "channel" / "direct" / "group" — host owns the enum. */
  kind: string;
  teamId?: string | null;
}

export interface KchatQueryConversationsResponse {
  conversations: readonly KchatConversation[];
}

export function queryMessages(
  req: KchatQueryMessagesRequest,
): Promise<KchatQueryMessagesResponse> {
  return invokeProcedure<KchatQueryMessagesResponse>(
    "kchat.query_messages",
    req,
  );
}

export function sendMessage(
  req: KchatSendMessageRequest,
): Promise<KchatSendMessageResponse> {
  return invokeProcedure<KchatSendMessageResponse>(
    "kchat.send_message",
    req,
  );
}

export function queryConversations(): Promise<KchatQueryConversationsResponse> {
  return invokeProcedure<KchatQueryConversationsResponse>(
    "kchat.query_conversations",
    {},
  );
}

/** Open a deeplink (e.g. `aecstudio://review?thread=<id>`). */
export async function openDeeplink(url: string): Promise<void> {
  const result = await host().openDeeplink(url);
  if (!result.ok) {
    throw new HostProcedureError(
      "EXTENSION_CAPABILITY_DENIED",
      "deeplink.open_external",
      result.message ?? "host refused deeplink",
    );
  }
}
