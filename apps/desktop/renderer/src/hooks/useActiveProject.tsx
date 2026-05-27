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
  const [undoLen, setUndoLen] = useState(0);
  const [redoLen, setRedoLen] = useState(0);
  const autoSaveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
      setLoading(true);
      try {
        const summary = (await aec.project.open(path)) as ProjectSummary;
        setProject(summary);
        setDirty(false);
        setUndoLen(0);
        setRedoLen(0);
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const createProject = useCallback(
    async (templateKey: string, name: string) => {
      setLoading(true);
      try {
        const summary = (await aec.project.createFromTemplate(
          templateKey,
          name,
        )) as ProjectSummary;
        setProject(summary);
        setDirty(false);
        setUndoLen(0);
        setRedoLen(0);
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const closeProject = useCallback(async () => {
    if (autoSaveTimerRef.current !== null) {
      clearTimeout(autoSaveTimerRef.current);
      autoSaveTimerRef.current = null;
    }
    await aec.project.close();
    setProject(null);
    setDirty(false);
    setUndoLen(0);
    setRedoLen(0);
  }, []);

  const saveProject = useCallback(async () => {
    if (project === null) return;
    try {
      const summary = (await aec.project.save(project.path)) as ProjectSummary;
      setProject(summary);
      setDirty(false);
    } catch {
      // Save failed — keep dirty flag so auto-save retries.
    }
  }, [project]);

  const markDirty = useCallback(() => {
    setDirty(true);
    // Debounced auto-save: 5 seconds after the last mutation.
    if (autoSaveTimerRef.current !== null) {
      clearTimeout(autoSaveTimerRef.current);
    }
    autoSaveTimerRef.current = setTimeout(() => {
      autoSaveTimerRef.current = null;
      void saveProject();
    }, 5_000);
  }, [saveProject]);

  const markClean = useCallback(() => {
    setDirty(false);
    if (autoSaveTimerRef.current !== null) {
      clearTimeout(autoSaveTimerRef.current);
      autoSaveTimerRef.current = null;
    }
  }, []);

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
