import { useState } from "react";

interface MaterialSwatch {
  id: string;
  name: string;
  color: string;
  tags: string[];
}

const MATERIALS: MaterialSwatch[] = [
  { id: "wall_white", name: "Wall White", color: "#f3f0e9", tags: ["wall"] },
  { id: "wood_oak", name: "Oak", color: "#b58864", tags: ["wood", "warm"] },
  { id: "wood_walnut", name: "Walnut", color: "#5b3a1c", tags: ["wood", "dark"] },
  { id: "concrete_polished", name: "Polished Concrete", color: "#8e8e8e", tags: ["concrete"] },
  { id: "tile_white", name: "White Tile", color: "#ececec", tags: ["tile", "wet"] },
  { id: "marble_carrara", name: "Carrara Marble", color: "#e7e2db", tags: ["stone"] },
  { id: "fabric_linen", name: "Linen", color: "#d6c9a8", tags: ["fabric"] },
  { id: "metal_brushed", name: "Brushed Metal", color: "#a9aab1", tags: ["metal"] },
];

export function MaterialPanel() {
  const [selected, setSelected] = useState<string | null>(null);
  return (
    <div data-testid="material-panel">
      <div className="material-grid">
        {MATERIALS.map((m) => (
          <button
            key={m.id}
            type="button"
            className="material-grid__item"
            onClick={() => setSelected(m.id)}
            aria-pressed={selected === m.id}
            data-testid={`material-${m.id}`}
          >
            <span
              className="material-grid__swatch"
              style={{ background: m.color }}
              aria-hidden
            />
            <span>{m.name}</span>
          </button>
        ))}
      </div>
      {selected && <MaterialInspector materialId={selected} />}
    </div>
  );
}

export function MaterialInspector({ materialId }: { materialId: string }) {
  const mat = MATERIALS.find((m) => m.id === materialId);
  if (!mat) return null;
  return (
    <section style={{ marginTop: 16 }} data-testid="material-inspector">
      <h3 style={{ fontSize: 13 }}>{mat.name}</h3>
      <div className="material-inspector__row">
        <span>Tags</span>
        <span>{mat.tags.join(", ")}</span>
      </div>
      <div className="material-inspector__row">
        <span>Color</span>
        <span style={{ background: mat.color, padding: "0 8px" }}>{mat.color}</span>
      </div>
    </section>
  );
}
