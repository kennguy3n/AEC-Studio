import { useState } from "react";
import { aec } from "../../api/aec";

export interface LayerState {
  name: string;
  color: string;
  visible: boolean;
  locked: boolean;
  current: boolean;
}

interface Props {
  layers: LayerState[];
  onChange: (next: LayerState[]) => void;
}

export function LayerPanel({ layers, onChange }: Props) {
  const [busy, setBusy] = useState(false);

  const dispatch = async (name: string, key: keyof LayerState, value: boolean) => {
    setBusy(true);
    try {
      await aec.draft.setLayerState({ name, key, value });
    } finally {
      setBusy(false);
    }
    onChange(
      layers.map((l) =>
        l.name === name
          ? {
              ...l,
              [key]: value,
              current: key === "current" ? value : l.current && !(key === "visible" && !value),
            }
          : key === "current" && value
            ? { ...l, current: false }
            : l,
      ),
    );
  };

  return (
    <section className="draft-panel" aria-label="Layers" data-testid="layer-panel">
      <div className="draft-panel__title">Layers</div>
      <table className="draft-panel__table">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Vis</th>
            <th scope="col">Lock</th>
            <th scope="col">Cur</th>
          </tr>
        </thead>
        <tbody>
          {layers.map((l) => (
            <tr key={l.name} data-testid={`layer-row-${l.name}`}>
              <td>
                <span
                  className="draft-panel__swatch"
                  style={{ background: l.color }}
                  aria-hidden="true"
                />
                {l.name}
              </td>
              <td>
                <input
                  type="checkbox"
                  checked={l.visible}
                  disabled={busy}
                  onChange={(e) => dispatch(l.name, "visible", e.target.checked)}
                  aria-label={`${l.name} visible`}
                />
              </td>
              <td>
                <input
                  type="checkbox"
                  checked={l.locked}
                  disabled={busy}
                  onChange={(e) => dispatch(l.name, "locked", e.target.checked)}
                  aria-label={`${l.name} locked`}
                />
              </td>
              <td>
                <input
                  type="radio"
                  name="current-layer"
                  checked={l.current}
                  disabled={busy}
                  onChange={() => dispatch(l.name, "current", true)}
                  aria-label={`${l.name} current`}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

export const DEFAULT_LAYERS: LayerState[] = [
  { name: "0", color: "#ffffff", visible: true, locked: false, current: true },
  { name: "Walls", color: "#cccccc", visible: true, locked: false, current: false },
  { name: "Dims", color: "#ffe066", visible: true, locked: false, current: false },
  { name: "Text", color: "#ff8a3d", visible: true, locked: false, current: false },
  { name: "Hatch", color: "#888888", visible: true, locked: false, current: false },
];
