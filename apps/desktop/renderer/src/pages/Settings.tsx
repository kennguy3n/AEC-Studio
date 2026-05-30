/**
 * Settings / Preferences page.
 *
 * Surfaces runtime configuration that's reasonable for users to
 * inspect and override. The page deliberately separates *read-only*
 * surfaces (hardware profile — the machine is what it is) from
 * *override* surfaces (AI model tier, render defaults, KChat
 * integration).
 *
 * Persistence is **in-memory only** in this build: the page holds the
 * user's choices for the duration of the session and surfaces a
 * `Saved at …` timestamp when the user clicks Save. There is no
 * `aec.settings.*` bridge method yet — when one is added, `onSave`
 * will write through to the project file. Until then, deliberately do
 * **not** add a fake IPC — surfacing a Save action that silently
 * does nothing on app restart would be worse than the honest
 * in-memory state.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { aec, RuntimeStatus } from "../api/aec";
import {
  parseLoopbackInstance,
  type KChatLoopbackInstance,
} from "../components/kchat/loopbackInstance";
import {
  applyThemeMode,
  readStoredThemeMode,
  resolveEffectiveTheme,
  type ThemeMode,
  writeStoredThemeMode,
} from "../lib/theme";

type AiModelTier = "tiny" | "small" | "medium" | "large";
type RenderPresetKey = "quick" | "standard" | "high" | "studio";
type Region = "metric" | "imperial";

/**
 * Wire shape returned by
 * `aec.extensions.listLoadDiagnostics()`. Mirrors the
 * `ExtensionLoadDiagnostic` interface in
 * `apps/desktop/electron/bridge.ts` — kept structurally identical
 * so the renderer can render without an extra mapping layer.
 */
interface ExtensionLoadDiagnostic {
  extensionId: string | null;
  path: string;
  stage: string;
  message: string;
}

/**
 * User-facing label for a stable `stage` wire tag. The Rust side
 * pins the strings (see `ExtensionLoadStage::as_wire_str`) so we
 * can do a static lookup here; unknown values fall through to the
 * raw tag (forward-compat with new stage variants).
 */
const EXTENSION_STAGE_LABELS: Readonly<Record<string, string>> = {
  manifest_read: "Manifest unreadable",
  manifest_parse: "Manifest parse error",
  manifest_validation: "Manifest validation failed",
  unsafe_path: "Unsafe path reference",
  signature_verification: "Signature verification failed",
  duplicate_id: "Duplicate extension id",
  asset_pack_install: "Asset-pack install failed",
  ai_tool_resolution: "AI-tool resolution failed",
};

function extensionStageLabel(stage: string): string {
  return EXTENSION_STAGE_LABELS[stage] ?? stage;
}

interface SettingsState {
  aiModelTierOverride: AiModelTier | "auto";
  defaultRenderPreset: RenderPresetKey;
  region: Region;
}

const DEFAULT_SETTINGS: SettingsState = {
  aiModelTierOverride: "auto",
  defaultRenderPreset: "standard",
  region: "metric",
};

