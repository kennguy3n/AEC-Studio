export type RenderPresetKey =
  | "quick"
  | "standard"
  | "high"
  | "studio"
  | "eevee_preview"
  | "walkthrough"
  | "panorama";

const PRESETS: { id: RenderPresetKey; label: string; description: string }[] = [
  { id: "eevee_preview", label: "EEVEE Preview", description: "Fast realtime preview" },
  { id: "quick", label: "Quick", description: "32 samples" },
  { id: "standard", label: "Standard", description: "128 samples" },
  { id: "high", label: "High", description: "256 samples" },
  { id: "studio", label: "Studio", description: "1024 samples" },
  { id: "walkthrough", label: "Walkthrough", description: "Video animation" },
  { id: "panorama", label: "Panorama", description: "Equirectangular" },
];

interface Props {
  active: RenderPresetKey;
  onChange: (preset: RenderPresetKey) => void;
}

export function PresetSelector({ active, onChange }: Props) {
  return (
    <fieldset
      className="render-preset"
      aria-label="Preset selector"
      data-testid="preset-selector"
    >
      <legend>Preset</legend>
      {PRESETS.map((p) => (
        <label
          key={p.id}
          className={`render-preset__row${active === p.id ? " render-preset__row--active" : ""}`}
          data-testid={`preset-row-${p.id}`}
        >
          <input
            type="radio"
            name="preset"
            checked={active === p.id}
            onChange={() => onChange(p.id)}
            data-testid={`preset-input-${p.id}`}
          />
          <span className="render-preset__label">{p.label}</span>
          <span className="render-preset__desc">{p.description}</span>
        </label>
      ))}
    </fieldset>
  );
}
