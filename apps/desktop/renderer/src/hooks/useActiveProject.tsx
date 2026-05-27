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
  useMemo,
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
  // every site that mutates dirtiness MUST go through `updateDirty`
  // below (which assigns both the state and the ref atomically) to
  // keep the two in lockstep. Direct `setDirty(...)` calls are
  // forbidden — Devin Review flagged the prior "pair of writes"
  // pattern as a discipline-only invariant where any future
  // contributor that adds a `setDirty(true)` without the matching
  // `dirtyRef.current = true` would silently break the re-arm guard
  // in the open/create/close catch blocks (failed transitions would
  // no longer rearm the timer because the ref read would lag).
  // Wrapping the pair in one helper makes the invariant
  // structurally unfakeable instead of a comment-enforced contract.
  const dirtyRef = useRef(false);

  // Atomic dirty mutator. The single entry point for changing
  // `dirty` state — assigns the React state and the synchronous ref
  // mirror in one place so the two cannot drift. Stable identity
  // (empty dep array) so callers can list it in their own dep
  // arrays without triggering refresh loops.
  //
  // Why a setter rather than functional-update style: every current
  // caller already knows the absolute value it wants (`true` from
  // `markDirty`, `false` from successful open/create/save/close /
  // `markClean`). A functional API would invite stale-state bugs
  // where callers compute the next value from a possibly-stale
  // `dirty` closure. Forcing the absolute value keeps each site's
  // intent explicit and grep-able.
  const updateDirty = useCallback((next: boolean) => {
    setDirty(next);
    dirtyRef.current = next;
  }, []);

  // Mirror `project?.path` into a ref so the save IIFE can detect
  // whether the user transitioned to a different project (or closed
  // the project entirely) while the save's bridge call was in flight.
  //
  // Without this guard, the following race silently corrupts renderer
  // state: user has project A open and dirty → 5s auto-save fires →
  // `aec.project.save(A)` dispatched and awaiting disk → user clicks
  // "Open Project B" → `openProject(B)` sets the active slot to B's
  // summary and clears dirty → A's save resolves → IIFE unconditionally
  // calls `setProject(summaryA)`, reverting the renderer to A while
  // the StatusBar header and every mode page that already re-rendered
  // against B is now addressing the wrong project on its next bridge
  // call. The window is small (one disk fsync) but not zero, and grows
  // with bigger projects, encrypted volumes, and any future bridge
  // checkpoint path. Every site that mutates the active project MUST
  // go through `updateProject` below (which assigns both the state
  // and the ref atomically) to keep the two in lockstep.
  const projectPathRef = useRef<string | null>(null);

  // Atomic project mutator. Single entry point for changing the
  // active project — assigns React state and the synchronous ref
  // mirror in one place so the two cannot drift. Same rationale and
  // pattern as `updateDirty`. Stable identity so callers can list it
  // in their dep arrays without triggering refresh loops.
  const updateProject = useCallback((next: ProjectSummary | null) => {
    setProject(next);
    projectPathRef.current = next?.path ?? null;
  }, []);

  // Mirror `saveProject` into a ref so the stable `armAutoSave`
  // helper can dispatch through the latest closure without taking
  // `saveProject` as a dep (which itself depends on `project` and so
  // changes on every project transition). The effect-based sync is
  // safe here because the ref is only read from `setTimeout`
  // callbacks that fire well after the current commit.
  const saveProjectRef = useRef<() => Promise<void>>(async () => {});

  // Coalesce concurrent `saveProject` invocations. Without this slot,
  // two architectural hazards exist when a save takes longer than the
  // 5s auto-save debounce window (slow disks, large projects,
  // encrypted volumes, or any future bridge-side flush-and-checkpoint
  // path that grows past 5s):
  //
  //   1. The 5s timer armed by `markDirty()` fires DURING an
  //      in-flight manual save and dispatches a second
  //      `aec.project.save(project.path)` call. The bridge layer is
  //      idempotent (SQLCipher WAL + atomic file write), so the data
  //      is safe — but the StatusBar flickers Saving→Saved→Saving→
  //      Saved for one logical save and the redundant call burns
  //      real disk I/O and battery on laptops.
  //   2. The user spams Ctrl+S during a slow save ("is it stuck?")
  //      and triggers N parallel bridge calls. Same I/O burn plus
  //      log spam and any future telemetry counter inflation.
  //
  // Returning the in-flight promise from subsequent invocations means
  // every caller awaits the same resolution, the StatusBar shows one
  // logical save, and the bridge sees exactly one `project.save` call
  // per logical save event. The ref is cleared in the inflight's
  // `finally` so the next save event starts fresh.
  //
  // The finally also performs an *equality* check on the ref before
  // clearing it: project-transition callbacks (open/create/close)
  // clear the slot explicitly so the new project's saves never
  // coalesce against a stale promise that belongs to the previous
  // project. Without the equality guard, this finally would race
  // and clobber a freshly-armed inflight for the new project,
  // leaving the next saveProject invocation to dispatch a duplicate
  // bridge call (defeating coalescing for the new project).
  const savingPromiseRef = useRef<Promise<void> | null>(null);

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
      updateProject(result.summary as ProjectSummary | null);
    } catch {
      updateProject(null);
    } finally {
      setLoading(false);
    }
  }, [updateProject]);

  useEffect(() => {
    void refreshProject();
  }, [refreshProject]);

  // Subscribe to the main-process push channel for active-project
  // changes (`aec.project.onActiveProjectChange`). The main-process
  // `active-project.ts` tracker fires `notify()` synchronously from
  // every `setActive*` / `clear*` call site (open / create / save /
  // close); main.ts forwards each notification to every renderer
  // window via `webContents.send("project:active-changed", summary)`.
  //
  // Today there is a single renderer window so this listener is a
  // defensive no-op for any push that originated from this window's
  // own `openProject` / `createProject` / `saveProject` /
  // `closeProject` (those callbacks update local state before the
  // push round-trips back; `updateProject` with the same payload is
  // idempotent because the path-ref equality check below skips
  // redundant state writes — and even if it didn't, React's
  // `setState` would bail on reference equality of the spread
  // summary, which it doesn't, so the path-ref guard is the only
  // thing preventing a wasted render per save).
  //
  // The push channel becomes load-bearing the moment a second
  // window is added (e.g. "Open project in new window", Print
  // Preview, pop-out viewport) or a test harness mutates the tracker
  // directly — every window stays in lockstep on the active project
  // without polling or relying on the originator's manual sync.
  //
  // We only update the `project` summary on push, NOT the renderer-
  // only fields (`dirty`, `saving`, `undoLen`, `redoLen`). Those are
  // window-local — a different window with its own undo stack and
  // its own dirty flag is allowed to have those diverge even if the
  // underlying project file is the same.
  useEffect(() => {
    const unsubscribe = aec.project.onActiveProjectChange((summary) => {
      // Skip the redundant write when the push reflects state we
      // already have. `projectPathRef` is the synchronous mirror of
      // `project?.path`, so this comparison sees the latest value
      // even within the same event tick — no risk of a stale state
      // read making us drop a real update.
      const incomingPath = summary?.path ?? null;
      if (incomingPath === projectPathRef.current) {
        return;
      }
      updateProject(summary as ProjectSummary | null);
    });
    return unsubscribe;
  }, [updateProject]);

  const openProject = useCallback(
    async (path: string) => {
      // Cancel BEFORE the bridge call to prevent the stale-timer race
      // documented on `cancelPendingAutoSave`. If the bridge call
      // succeeds, `updateDirty(false)` below clears the dirty flag and
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
      // Clear any in-flight save's coalescing slot. A save for the
      // previous project that is still on the wire belongs to a
      // project the user is abandoning; the IIFE's setProject is
      // guarded by `projectPathRef` so its state commit is dropped,
      // but the slot itself must be cleared here so the freshly
      // opened project can dispatch its own first save without
      // coalescing against the old project's promise (which would
      // return resolved as soon as the old save finishes, falsely
      // telling B's caller "your save is done" when B was never
      // actually saved). The finally inside the old IIFE uses an
      // equality check before re-clearing the slot, so a new
      // inflight armed by B is not accidentally clobbered when A's
      // promise eventually settles.
      savingPromiseRef.current = null;
      try {
        const summary = (await aec.project.open(path)) as ProjectSummary;
        updateProject(summary);
        updateDirty(false);
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
    [cancelPendingAutoSave, armAutoSave, updateDirty, updateProject],
  );

  const createProject = useCallback(
    async (templateKey: string, name: string) => {
      // Same cancel-before-await + catch-rearm + clear-saving-slot
      // pattern as `openProject` — see that callback for the full
      // rationale on each step.
      cancelPendingAutoSave();
      savingPromiseRef.current = null;
      setLoading(true);
      try {
        const summary = (await aec.project.createFromTemplate(
          templateKey,
          name,
        )) as ProjectSummary;
        updateProject(summary);
        updateDirty(false);
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
    [cancelPendingAutoSave, armAutoSave, updateDirty, updateProject],
  );

  const closeProject = useCallback(async () => {
    // Same cancel-before-await + catch-rearm pattern as
    // `openProject` / `createProject`. Today the main-process
    // `project:close` handler synchronously calls
    // `clearActiveProjectPath()` and cannot throw — so the catch is
    // unreachable in the current build. But the handler signature is
    // `async`, and any future enhancement (flush-pending-changes
    // before close, user-confirmation prompt with await-roundtrip,
    // telemetry submit before clearing the slot, integrity-check on
    // the project file before release) introduces a real failure
    // surface. Without the catch, the bridge would reject, the
    // `setProject(null)` / dirty-clear below would never run, and the
    // *current* project would remain active with `dirty === true`
    // while its auto-save timer is permanently cancelled — identical
    // data-loss window to the one documented on `openProject`. The
    // re-throw lets callers (App `close` handler) surface the failure
    // as an error toast while still keeping the user on a stable
    // route. `armAutoSave` is stable (empty deps via
    // `saveProjectRef`), so adding it here does not unstabilize
    // `closeProject`'s identity across project transitions.
    cancelPendingAutoSave();
    // Clear the inflight save slot for the same reason as
    // `openProject` / `createProject` — see those callbacks. The
    // user is leaving the active project; any in-flight save belongs
    // to a project they no longer have open, and a subsequent
    // openProject must not coalesce against it.
    savingPromiseRef.current = null;
    try {
      await aec.project.close();
      updateProject(null);
      updateDirty(false);
      setUndoLen(0);
      setRedoLen(0);
    } catch (err) {
      if (dirtyRef.current) {
        armAutoSave();
      }
      throw err;
    }
  }, [cancelPendingAutoSave, armAutoSave, updateDirty, updateProject]);

  const saveProject = useCallback(async () => {
    // Read the active project's path through `projectPathRef` rather
    // than the captured `project` state. The ref is updated
    // synchronously alongside every `setProject` site via
    // `updateProject`, so this read always sees the latest value.
    // Threading the path through the ref — instead of taking
    // `project` as a `useCallback` dep — keeps `saveProject`'s
    // identity stable across saves: `updateProject(summary)` runs on
    // every successful save (to surface the refreshed `modifiedAt`
    // timestamp), which creates a fresh `project` object reference
    // and would otherwise re-derive a new `saveProject` closure on
    // every save. That cascade churned the `useMemo` context `value`
    // identity in `ActiveProjectProvider` on every save tick,
    // re-rendering every consumer that depends on the context even
    // when their used fields hadn't changed. The same logic applies
    // to `setActiveProject` push notifications from the main process
    // — they replace the summary object even when the path is
    // unchanged, so threading the path through the ref decouples
    // identity from notification cadence too.
    const savePath = projectPathRef.current;
    if (savePath === null) return;
    // Coalesce: if a save is already in flight, return its promise so
    // every caller awaits the same resolution. See `savingPromiseRef`
    // for the full rationale (handles both the auto-save-fires-during-
    // manual-save race when saves take >5s, and the Ctrl+S spam case).
    if (savingPromiseRef.current !== null) {
      return savingPromiseRef.current;
    }
    // Cancel any pending auto-save BEFORE the bridge call. A manual
    // save satisfies the auto-save's contract — the user's changes
    // will reach disk well before the 5s debounce would have fired,
    // so leaving the timer armed only creates a flicker hazard. The
    // counterpart cancel in the success path below covers a different
    // race (a `markDirty` arriving *during* the save's await window);
    // together they ensure no timer slot ever survives a save's
    // lifetime. This cancel cannot be moved into the inflight IIFE
    // because the IIFE is async — by the time it runs, the queued
    // timer may have already fired and dispatched a second save.
    cancelPendingAutoSave();
    setSaving(true);
    // `inflight` is referenced from inside its own IIFE so the
    // finally can compare against `savingPromiseRef.current` without
    // racing a fresh inflight that a project-transition callback has
    // since installed in the slot. `let` + null-init keeps TypeScript
    // happy with the self-reference; the IIFE body cannot read
    // `inflight` until after the await resolves, by which time the
    // assignment below has completed.
    let inflight: Promise<void> | null = null;
    inflight = (async () => {
      try {
        const summary = (await aec.project.save(savePath)) as ProjectSummary;
        // Guard: only commit the save's result to React state if the
        // user is still on the same project. If a project transition
        // (open/create/close) ran while this save was in flight,
        // `projectPathRef.current` no longer matches `savePath`;
        // applying `setProject(summary)` here would silently revert
        // the renderer to the old project while every mode page is
        // already rendering against the new one. The bridge call
        // itself is not wasted — the old project's bytes have safely
        // landed on disk, which is the only durable contract a save
        // promises.
        if (projectPathRef.current === savePath) {
          updateProject(summary);
          updateDirty(false);
          // Cancel again: a `markDirty` that arrived DURING the save's
          // await would have re-armed the timer. The fresh save
          // already includes whatever was in the bridge at write-time
          // (the bridge holds its own write lock, so the
          // post-mutation state is what landed on disk); we clear the
          // slot here so the next mutation arms a single fresh 5s
          // debounce instead of racing a stale armed timer against
          // the just-completed save.
          cancelPendingAutoSave();
        }
      } finally {
        // `dirty` stays true on failure so the next mutation (or a
        // caller's manual retry) re-arms a save. The catch in the
        // caller decides whether to surface the failure as a toast
        // (manual save) or swallow it (auto-save). The `inflight`
        // promise is cleared here so the next save event starts from
        // a clean coalescing slot — without this, a failed save
        // would permanently lock out future `saveProject` calls.
        //
        // Equality-guard the clear: if a project transition cleared
        // the slot and a fresh save was dispatched for the new
        // project, that fresh inflight is now in the slot. Clearing
        // unconditionally would lose the new inflight and break
        // coalescing for the new project. Only clear when the slot
        // still holds *this* IIFE's promise.
        setSaving(false);
        if (savingPromiseRef.current === inflight) {
          savingPromiseRef.current = null;
        }
      }
    })();
    savingPromiseRef.current = inflight;
    return inflight;
  }, [cancelPendingAutoSave, updateDirty, updateProject]);

  // Keep the `saveProject` ref pointed at the latest closure so the
  // stable `armAutoSave` helper dispatches through the current
  // project's save logic without taking `saveProject` as a dep.
  useEffect(() => {
    saveProjectRef.current = saveProject;
  }, [saveProject]);

  const markDirty = useCallback(() => {
    updateDirty(true);
    armAutoSave();
  }, [armAutoSave, updateDirty]);

  const markClean = useCallback(() => {
    updateDirty(false);
    cancelPendingAutoSave();
  }, [cancelPendingAutoSave, updateDirty]);

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

  // Memoise the context value so consumers don't tear down their
  // subtrees on every provider render. Without this, *every*
  // re-render of `ActiveProjectProvider` — including ones driven
  // by orthogonal state like `dirty`, `saving`, `undoLen` ticking —
  // would create a fresh `value` object identity. Every component
  // that calls `useActiveProject()` would then re-render even if it
  // only reads, say, `project.name`. Today this is benign because
  // there are no expensive consumers, but it becomes a real perf
  // footgun the moment a consumer renders an expensive subtree
  // (3D viewport overlay, large schedule table, etc.).
  //
  // The dep list enumerates every value (state + every callback)
  // that the object exposes; each callback is itself `useCallback`-
  // wrapped above so its identity is stable across renders where
  // its own deps haven't changed. The end result: `value` keeps
  // referential equality across renders that don't actually change
  // any of the exposed state, satisfying React's `Object.is`-based
  // context-propagation bail-out.
  const value = useMemo<ActiveProjectState>(
    () => ({
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
    }),
    [
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
    ],
  );

  return (
    <ActiveProjectContext.Provider value={value}>
      {children}
    </ActiveProjectContext.Provider>
  );
}