export function Settings() {
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [settings, setSettings] = useState<SettingsState>(DEFAULT_SETTINGS);
  const [savedAt, setSavedAt] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [kchatStatus, setKchatStatus] = useState<{
    state: "connected" | "reconnecting" | "disconnected";
    publisherKind: "loopback_http" | "in_memory";
    instance: KChatLoopbackInstance | null;
    /**
     * Bridge-persisted master enable flag mirroring
     * `KChatConfig::enabled`. Source of truth for the
     * "Enable KChat integration" checkbox — we render
     * directly from this rather than keeping a separate
     * `settings.kchatEnabled` so the toggle stays in lockstep
     * with the publish gate on every status poll / project
     * open.
     */
    enabled: boolean;
  } | null>(null);
  const [kchatReloading, setKchatReloading] = useState(false);
  // Inline feedback for the bridge-persisted enable-toggle round
  // trip. `kchat:setEnabled` is fast (read+write on a single
  // `RwLock` inside the bridge) but still async, so the checkbox
  // is disabled while the call is in flight to keep the UI from
  // sending two flips before the first one lands.
  const [kchatTogglePending, setKchatTogglePending] = useState(false);
  const [kchatToggleError, setKchatToggleError] = useState<string | null>(
    null,
  );
  // Surfaces the last `kchat:reload` failure inline next to the
  // reload button. The `onClick={() => void onReloadKChat()}` call
  // site cannot observe a rejected promise (React ignores returned
  // Promises from `onClick`), so without this state any reload
  // failure would silently disappear and the user would think
  // their click did nothing. Cleared on the next successful reload.
  const [kchatReloadError, setKchatReloadError] = useState<string | null>(
    null,
  );
  // Per-extension boot diagnostics buffered by the Rust bridge.
  // `null` while the initial fetch is in flight; the empty array
  // means the bridge saw no failures (the common path — we hide
  // the diagnostics card entirely in that case). A non-empty array
  // is rendered as a read-only list so the user can see which
  // extension failed and why without the bridge having silently
  // dropped it.
  const [extensionDiagnostics, setExtensionDiagnostics] = useState<
    ExtensionLoadDiagnostic[] | null
  >(null);

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
    (async () => {
      try {
        const s = await aec.kchat.status();
        if (!cancelled) {
          setKchatStatus({
            enabled: s.enabled,
            state: s.state,
            publisherKind: s.publisherKind,
            instance: parseLoopbackInstance(s.instanceJson),
          });
        }
      } catch {
        // KChat status is best-effort; surfacing a hard error here
        // would prevent the user from changing other preferences.
      }
    })();
    (async () => {
      try {
        // Frozen for the lifetime of the bridge (extensions are
        // not hot-reloaded in this build), so a single fetch on
        // mount is enough — no polling necessary. The promise
        // resolves quickly because the underlying napi method is
        // a sync read on a process-memory slice.
        const diags = await aec.extensions.listLoadDiagnostics();
        if (!cancelled) {
          setExtensionDiagnostics(diags);
        }
      } catch {
        // Treat any failure here as "no diagnostics to show" —
        // surfacing a hard error in the diagnostics card itself
        // would be a confusing UX (a diagnostics panel that's
        // broken because the diagnostics IPC is broken). The
        // Settings page stays functional for every other
        // preference.
        if (!cancelled) {
          setExtensionDiagnostics([]);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const onReloadKChat = useCallback(async () => {
    setKchatReloading(true);
    try {
      const s = await aec.kchat.reload();
      setKchatStatus({
        enabled: s.enabled,
        state: s.state,
        publisherKind: s.publisherKind,
        instance: parseLoopbackInstance(s.instanceJson),
      });
      // Clear any prior failure once we've successfully refreshed.
      setKchatReloadError(null);
    } catch (e: unknown) {
      // `kchat:reload` can reject when the bridge is mid-restart,
      // the discovered socket is unreachable, or napi serialization
      // fails. The `onClick={() => void onReloadKChat()}` call site
      // throws away the returned promise, so we must capture the
      // error here or it propagates as an unhandled rejection and
      // the user sees no feedback. We deliberately surface the
      // message inline rather than reusing the page-level `error`
      // state — a transient KChat reconnect failure shouldn't make
      // the whole Settings page look broken.
      setKchatReloadError(e instanceof Error ? e.message : String(e));
    } finally {
      setKchatReloading(false);
    }
  }, []);

  /**
   * Flip the bridge-persisted master KChat enable switch via
   * `kchat:setEnabled`. Mirrors the fresh status snapshot the IPC
   * call returns onto local state so the checkbox / chip / state
   * line all converge in a single round trip — no follow-up
   * `kchat:status` poll required.
   */
  const onToggleKChatEnabled = useCallback(async (next: boolean) => {
    setKchatTogglePending(true);
    setKchatToggleError(null);
    try {
      const s = await aec.kchat.setEnabled({ enabled: next });
      setKchatStatus({
        enabled: s.enabled,
        state: s.state,
        publisherKind: s.publisherKind,
        instance: parseLoopbackInstance(s.instanceJson),
      });
    } catch (e) {
      // The bridge can only fail here on a panic in the
      // `RwLock` or a serialization error — neither is
      // user-actionable, but surfacing the message keeps the
      // user from thinking the checkbox is silently broken.
      setKchatToggleError(e instanceof Error ? e.message : String(e));
    } finally {
      setKchatTogglePending(false);
    }
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

  // Phase 17 Task 8 — Theme. The boot script in `main.tsx` already
  // applied the stored mode to `<html>` synchronously, so the
  // initial state read is consistent with what the user sees.
  const [themeMode, setThemeMode] = useState<ThemeMode>(() =>
    readStoredThemeMode(),
  );
  // Re-resolve "system" when the OS toggles dark/light. We keep a
  // counter rather than the resolved value directly so the
  // displayed label updates *after* the new value is in effect
  // (paint-after-commit, not commit-during-render).
  const [, setOsHintTick] = useState(0);
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => {
      if (themeMode === "system") setOsHintTick((t) => t + 1);
    };
    // `addEventListener` is the modern path; older Safari needs
    // `addListener`. Try the modern one and fall back so this works
    // in all environments AEC Studio supports.
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", handler);
      return () => mql.removeEventListener("change", handler);
    }
    if (typeof mql.addListener === "function") {
      mql.addListener(handler);
      return () => mql.removeListener(handler);
    }
    return undefined;
  }, [themeMode]);
  const setTheme = useCallback((mode: ThemeMode) => {
    applyThemeMode(mode);
    writeStoredThemeMode(mode);
    setThemeMode(mode);
    // Theme toggles are immediate — the Save button is for the
    // other settings on this page. Don't reset savedAt.
  }, []);
  // Computed on every render. `resolveEffectiveTheme` is a string
  // compare plus a `matchMedia` read in System mode — microseconds —
  // so the memo overhead is a net loss. Memoising on `[themeMode]`
  // alone would also be wrong: when the OS flips under System mode
  // the dep array is unchanged, so the memo would hand back the
  // cached resolution against the previous OS preference.
  const effectiveTheme = resolveEffectiveTheme(themeMode);

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
        data-testid="settings-section-appearance"
        aria-label="Appearance"
      >
        <h2>Appearance</h2>
        <fieldset>
          <legend>Theme</legend>
          <label>
            <input
              type="radio"
              name="theme"
              value="system"
              data-testid="settings-theme-system"
              checked={themeMode === "system"}
              onChange={() => setTheme("system")}
            />
            System ({effectiveTheme === "dark" ? "dark" : "light"} right now)
          </label>
          <label>
            <input
              type="radio"
              name="theme"
              value="light"
              data-testid="settings-theme-light"
              checked={themeMode === "light"}
              onChange={() => setTheme("light")}
            />
            Light
          </label>
          <label>
            <input
              type="radio"
              name="theme"
              value="dark"
              data-testid="settings-theme-dark"
              checked={themeMode === "dark"}
              onChange={() => setTheme("dark")}
            />
            Dark
          </label>
        </fieldset>
        <p className="settings-page__subtitle">
          Changes apply immediately and persist across sessions.
        </p>
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

      {extensionDiagnostics !== null && extensionDiagnostics.length > 0 && (
        <section
          className="settings-section settings-section--extension-diagnostics"
          data-testid="settings-section-extension-diagnostics"
          aria-label="Extension load diagnostics"
          role="region"
        >
          <h2>Extension load diagnostics</h2>
          <p>
            The following installed extensions failed to load during
            this AEC Studio session. The remaining extensions are
            unaffected — AEC Studio remains fully functional — but
            the assets, templates, schedules, and AI tools shipped
            by the listed extensions are unavailable until the
            errors are resolved. Restart AEC Studio after fixing
            the underlying files to retry loading.
          </p>
          <ul
            className="settings-extension-diagnostics"
            data-testid="settings-extension-diagnostics-list"
          >
            {extensionDiagnostics.map((d, i) => (
              <li
                key={`${d.path}__${d.stage}__${i}`}
                data-testid="settings-extension-diagnostic-item"
                data-stage={d.stage}
                data-extension-id={d.extensionId ?? ""}
              >
                <div className="settings-extension-diagnostics__heading">
                  <strong>{d.extensionId ?? "(unknown extension)"}</strong>{" "}
                  <span className="settings-extension-diagnostics__stage">
                    {extensionStageLabel(d.stage)}
                  </span>
                </div>
                <div className="settings-extension-diagnostics__path">
                  <code>{d.path}</code>
                </div>
                <div className="settings-extension-diagnostics__message">
                  {d.message}
                </div>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section
        className="settings-section"
        data-testid="settings-section-kchat"
        aria-label="KChat integration"
      >
        <h2>KChat integration</h2>
        <p>
          Optional one-way publishing of artefacts (renders, sheets,
          packs) to a KChat thread, with inline review comments
          flowing back into the audit trail. AEC Studio remains
          fully functional without it. The toggle below is
          bridge-persisted onto the active project's
          <code> settings.kchat.enabled </code> manifest field via
          <code> kchat:setEnabled </code> and gates the
          <code> kchat:publish </code> IPC handler in the Electron
          main process — disabling it stops publishes at the
          source, not just in the UI.
        </p>
        <label>
          <input
            type="checkbox"
            data-testid="settings-kchat-enabled"
            checked={kchatStatus?.enabled ?? false}
            disabled={kchatStatus === null || kchatTogglePending}
            onChange={(e) => void onToggleKChatEnabled(e.target.checked)}
          />
          Enable KChat integration
          {kchatTogglePending && (
            <span
              data-testid="settings-kchat-enabled-pending"
              className="settings-page__inline-pending"
              aria-live="polite"
            >
              {" "}
              (saving…)
            </span>
          )}
        </label>
        {kchatToggleError && (
          <p
            data-testid="settings-kchat-enabled-error"
            className="settings-page__inline-error"
            role="alert"
          >
            Toggle failed: {kchatToggleError}
          </p>
        )}
        <div
          className="settings-kchat-instance"
          data-testid="settings-kchat-instance"
        >
          {kchatStatus === null ? (
            <p>Checking for KChat Desktop&hellip;</p>
          ) : kchatStatus.instance ? (
            <ul>
              <li>
                Publisher: <code>{kchatStatus.publisherKind}</code>
              </li>
              <li>
                Connection: <code>{kchatStatus.state}</code>
              </li>
              <li>
                Loopback API:{" "}
                <code>
                  {kchatStatus.instance.apiServerRunning
                    ? `127.0.0.1:${kchatStatus.instance.apiServerPort ?? "?"}`
                    : "not running"}
                </code>
              </li>
              <li>
                Port file:{" "}
                <code>{kchatStatus.instance.portFilePath ?? "\u2014"}</code>
              </li>
              <li>
                Extension heartbeat:{" "}
                <code>
                  {kchatStatus.instance.lastExtensionContactAt ??
                    "never (not yet seen this session)"}
                </code>
              </li>
              <li>
                Publish queue:{" "}
                <code>{kchatStatus.instance.queuedPublishCount} card(s)</code>
              </li>
              <li>
                Review threads:{" "}
                <code>{kchatStatus.instance.reviewThreadCount}</code>
              </li>
            </ul>
          ) : (
            <p>
              The loopback API is not running on this process. Install
              the AEC Studio companion extension inside KChat Desktop
              and restart AEC Studio to enable publishing.
            </p>
          )}
          <button
            type="button"
            data-testid="settings-kchat-reload"
            disabled={kchatReloading}
            onClick={() => void onReloadKChat()}
          >
            {kchatReloading ? "Reloading…" : "Reload KChat connection"}
          </button>
          {kchatReloadError && (
            <p
              data-testid="settings-kchat-reload-error"
              className="settings-page__inline-error"
              role="alert"
            >
              Reload failed: {kchatReloadError}
            </p>
          )}
        </div>
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
