/**
 * Lighting mood picker.
 *
 * Mirrors `crates/aec_render/src/lighting.rs::LightingPresetKind`. The
 * ids and labels match `LightingPreset::from_kind(...)` so the Rust
 * side can resolve a selection by id without any extra mapping.
 */

export type LightingPresetId =
  | "warm_evening"
  | "daylight"
  | "studio"
  | "golden_hour"
  | "blue_twilight"
  | "overcast";

export interface LightingPresetSummary {
  id: LightingPresetId;
  label: string;
  /** Sun color temperature for the badge — matches the Rust preset. */
  sunKelvin: number;
  description: string;
}

export const LIGHTING_PRESETS: LightingPresetSummary[] = [
  {
    id: "warm_evening",
    label: "Warm Evening",
    sunKelvin: 3200,
    description: "Low sun, tungsten fill",
  },
  {
    id: "daylight",
    label: "Daylight",
    sunKelvin: 6500,
    description: "Midday sun",
  },
  {
    id: "studio",
    label: "Studio",
    sunKelvin: 5500,
    description: "Key / fill / rim",
  },
  {
    id: "golden_hour",
    label: "Golden Hour",
    sunKelvin: 3800,
    description: "Warm low-angle sunlight",
  },
  {
    id: "blue_twilight",
    label: "Blue Twilight",
    sunKelvin: 9000,
    description: "Cool dusk + interior accents",
  },
  {
    id: "overcast",
    label: "Overcast",
    sunKelvin: 6700,
    description: "Diffuse cloudy sky",
  },
];

interface Props {
  active: LightingPresetId;
  onChange: (id: LightingPresetId) => void;
}

export function LightingPresetSelector({ active, onChange }: Props) {
  return (
    <fieldset
      className="lighting-preset"
      aria-label="Lighting preset"
      data-testid="lighting-preset-selector"
    >
      <legend>Lighting</legend>
      <div className="lighting-preset__grid">
        {LIGHTING_PRESETS.map((p) => (
          <button
            key={p.id}
            type="button"
            className={`lighting-preset__tile${active === p.id ? " lighting-preset__tile--active" : ""}`}
            aria-pressed={active === p.id}
            data-testid={`lighting-preset-tile-${p.id}`}
            onClick={() => onChange(p.id)}
          >
            <span className="lighting-preset__label">{p.label}</span>
            <span className="lighting-preset__temp">{p.sunKelvin} K</span>
            <span className="lighting-preset__desc">{p.description}</span>
          </button>
        ))}
      </div>
    </fieldset>
  );
}
