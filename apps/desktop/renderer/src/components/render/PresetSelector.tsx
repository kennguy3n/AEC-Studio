/**
 * Render preset picker.
 *
 * Mirrors the bundled presets in `crates/aec_render/src/preset.rs` —
 * the labels/samples/resolution shown here are the same numbers the
 * Rust `RenderPreset::quick()` / `standard()` / ... factory methods
 * return. Keep this table in sync when editing the Rust side.
 */

export type RenderPresetKey =
  | "quick"
  | "standard"
  | "high"
  | "studio"
  | "eevee_preview"
  | "walkthrough"
  | "panorama";

/**
 * Hardware tier as reported by the governor (`RuntimeStatus.tier`).
 * Mapped to a recommended preset in
 * `crates/aec_render/src/preset.rs::recommend_preset`. The same mapping
 * is mirrored client-side so the UI can show the "recommended" badge
 * without round-tripping to the bridge.
 */
export type HardwareTier = "Low" | "Medium" | "High" | "Pro";

interface PresetDetail {
  id: RenderPresetKey;
  label: string;
  shortDescription: string;
  samples: number;
  resolution: [number, number];
  /** Hardware tier this preset is the default for, if any. */
  defaultForTier?: HardwareTier;
}

export const PRESETS: PresetDetail[] = [
  {
    id: "eevee_preview",
    label: "Realtime Preview",
    shortDescription: "Fast realtime preview",
    samples: 64,
    // Matches `RenderPreset::realtime_preview()` in
    // crates/aec_render/src/preset.rs (1280×720) — the on-wire id
    // stays `eevee_preview` for backward compatibility with existing
    // `.aecstudio` project files, but the underlying implementation is
    // now the native PBR rasterizer (see preview.rs / pbr_preview.rs).
    // The governor's `preview_resolution_scale` (still serialized as
    // `eevee_resolution_scale` on the wire for compat) is applied at
    // draw time — the preset itself stores the unscaled resolution.
    resolution: [1280, 720],
  },
  {
    id: "quick",
    label: "Quick",
    shortDescription: "Draft render",
    samples: 32,
    resolution: [1280, 720],
    defaultForTier: "Low",
  },
  {
    id: "standard",
    label: "Standard",
    shortDescription: "Balanced quality",
    samples: 128,
    resolution: [1920, 1080],
    defaultForTier: "Medium",
  },
  {
    id: "high",
    label: "High",
    shortDescription: "Presentation",
    samples: 256,
    // Matches `RenderPreset::high()` in crates/aec_render/src/preset.rs.
    resolution: [1920, 1080],
    defaultForTier: "High",
  },
  {
    id: "studio",
    label: "Studio",
    shortDescription: "Print-ready",
    samples: 1024,
    resolution: [3840, 2160],
    defaultForTier: "Pro",
  },
  {
    id: "walkthrough",
    label: "Walkthrough",
    shortDescription: "Animation",
    samples: 96,
    resolution: [1920, 1080],
  },
  {
    id: "panorama",
    label: "Panorama",
    shortDescription: "Equirectangular 360°",
    samples: 512,
    resolution: [4096, 2048],
  },
];

/** All preset ids in display order — convenient for batch selectors. */
export const PRESET_IDS: RenderPresetKey[] = PRESETS.map((p) => p.id);

/** Pick the preset id recommended for a given hardware tier. */
export function recommendedPresetFor(tier: HardwareTier): RenderPresetKey {
  switch (tier) {
    case "Low":
      return "quick";
    case "Medium":
      return "standard";
    case "High":
      return "high";
    case "Pro":
      return "studio";
  }
}

interface Props {
  active: RenderPresetKey;
  onChange: (preset: RenderPresetKey) => void;
  /**
   * Active hardware tier. When supplied, the preset that's the default
   * for this tier shows a "Recommended for $tier" badge.
   */
  tier?: HardwareTier;
}

export function PresetSelector({ active, onChange, tier }: Props) {
  const recommended = tier ? recommendedPresetFor(tier) : null;
  return (
    <fieldset
      className="render-preset"
      aria-label="Preset selector"
      data-testid="preset-selector"
    >
      <legend>Preset</legend>
      {PRESETS.map((p) => {
        const isRecommended = recommended === p.id;
        return (
          <label
            key={p.id}
            className={`render-preset__row${active === p.id ? " render-preset__row--active" : ""}${isRecommended ? " render-preset__row--recommended" : ""}`}
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
            <span className="render-preset__desc">
              {p.samples} samples · {p.resolution[0]}×{p.resolution[1]}
              {" · "}
              {p.shortDescription}
            </span>
            {isRecommended && (
              <span
                className="render-preset__badge"
                role="status"
                aria-label={`Recommended for ${tier}`}
                data-testid={`preset-recommended-${p.id}`}
              >
                Recommended for {tier}
              </span>
            )}
          </label>
        );
      })}
    </fieldset>
  );
}
