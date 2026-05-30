import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { aec, ProjectSummary, RuntimeStatus } from "../api/aec";
import { ProjectCard } from "../components/ProjectCard";
import { TemplateCard, DEFAULT_TEMPLATES, TemplateChoice } from "../components/TemplateCard";
import { HardwareProfileCard } from "../components/HardwareProfileCard";
import {
  OnboardingModal,
  readOnboardingDismissed,
  writeOnboardingDismissed,
} from "../components/OnboardingModal";
import { useActiveProject } from "../hooks/useActiveProject";
import { useToast } from "../hooks/useToast";

export function Home() {
  const navigate = useNavigate();
  const { openProject, createProject } = useActiveProject();
  const { addToast } = useToast();
  const [recents, setRecents] = useState<ProjectSummary[]>([]);
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  // Phase 17 Group C Task 19 — first-run onboarding modal.
  // `dismissed` is hydrated from `localStorage` on mount so the
  // modal stays hidden after the user has dismissed it once.
  // `recents.length === 0` is checked at render time; the modal
  // disappears the moment the user opens or creates their first
  // project.
  const [onboardingDismissed, setOnboardingDismissed] = useState<boolean>(
    () => readOnboardingDismissed(),
  );

  useEffect(() => {
    let alive = true;
    void aec.project.listRecents().then((r) => {
      if (alive) setRecents(r as ProjectSummary[]);
    });
    void aec.runtime.status().then((s) => {
      if (alive) setStatus(s as RuntimeStatus);
    });
    return () => {
      alive = false;
    };
  }, []);

  const dismissOnboarding = () => {
    writeOnboardingDismissed();
    setOnboardingDismissed(true);
  };
  const startEmptyFromOnboarding = () => {
    // Persist before navigating so the next mount of Home doesn't
    // surface the modal again. We still let the create flow's own
    // toast surface failures.
    writeOnboardingDismissed();
    setOnboardingDismissed(true);
    // The canonical empty template key is `general.empty` — wired
    // to `templates/general/empty.json` and surfaced by
    // `DEFAULT_TEMPLATES[0]` in `TemplateCard.tsx`. An earlier
    // revision looked up the literal `"empty"` key, which never
    // matched any template; the `.find()` returned `undefined` and
    // the `?? DEFAULT_TEMPLATES[0]` fallback silently created the
    // first listed template (an Apartment) instead. Falling back
    // here would mask future template-list churn the same way, so
    // surface the misconfiguration as an error toast instead.
    const empty = DEFAULT_TEMPLATES.find((t) => t.key === "general.empty");
    if (empty) {
      void createFromTemplate(empty);
    } else {
      addToast(
        "error",
        "Empty template is unavailable. Pick a template from the list below.",
      );
    }
  };

  async function createFromTemplate(t: TemplateChoice) {
    try {
      await createProject(t.key, `${t.name} Project`);
      navigate("/design");
    } catch (err) {
      addToast(
        "error",
        `Failed to create project: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }

  async function onOpenProject() {
    const dialog = await aec.dialog.openDirectory({
      title: "Open AEC Studio project",
    });
    if (dialog.canceled || !dialog.path) return;
    try {
      await openProject(dialog.path);
      navigate("/design");
    } catch (err) {
      addToast(
        "error",
        `Failed to open project: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }

  async function onOpenRecent(p: ProjectSummary) {
    try {
      await openProject(p.path);
      navigate("/design");
    } catch (err) {
      // The project may have been moved / deleted. Remove it from
      // the recents list so the user doesn't keep clicking a dead
      // card, and show an error toast.
      setRecents((prev) =>
        prev.filter((r) => r.projectId !== p.projectId),
      );
      addToast(
        "error",
        `Could not open "${p.name}": ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }

  return (
    <div className="home">
      <OnboardingModal
        open={recents.length === 0 && !onboardingDismissed}
        onDismiss={dismissOnboarding}
        onStartEmpty={startEmptyFromOnboarding}
      />
      <header className="home__header">
        <div>
          <h1 className="home__title">AEC Studio</h1>
          <p className="home__subtitle">
            Local-first design, drafting, BIM, and rendering.
          </p>
        </div>
        <button
          type="button"
          className="home__open-btn"
          onClick={onOpenProject}
          data-testid="home-open-project"
        >
          Open Project
        </button>
      </header>

      <HardwareProfileCard status={status} />

      <section>
        <header className="home__section-header">
          <h2 className="home__section-title">Recent projects</h2>
        </header>
        {recents.length === 0 ? (
          <div className="card" role="status">
            No recent projects yet. Start one from a template below.
          </div>
        ) : (
          <div className="recent-grid">
            {recents.map((p) => (
              <ProjectCard
                key={p.projectId}
                project={p}
                onOpen={() => void onOpenRecent(p)}
              />
            ))}
          </div>
        )}
      </section>

      <section>
        <header className="home__section-header">
          <h2 className="home__section-title">Templates</h2>
        </header>
        <div className="template-grid">
          {DEFAULT_TEMPLATES.map((t) => (
            <TemplateCard key={t.key} template={t} onCreate={createFromTemplate} />
          ))}
        </div>
      </section>
    </div>
  );
}
