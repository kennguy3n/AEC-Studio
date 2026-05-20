import { useState } from "react";
import { aec } from "../../api/aec";

/**
 * One row in the proposed layout — mirrors the Rust `LayoutProposal`
 * struct from `crates/aec_ai/src/layout_suggestion.rs`. Either
 * `assetId` (new placement) or `targetEntity` (reposition existing) is
 * always set; both can never be empty.
 */
export interface LayoutSuggestionProposal {
  assetId?: string | null;
  targetEntity?: string | null;
  positionMm: [number, number, number];
  rotationDeg: number;
}

interface Props {
  /** Room (space) entity id this layout is anchored to. */
  roomAnchor: string | null;
  /** Current proposals to display, or `null` until the user runs the tool. */
  proposals: LayoutSuggestionProposal[] | null;
  /**
   * Invoked when the user clicks "Suggest layout". Lets the parent
   * decide how the underlying AI call is wired (real sidecar vs. test
   * fixture vs. in-process backend).
   */
  onSuggest: (roomAnchor: string) => Promise<LayoutSuggestionProposal[]>;
  /** Render a per-proposal Apply button; the parent applies the diff. */
  onApplyProposal: (proposal: LayoutSuggestionProposal) => void;
}

/**
 * "Layout suggestions" UI for the Design mode AI panel. Triggers the
 * `layout_suggestion` AI tool for the active room and renders each
 * proposal with its position, rotation, and whether it's a new
 * placement or a reposition of an existing piece of furniture.
 */
export function LayoutSuggestionsPanel({
  roomAnchor,
  proposals,
  onSuggest,
  onApplyProposal,
}: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = async () => {
    if (!roomAnchor) return;
    setBusy(true);
    setError(null);
    try {
      await onSuggest(roomAnchor);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      className="design-panel design-panel--layout"
      aria-label="Layout suggestions"
      data-testid="layout-suggestions-panel"
    >
      <div className="design-panel__title">Layout suggestions</div>
      <p style={{ fontSize: 12, color: "var(--aec-color-text-secondary)" }}>
        Propose a furniture arrangement for the active room. Anchor:
        {" "}
        {roomAnchor ?? "(no room selected)"}
      </p>
      <button
        type="button"
        className="button button--ghost"
        onClick={run}
        disabled={!roomAnchor || busy}
        data-testid="layout-suggestions-run"
      >
        {busy ? "Thinking…" : "Suggest layout"}
      </button>
      {error && (
        <p
          role="alert"
          style={{ color: "var(--aec-color-danger)", fontSize: 12 }}
          data-testid="layout-suggestions-error"
        >
          {error}
        </p>
      )}
      {proposals && proposals.length === 0 && (
        <p style={{ fontSize: 12 }} data-testid="layout-suggestions-empty">
          No proposals returned for this room.
        </p>
      )}
      {proposals && proposals.length > 0 && (
        <ol
          style={{
            listStyle: "decimal",
            padding: "0 0 0 18px",
            margin: 0,
            fontSize: 12,
          }}
          data-testid="layout-suggestions-list"
        >
          {proposals.map((p, i) => (
            <li
              key={`${p.assetId ?? p.targetEntity ?? "p"}-${i}`}
              data-testid={`layout-suggestion-row-${i}`}
              style={{ marginBottom: 6 }}
            >
              <div>
                <strong>
                  {p.targetEntity ? "Move " : "Place "}
                  {p.assetId ?? p.targetEntity}
                </strong>
              </div>
              <div>
                pos = [{p.positionMm.map((v) => v.toFixed(0)).join(", ")}] mm,
                rot = {p.rotationDeg.toFixed(0)}°
              </div>
              <button
                type="button"
                className="button button--ghost"
                onClick={() => onApplyProposal(p)}
                data-testid={`layout-suggestion-apply-${i}`}
              >
                Apply
              </button>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

/**
 * Helper that runs `aec.ai.plan` for the `layout_suggestion` tool. Kept
 * here (rather than in the panel) so tests can render the component
 * without going through IPC.
 *
 * Returns the proposals from the `parsed` payload that the bridge
 * attaches alongside the `diffId` — the bridge's `AiPlanResponse`
 * mirrors the Rust `LayoutSuggestionResult` shape for the
 * `layout_suggestion` tool. When the parsed payload is absent (e.g. a
 * native backend that hasn't been wired yet) we return an empty array
 * so the panel renders its "no proposals" state instead of crashing.
 */
export async function suggestLayout(
  roomAnchor: string,
): Promise<LayoutSuggestionProposal[]> {
  const response = (await aec.ai.plan({
    tool: "layout_suggestion",
    scope: "design",
    prompt: "Propose furniture arrangement for the selected room.",
    context: { room_anchor: roomAnchor },
    max_entities_modified: 16,
  })) as {
    diffId: string;
    parsed?: {
      tool?: string;
      room_anchor?: string;
      proposals?: Array<{
        asset_id?: string | null;
        target_entity?: string | null;
        position_mm: [number, number, number];
        rotation_deg: number;
      }>;
    } | null;
  };
  const parsed = response.parsed ?? null;
  if (!parsed || parsed.tool !== "layout_suggestion") return [];
  const rows = parsed.proposals ?? [];
  return rows.map((r) => ({
    assetId: r.asset_id ?? null,
    targetEntity: r.target_entity ?? null,
    positionMm: r.position_mm,
    rotationDeg: r.rotation_deg,
  }));
}
