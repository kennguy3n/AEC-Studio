/**
 * Phase 17 Group B Task 11 — Real `MaterialPanel`.
 *
 * Replaces the previous hardcoded 8-swatch stub with a live panel
 * that reads from the project's `MaterialLibrary` via
 * `aec.design.listMaterials()` (preload bridge → napi backend, or
 * the in-process renderer fallback in `renderer-backend.ts` for
 * tests / vite-preview). Each swatch renders a PBR-style preview
 * (linear-space albedo → sRGB, with a roughness-derived gloss
 * highlight + metallic specular ring) so designers can scan the
 * library at a glance without paying a render-time price for a
 * full PBR sphere thumbnail.
 *
 * Clicking a swatch opens the {@link MaterialInspector} with
 * editable sliders for albedo (color picker), metallic, roughness,
 * IOR, and transmission. Every slider change is debounced through a
 * monotonic generation counter (mirrors `AssetBrowser`'s
 * `fetchGenerationRef` discipline) so a fast drag of the slider
 * does not produce a stale-write race where an older patch
 * overwrites a newer one. The bridge's
 * `aec.design.updateMaterial(materialId, patch)` IS the live
 * preview path: the napi backend mutates the project's
 * `MaterialStore` in place and the path tracer / PBR preview
 * picks up the change on the next frame. We deliberately do NOT
 * route through `commandApply` here — the existing
 * `design.paint_material` command targets a specific entity and
 * is not the right shape for "edit the material library"; if
 * undo/redo of material-library edits is needed later, a dedicated
 * `design.set_material` command should be added to the engine.
 *
 * Style-tag filter tabs (All / Scandinavian / Industrial / Japandi)
 * push the active tag into the `MaterialListQuery.styleTags`
 * filter and refetch. The bridge's seed pack covers each of these
 * tags so the default project shows a non-empty grid on every tab.
 *
 * Swatches are draggable with a `application/aec-material-id` MIME
 * type (paralleling `AssetBrowser`'s `application/aec-asset-id`).
 * The viewport drop receiver (separate task — viewport hit-testing
 * + `design.paint_material` dispatch) keys off the same MIME to
 * dispatch a paint command onto the entity under the drop point.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { aec, type MaterialSummary, type MaterialUpdate } from "../../api/aec";

/**
 * Style-tag filter tabs. Tag strings match the bridge's lowercase
 * convention (`"scandinavian"`, `"industrial"`, `"japandi"`) — the
 * label is rendered separately so the visible UI reads in title case
 * while the query stays exact-match.
 */
const STYLE_TABS: ReadonlyArray<{ id: string | null; label: string }> = [
  { id: null, label: "All" },
  { id: "scandinavian", label: "Scandinavian" },
  { id: "industrial", label: "Industrial" },
  { id: "japandi", label: "Japandi" },
];

/**
 * The Material Library drag-drop MIME type. Parallels
 * `application/aec-asset-id` from `AssetBrowser` so the viewport
 * drop dispatcher can `getData(MATERIAL_MIME)` to detect a material
 * paint vs. `getData(ASSET_MIME)` for a place-asset.
 */
export const MATERIAL_DRAG_MIME = "application/aec-material-id";

/**
 * Linear → sRGB transfer (IEC 61966-2-1). Used to display the
 * bridge's linear-space `albedo` triple in the swatch and color
 * picker.
 */
function linearToSrgb(c: number): number {
  if (!Number.isFinite(c)) return 0;
  const clamped = Math.min(1, Math.max(0, c));
  return clamped <= 0.0031308
    ? 12.92 * clamped
    : 1.055 * Math.pow(clamped, 1 / 2.4) - 0.055;
}

/**
 * sRGB → linear transfer. Used to convert the color picker's
 * `#rrggbb` (sRGB) output back to the linear-space triple the
 * bridge expects.
 */
function srgbToLinear(c: number): number {
  if (!Number.isFinite(c)) return 0;
  const clamped = Math.min(1, Math.max(0, c));
  return clamped <= 0.04045
    ? clamped / 12.92
    : Math.pow((clamped + 0.055) / 1.055, 2.4);
}

/**
 * Convert a linear-space `[r,g,b]` triple to a CSS `#rrggbb` sRGB
 * hex string for the color picker / swatch background.
 */
