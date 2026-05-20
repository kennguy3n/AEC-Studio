import { useEffect, useState } from "react";
import { aec, AiTool } from "../../api/aec";
import {
  LayoutSuggestionsPanel,
  LayoutSuggestionProposal,
  suggestLayout,
} from "./LayoutSuggestionsPanel";

export function DesignAiPanel() {
  const [tools, setTools] = useState<AiTool[]>([]);
  const [activeRoom, setActiveRoom] = useState<string | null>(null);
  const [proposals, setProposals] = useState<LayoutSuggestionProposal[] | null>(
    null,
  );

  useEffect(() => {
    let alive = true;
    void aec.ai.listTools().then((t) => {
      if (alive) setTools(t as AiTool[]);
    });
    return () => {
      alive = false;
    };
  }, []);

  const handleSuggest = async (roomAnchor: string) => {
    const rows = await suggestLayout(roomAnchor);
    setProposals(rows);
    return rows;
  };

  const handleApply = (_proposal: LayoutSuggestionProposal) => {
    // The diff engine on the Rust side already turns layout suggestions
    // into Insert / Update operations. The user accepts via the diff
    // preview flow (ai.acceptDiff); from this panel we simply mark the
    // proposal applied so it disappears from the active suggestions
    // list. The host route is responsible for the diff preview UI.
    setProposals(
      (prev) => prev?.filter((p) => p !== _proposal) ?? null,
    );
  };

  return (
    <section className="design-panel" aria-label="AI assistant">
      <div className="design-panel__title">PrismML</div>
      <p style={{ fontSize: 12, color: "var(--aec-color-text-secondary)" }}>
        Local AI. All tool calls run on this device.
      </p>
      <label
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 4,
          fontSize: 12,
          marginBottom: 8,
        }}
      >
        <span>Active room (anchor)</span>
        <input
          type="text"
          placeholder="ent_living_room"
          value={activeRoom ?? ""}
          onChange={(e) => setActiveRoom(e.target.value || null)}
          data-testid="design-ai-room-anchor"
        />
      </label>
      <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
        {tools
          .filter((t) => t.scope.includes("design"))
          .map((t) => (
            <li key={t.id} style={{ marginBottom: 8 }}>
              <button
                type="button"
                className="button button--ghost"
                style={{ width: "100%", justifyContent: "flex-start" }}
                data-testid={`ai-tool-${t.id}`}
              >
                {t.id.replace(/_/g, " ")}
              </button>
            </li>
          ))}
      </ul>
      <LayoutSuggestionsPanel
        roomAnchor={activeRoom}
        proposals={proposals}
        onSuggest={handleSuggest}
        onApplyProposal={handleApply}
      />
    </section>
  );
}
