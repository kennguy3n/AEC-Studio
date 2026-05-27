/**
 * Active-project React context and hook.
 *
 * Every mode page (Design, Draft, BIM, Render, Deliver) routes bridge
 * calls through the active project. Until Phase 13 the BIM page
 * hard-coded `"demo://project.ifc"` and the other pages either
 * omitted the project path (relying on the main-process tracker) or
 * used placeholder paths. This context surfaces the active project
 * summary on the renderer side so every page can:
 *
 *   * Read `project.path` for `commandApply`, `bimImportIfc`, etc.
 *   * Read `project.name` for header / breadcrumb / toast messages.
 *   * Call `openProject(path)` to open a project from the Home page.
 *   * Call `createProject(template, name)` to create from a template.
 *   * Call `closeProject()` to return to the Home screen.
 *   * Check `project === null` to render the route guard.
 *
 * The context polls `project:current` on mount and refreshes on
 * open / create / save / close / command-apply so any page that
 * consumes the hook always sees the latest project state.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";
import { aec, ProjectSummary } from "../api/aec";

export interface ActiveProjectState {
  project: ProjectSummary | null;
  loading: boolean;
  dirty: boolean;
  saving: boolean;
  undoLen: number;
  redoLen: number;
  openProject: (path: string) => Promise<void>;
  createProject: (templateKey: string, name: string) => Promise<void>;
  closeProject: () => Promise<void>;
  saveProject: () => Promise<void>;
  refreshProject: () => Promise<void>;
  markDirty: () => void;
  markClean: () => void;
  setUndoRedo: (undoLen: number, redoLen: number) => void;
}

const ActiveProjectContext = createContext<ActiveProjectState | null>(null);

export function useActiveProject(): ActiveProjectState {
  const ctx = useContext(ActiveProjectContext);
  if (ctx === null) {
    throw new Error(
      "useActiveProject must be used inside an <ActiveProjectProvider>",
    );
  }
  return ctx;
}

export function ActiveProjectProvider({
  children,
}: {
  children: ReactNode;
}) {
  const [project, setProject] = useState<ProjectSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [undoLen, setUndoLen] = useState(0);
  const [redoLen, setRedoLen] = useState(0);
  const autoSaveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Mirror `dirty` into a ref so stable callbacks (`armAutoSave`,
  // open/create catch blocks) can read the latest value without
  // listing `dirty` in their dep arrays — which would otherwise
  // unstabilize their identity on every keystroke and break consumer
  // `useEffect` deps (e.g., tests that read `openProject` as a dep).
  // State commits and useEffect ref-syncs run *after* the current
  // event handler completes, so updating the ref via `useEffect`
  // alone would leave it one tick stale during the same event tick;
  // every site that calls `setDirty(...)` therefore also assigns
  // `dirtyRef.current` synchronously to keep the two in lockstep.
  const dirtyRef = useRef(false);

  // Mirror `saveProject` into a ref so the stable `armAutoSave`
  // helper can dispatch through the latest closure without taking
  // `saveProject` as a dep (which itself depends on `project` and so
  // changes on every project transition). The effect-based sync is
  // safe here because the ref is only read from `setTimeout`
  // callbacks that fire well after the current commit.
  const saveProjectRef = useRef<() => Promise<void>>(async () => {});

  // Cancel any pending auto-save before transitioning the active
  // project (open / create / close). Without this, a debounced timer
  // armed by `markDirty()` for project A can fire *after* the user has
  // already opened project B; the timer's captured `saveProject`
  // closure references project A, so `aec.project.save(A.path)` would
  // run, its returned summary would `setProject(summaryA)`, and the
  // main-process `project:save` handler would call
  // `setActiveProject(summaryA)` — silently re-binding the active
  // project to A while the renderer header still says "B". Every
  // subsequent `draft:*` / `deliver:*` / `command:*` call would then
  // address the wrong project. Centralizing the cancel in one helper
  // ensures future project-transition branches (e.g., "switch to
  // recent") inherit the guarantee without re-deriving it.
  const cancelPendingAutoSave = useCallback(() => {
    if (autoSaveTimerRef.current !== null) {
      clearTimeout(autoSaveTimerRef.current);
      autoSaveTimerRef.current = null;
    }
  }, []);

  // Arm (or re-arm) the 5s debounced auto-save timer. Used by
  // `markDirty` on every mutation and by the open/create catch
  // blocks to restore auto-save protection when a project transition
  // fails mid-flight. Kept stable (empty dep array) by dispatching
  // through `saveProjectRef` rather than capturing `saveProject`
  // directly — this preserves the identity of every callback that
  // depends on `armAutoSave` (including `openProject`/`createProject`),
  // so consumer `useEffect`s that list them don't refire on every
  // dirty-flag flip.
  const armAutoSave = useCallback(() => {
    if (autoSaveTimerRef.current !== null) {
      clearTimeout(autoSaveTimerRef.current);
    }
    autoSaveTimerRef.current = setTimeout(() => {
      autoSaveTimerRef.current = null;
      // Auto-save is fire-and-forget: a transient failure (network
      // blip, disk pressure) shouldn't crash the timer or trigger an
      // error toast — the next mutation will re-arm and retry.
      saveProjectRef.current().catch(() => {
        // Intentional swallow: dirty flag stays set so the next
        // `markDirty` debounce will retry.
      });
    }, 5_000);
  }, []);

  const refreshProject = useCallback(async () => {
    try {
      const result = await aec.project.current();
      setProject(result.summary as ProjectSummary | null);
    } catch {
      setProject(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshProject();
  }, [refreshProject]);

  const openProject = useCallback(
    async (path: string) => {
      // Cancel BEFORE the bridge call to prevent the stale-timer race
      // documented on `cancelPendingAutoSave`. If the bridge call
      // succeeds, `setDirty(false)` below clears the dirty flag and
      // the cancel was correct (no auto-save needed for the freshly
      // opened project, which starts clean). If the bridge call
      // throws (corrupt file, permission denied, disk full, etc.),
      // the *current* project remains active with `dirty === true`
      // but its auto-save timer is already cancelled — without the
      // catch below, pending changes would only persist on the next
      // user mutation (which may never come if they walk away),
      // creating a data-loss window equal to the dwell time before
      // the open failure surfaced.
      cancelPendingAutoSave();
      setLoading(true);
      try {
        const summary = (await aec.project.open(path)) as ProjectSummary;
        setProject(summary);
        setDirty(false);
        dirtyRef.current = false;
        setUndoLen(0);
        setRedoLen(0);
      } catch (err) {
        // Re-arm the timer so the still-active project retains its
        // auto-save protection. Guarded by `dirtyRef` so a failed
        // open from a clean state doesn't spuriously schedule a
        // save-while-clean call (which would no-op via the `project
        // === null` / clean-flag check inside `saveProject`, but the
        // extra timer churn is wasteful). Re-throw so callers can
        // surface the failure (toast, route stay-put, etc.) — the
        // hook deliberately doesn't toast itself to keep the
        // ToastProvider an optional consumer-side concern.
        if (dirtyRef.current) {
          armAutoSave();
        }
        throw err;
      } finally {
        setLoading(false);
      }
    },
    [cancelPendingAutoSave, armAutoSave],
  );

  const createProject = useCallback(
    async (templateKey: string, name: string) => {
      // Same cancel-before-await + catch-rearm pattern as
      // `openProject` — see that callback for the full rationale.
      cancelPendingAutoSave();
      setLoading(true);
      try {
        const summary = (await aec.project.createFromTemplate(
          templateKey,
          name,
        )) as ProjectSummary;
        setProject(summary);
        setDirty(false);
        dirtyRef.current = false;
        setUndoLen(0);
        setRedoLen(0);
      } catch (err) {
        if (dirtyRef.current) {
          armAutoSave();
        }
        throw err;
      } finally {
        setLoading(false);
      }
    },
    [cancelPendingAutoSave, armAutoSave],
  );

  const closeProject = useCallback(async () => {
    cancelPendingAutoSave();
    await aec.project.close();
    setProject(null);
    setDirty(false);
    dirtyRef.current = false;
    setUndoLen(0);
    setRedoLen(0);
  }, [cancelPendingAutoSave]);

  const saveProject = useCallback(async () => {
    if (project === null) return;
    setSaving(true);
    try {
      const summary = (await aec.project.save(project.path)) as ProjectSummary;
      setProject(summary);
      setDirty(false);
      dirtyRef.current = false;
      // Cancel any pending auto-save timer on the success path. A
      // manual `Ctrl+S` (or any other caller invoking `saveProject`
      // directly) immediately after a mutation would otherwise leave
      // the 5s timer armed from `markDirty()` — that timer would then
      // fire, call `saveProject` again, and flash the StatusBar
      // "Saved" → "Saving…" → "Saved" for no work. The cancel must
      // live here (next to the `setDirty(false)`) and not at the
      // App-level save handler, because *every* successful save —
      // including the auto-save timer's own invocation — must clear
      // the slot so a subsequent re-arming cannot pile up against a
      // stale pending timer. Calling `cancelPendingAutoSave()` is a
      // no-op when the slot is already empty (the auto-save path
      // nulls the ref before invoking `saveProject`), so there's no
      // double-clear hazard.
      cancelPendingAutoSave();
    } finally {
      // `dirty` stays true on failure so the auto-save timer (or the
      // user's next mutation) re-arms a retry without explicit reset
      // logic here; the catch in the caller decides whether to surface
      // the failure as a toast (manual save) or swallow it (auto-save).
      setSaving(false);
    }
  }, [project, cancelPendingAutoSave]);

  // Keep the `saveProject` ref pointed at the latest closure so the
  // stable `armAutoSave` helper dispatches through the current
  // project's save logic without taking `saveProject` as a dep.
  useEffect(() => {
    saveProjectRef.current = saveProject;
  }, [saveProject]);

  const markDirty = useCallback(() => {
    setDirty(true);
    dirtyRef.current = true;
    armAutoSave();
  }, [armAutoSave]);

  const markClean = useCallback(() => {
    setDirty(false);
    dirtyRef.current = false;
    cancelPendingAutoSave();
  }, [cancelPendingAutoSave]);

  const setUndoRedo = useCallback(
    (undo: number, redo: number) => {
      setUndoLen(undo);
      setRedoLen(redo);
    },
    [],
  );

  // Clean up auto-save timer on unmount.
  useEffect(() => {
    return () => {
      if (autoSaveTimerRef.current !== null) {
        clearTimeout(autoSaveTimerRef.current);
      }
    };
  }, []);

  const value: ActiveProjectState = {
    project,
    loading,
    dirty,
    saving,
    undoLen,
    redoLen,
    openProject,
    createProject,
    closeProject,
    saveProject,
    refreshProject,
    markDirty,
    markClean,
    setUndoRedo,
  };

  return (
    <ActiveProjectContext.Provider value={value}>
      {children}
    </ActiveProjectContext.Provider>
  );
}