function linearToHex(rgb: readonly [number, number, number]): string {
  const toByte = (c: number) =>
    Math.round(linearToSrgb(c) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${toByte(rgb[0])}${toByte(rgb[1])}${toByte(rgb[2])}`;
}

/**
 * Parse a `#rrggbb` or `#rgb` sRGB hex string to a linear `[r,g,b]`
 * triple. Falls back to opaque white on parse failure so the bridge
 * never receives `NaN` / out-of-range values.
 */
function hexToLinear(hex: string): [number, number, number] {
  let value = hex.trim();
  if (value.startsWith("#")) value = value.slice(1);
  if (value.length === 3) {
    value = value
      .split("")
      .map((c) => c + c)
      .join("");
  }
  if (value.length !== 6 || !/^[0-9a-fA-F]{6}$/.test(value)) {
    return [1, 1, 1];
  }
  const r = parseInt(value.slice(0, 2), 16) / 255;
  const g = parseInt(value.slice(2, 4), 16) / 255;
  const b = parseInt(value.slice(4, 6), 16) / 255;
  return [srgbToLinear(r), srgbToLinear(g), srgbToLinear(b)];
}

/**
 * Produce a CSS `background` value that approximates a PBR sphere
 * preview without paying the cost of a real GPU pipeline: a base
 * albedo fill, a radial gloss highlight whose intensity scales
 * with `1 - roughness`, and a metallic specular ring tinted by the
 * albedo when `metallic > 0.5` (matches the dielectric/conductor
 * Fresnel separation in `crates/aec_render/src/material.rs`).
 */
function swatchBackground(mat: MaterialSummary): string {
  const base = linearToHex(mat.albedo);
  const gloss = Math.max(0, Math.min(1, 1 - mat.roughness));
  const highlightAlpha = (0.15 + 0.5 * gloss).toFixed(3);
  const radial = `radial-gradient(circle at 30% 30%, rgba(255,255,255,${highlightAlpha}) 0%, rgba(255,255,255,0) 55%)`;
  if (mat.metallic > 0.5) {
    // Metallic surfaces tint the specular by the base color (no
    // achromatic dielectric highlight) — replicate by mixing the
    // albedo at higher alpha across the whole face.
    const tint = `radial-gradient(circle at 70% 70%, ${base} 0%, transparent 60%)`;
    return `${radial}, ${tint}, ${base}`;
  }
  return `${radial}, ${base}`;
}

/**
 * Top-level material panel. Owns the active style-tag filter and
 * the currently-selected material; delegates editing to
 * {@link MaterialInspector} (which receives a stable `onChange`
 * callback so its slider local state stays in sync with the
 * bridge-confirmed `MaterialSummary`).
 */
export function MaterialPanel() {
  const [styleTag, setStyleTag] = useState<string | null>(null);
  const [materials, setMaterials] = useState<MaterialSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Monotonic fetch generation counter — same pattern AssetBrowser
  // uses (see comment there). Discards stale results when the user
  // rapidly toggles tabs.
  const fetchGenRef = useRef(0);

  const refetch = useCallback(async () => {
    fetchGenRef.current += 1;
    const myGen = fetchGenRef.current;
    setLoading(true);
    setError(null);
    try {
      const rows = (await aec.design.listMaterials({
        styleTags: styleTag ? [styleTag] : [],
      })) as MaterialSummary[];
      if (fetchGenRef.current !== myGen) return;
      setMaterials(rows);
    } catch (err) {
      if (fetchGenRef.current !== myGen) return;
      setError(err instanceof Error ? err.message : String(err));
      setMaterials([]);
    } finally {
      if (fetchGenRef.current === myGen) {
        setLoading(false);
      }
    }
  }, [styleTag]);

  useEffect(() => {
    void refetch();
    return () => {
      // Treat any in-flight fetch from this effect run as stale.
      fetchGenRef.current += 1;
    };
  }, [refetch]);

  /**
   * Apply a partial update to a material via the bridge, then
   * merge the bridge-confirmed `MaterialSummary` into local state
   * so the swatch grid + inspector both see the new values.
   *
   * Uses a per-material monotonic generation counter so a fast
   * drag of a slider cannot land an older response on top of a
   * newer one (bridge calls are async; the network/IPC order is
   * not strictly preserved on heavy loads).
   */
  const updateGenRef = useRef<Map<string, number>>(new Map());
  const onMaterialChange = useCallback(
    async (materialId: string, patch: MaterialUpdate) => {
      const prev = updateGenRef.current.get(materialId) ?? 0;
      const myGen = prev + 1;
      updateGenRef.current.set(materialId, myGen);
      try {
        const next = (await aec.design.updateMaterial(
          materialId,
          patch,
        )) as MaterialSummary;
        if (updateGenRef.current.get(materialId) !== myGen) return;
        setMaterials((cur) =>
          cur.map((m) => (m.materialId === materialId ? next : m)),
        );
      } catch (err) {
        if (updateGenRef.current.get(materialId) !== myGen) return;
        // The bridge layer's validators throw on out-of-range
        // values; surface the message to the inspector via panel
        // state so the user sees *why* the slider snapped back.
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [],
  );

  function onDragStart(e: React.DragEvent, m: MaterialSummary) {
    e.dataTransfer.setData(MATERIAL_DRAG_MIME, m.materialId);
    e.dataTransfer.effectAllowed = "copy";
  }

  const selected = useMemo(
    () => materials.find((m) => m.materialId === selectedId) ?? null,
    [materials, selectedId],
  );

  return (
    <div data-testid="material-panel">
      <div
        className="material-tabs"
        role="tablist"
        aria-label="Material style filter"
      >
        {STYLE_TABS.map((tab) => {
          const active = styleTag === tab.id;
          return (
            <button
              key={tab.label}
              type="button"
              role="tab"
              aria-selected={active}
              className={`material-tabs__tab${active ? " material-tabs__tab--active" : ""}`}
              onClick={() => setStyleTag(tab.id)}
              data-testid={`material-tab-${tab.id ?? "all"}`}
            >
              {tab.label}
            </button>
          );
        })}
      </div>
      {error && (
        <div
          role="alert"
          className="material-panel__error"
          data-testid="material-panel-error"
        >
          {error}
        </div>
      )}
      {loading && materials.length === 0 ? (
        <div data-testid="material-panel-loading">Loading…</div>
      ) : materials.length === 0 ? (
        <div className="card" data-testid="material-panel-empty">
          No materials match this filter.
        </div>
      ) : (
        <div className="material-grid">
          {materials.map((m) => {
            const isSelected = selectedId === m.materialId;
            return (
              <button
                key={m.materialId}
                type="button"
                className="material-grid__item"
                onClick={() => setSelectedId(m.materialId)}
                aria-pressed={isSelected}
                draggable
                onDragStart={(e) => onDragStart(e, m)}
                data-testid={`material-${m.materialId}`}
                title={m.name}
              >
                <span
                  className="material-grid__swatch"
                  style={{ background: swatchBackground(m) }}
                  aria-hidden
                />
                <span className="material-grid__name">{m.name}</span>
              </button>
            );
          })}
        </div>
      )}
      {selected && (
        <MaterialInspector material={selected} onChange={onMaterialChange} />
      )}
    </div>
  );
}

/**
 * Slider props shared across the metallic / roughness / IOR /
 * transmission rows so the inspector renders consistently and the
 * tests can target each control by a stable `data-testid`.
 */
interface SliderRowProps {
  label: string;
  testId: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (next: number) => void;
}

function SliderRow({
  label,
  testId,
  value,
  min,
  max,
  step = 0.01,
  onChange,
}: SliderRowProps) {
  return (
    <div className="material-inspector__row">
      <label htmlFor={testId}>{label}</label>
      <div className="material-inspector__slider">
        <input
          id={testId}
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(e) => {
            const next = parseFloat(e.target.value);
            if (Number.isFinite(next)) onChange(next);
          }}
          data-testid={testId}
          aria-valuemin={min}
          aria-valuemax={max}
          aria-valuenow={value}
        />
        <span
          className="material-inspector__value"
          data-testid={`${testId}-value`}
        >
          {value.toFixed(2)}
        </span>
      </div>
    </div>
  );
}

/**
 * Detail editor for a single material. Receives the
 * bridge-confirmed `MaterialSummary` from the parent so the
 * displayed values can never desync from the source of truth
 * (vs. holding a local copy that drifts after the first slider
 * move). Slider changes are fed back to the parent via
 * `onChange`, which is responsible for staging the `updateMaterial`
 * bridge call.
 */
export function MaterialInspector({
  material,
  onChange,
}: {
  material: MaterialSummary;
  onChange: (materialId: string, patch: MaterialUpdate) => void | Promise<void>;
}) {
  const albedoHex = linearToHex(material.albedo);

  return (
    <section
      className="material-inspector"
      data-testid="material-inspector"
      aria-label={`Inspector for ${material.name}`}
    >
      <h3>{material.name}</h3>
      <div className="material-inspector__row">
        <label htmlFor="material-inspector-albedo">Albedo</label>
        <div className="material-inspector__color">
          <input
            id="material-inspector-albedo"
            type="color"
            value={albedoHex}
            onChange={(e) => {
              const linear = hexToLinear(e.target.value);
              void onChange(material.materialId, { albedo: linear });
            }}
            data-testid="material-inspector-albedo"
            aria-label="Albedo color"
          />
          <span
            className="material-inspector__value"
            data-testid="material-inspector-albedo-hex"
          >
            {albedoHex}
          </span>
        </div>
      </div>
      <SliderRow
        label="Metallic"
        testId="material-inspector-metallic"
        value={material.metallic}
        min={0}
        max={1}
        onChange={(v) =>
          void onChange(material.materialId, { metallic: v })
        }
      />
      <SliderRow
        label="Roughness"
        testId="material-inspector-roughness"
        value={material.roughness}
        min={0}
        max={1}
        onChange={(v) =>
          void onChange(material.materialId, { roughness: v })
        }
      />
      <SliderRow
        label="IOR"
        testId="material-inspector-ior"
        value={material.ior}
        min={1}
        max={5}
        step={0.01}
        onChange={(v) => void onChange(material.materialId, { ior: v })}
      />
      <SliderRow
        label="Transmission"
        testId="material-inspector-transmission"
        value={material.transmission}
        min={0}
        max={1}
        onChange={(v) =>
          void onChange(material.materialId, { transmission: v })
        }
      />
      {material.styleTags.length > 0 && (
        <div className="material-inspector__row">
          <span>Style</span>
          <span data-testid="material-inspector-styletags">
            {material.styleTags.join(", ")}
          </span>
        </div>
      )}
    </section>
  );
}
