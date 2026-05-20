/**
 * Settings / Preferences page.
 *
 * Surfaces runtime configuration that's reasonable for users to
 * inspect and override. The page deliberately separates *read-only*
 * surfaces (hardware profile — the machine is what it is) from
 * *override* surfaces (AI model tier, render defaults, Blender path,
 * KChat integration).
 *
 * Persistence is **in-memory only** in this build: the page holds the
 * user's choices for the duration of the session and surfaces a
 * `Saved at …` timestamp when the user clicks Save. A persistent
 * `aec.settings.*` bridge method is on the Phase 7 follow-up list; once
 * it lands, `onSave` will write through to the project file. Until
 * then, deliberately do **not** add a fake IPC — surfacing a Save
 * action that silently does nothing on app restart would be worse
 * than the honest in-memory state.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { aec, RuntimeStatus } from "../api/aec";

type AiModelTier = "tiny" | "small" | "medium" | "large";
type RenderPresetKey = "quick" | "standard" | "high" | "studio";
type Region = "metric" | "imperial";

interface SettingsState {
  aiModelTierOverride: AiModelTier | "auto";
  defaultRenderPreset: RenderPresetKey;
  region: Region;
  kchatEnabled: boolean;
  blenderPathOverride: string;
}

const DEFAULT_SETTINGS: SettingsState = {
  aiModelTierOverride: "auto",
  defaultRenderPreset: "standard",
  region: "metric",
  kchatEnabled: false,
  blenderPathOverride: "",
};

export function Settings() {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [settings, setSettings] = useState<SettingsState>(DEFAULT_SETTINGS);
  const [savedAt, setSavedAt] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const s = (await aec.runtime.status()) as RuntimeStatus;
        if (!cancelled) {
          setStatus(s);
        }
      } catch (e: unknown) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const updateSetting = useCallback(
    <K extends keyof SettingsState>(key: K, value: SettingsState[K]) => {
      setSettings((prev) => ({ ...prev, [key]: value }));
      setSavedAt(null);
    },
    [],
  );

  const onSave = useCallback(() => {
    setSavedAt(new Date().toISOString());
  }, []);

  const tierLabel = useMemo(() => {
    if (!status) return "Detecting…";
    return `${status.tier} (${status.cpu.physicalCores}c / ${status.cpu.logicalCores}t, ${(
      status.ramTotalMb / 1024
    ).toFixed(1)} GB RAM)`;
  }, [status]);

  return (
    <div className="settings-page" data-testid="settings-page">
      <header className="settings-page__header">
        <h1>Settings</h1>
        <p className="settings-page__subtitle">
          Local preferences. Nothing here is sent off this machine.
        </p>
      </header>

      <section
        className="settings-section"
        data-testid="settings-section-hardware"
        aria-label="Hardware profile"
      >
        <h2>Hardware profile</h2>
        <dl className="settings-list">
          <div>
            <dt>Tier</dt>
            <dd data-testid="settings-hw-tier">{tierLabel}</dd>
          </div>
          <div>
            <dt>GPU</dt>
            <dd>
              {status?.gpu
                ? `${status.gpu.vendor} ${status.gpu.model} (${(status.gpu.vramMb / 1024).toFixed(1)} GB)`
                : "Unknown"}
            </dd>
          </div>
          <div>
            <dt>OS</dt>
            <dd>{status?.os ?? "—"}</dd>
          </div>
        </dl>
      </section>

      <section
        className="settings-section"
        data-testid="settings-section-ai"
        aria-label="AI model tier"
      >
        <h2>AI model tier</h2>
        <p>
          Override the per-tier AI model the governor would otherwise
          pick. Leave on <em>Auto</em> to follow the hardware tier.
        </p>
        <select
          data-testid="settings-ai-tier"
          aria-label="AI model tier override"
          value={settings.aiModelTierOverride}
          onChange={(e) =>
            updateSetting(
              "aiModelTierOverride",
              e.target.value as SettingsState["aiModelTierOverride"],
            )
          }
        >
          <option value="auto">Auto (use hardware tier)</option>
          <option value="tiny">Tiny — laptop-class</option>
          <option value="small">Small</option>
          <option value="medium">Medium</option>
          <option value="large">Large — workstation only</option>
        </select>
      </section>

      <section
        className="settings-section"
        data-testid="settings-section-render"
        aria-label="Render defaults"
      >
        <h2>Render defaults</h2>
        <label>
          Default preset
          <select
            data-testid="settings-render-preset"
            aria-label="Default render preset"
            value={settings.defaultRenderPreset}
            onChange={(e) =>
              updateSetting(
                "defaultRenderPreset",
                e.target.value as RenderPresetKey,
              )
            }
          >
            <option value="quick">Quick</option>
            <option value="standard">Standard</option>
            <option value="high">High</option>
            <option value="studio">Studio</option>
          </select>
        </label>
      </section>

      <section
        className="settings-section"
        data-testid="settings-section-region"
        aria-label="Units and region"
      >
        <h2>Units &amp; region</h2>
        <fieldset>
          <legend>Default unit system</legend>
          <label>
            <input
              type="radio"
              name="region"
              value="metric"
              data-testid="settings-region-metric"
              checked={settings.region === "metric"}
              onChange={() => updateSetting("region", "metric")}
            />
            Metric (mm, m, kg)
          </label>
          <label>
            <input
              type="radio"
              name="region"
              value="imperial"
              data-testid="settings-region-imperial"
              checked={settings.region === "imperial"}
              onChange={() => updateSetting("region", "imperial")}
            />
            Imperial (in, ft, lb)
          </label>
        </fieldset>
      </section>

      <section
        className="settings-section"
        data-testid="settings-section-kchat"
        aria-label="KChat integration"
      >
        <h2>KChat integration</h2>
        <p>
          Optional one-way publishing of artefacts (renders, sheets,
          packs) to a KChat thread, with inline review comments
          flowing back into the audit trail. Disabled by default —
          AEC Studio remains fully functional without it.
        </p>
        <label>
          <input
            type="checkbox"
            data-testid="settings-kchat-enabled"
            checked={settings.kchatEnabled}
            onChange={(e) => updateSetting("kchatEnabled", e.target.checked)}
          />
          Enable KChat integration
        </label>
      </section>

      <section
        className="settings-section"
        data-testid="settings-section-blender"
        aria-label="Blender path"
      >
        <h2>Blender path override</h2>
        <p>
          Auto-discovered from <code>AEC_BLENDER_BIN</code>, the
          platform's known install paths, or <code>PATH</code>. Set
          this only if you need to pin a specific Blender install.
        </p>
        <input
          type="text"
          data-testid="settings-blender-path"
          aria-label="Blender path"
          placeholder="/usr/bin/blender (auto-discovered)"
          value={settings.blenderPathOverride}
          onChange={(e) =>
            updateSetting("blenderPathOverride", e.target.value)
          }
        />
      </section>

      <footer className="settings-page__footer">
        <button
          type="button"
          data-testid="settings-save"
          onClick={onSave}
        >
          Save preferences
        </button>
        {savedAt && (
          <span
            data-testid="settings-saved-stamp"
            className="settings-page__saved-stamp"
          >
            Saved at {new Date(savedAt).toLocaleTimeString()}
          </span>
        )}
        {error && (
          <span data-testid="settings-error" className="settings-page__error">
            {error}
          </span>
        )}
      </footer>
    </div>
  );
}

export default Settings;
