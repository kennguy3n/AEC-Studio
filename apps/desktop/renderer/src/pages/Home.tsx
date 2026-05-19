import { useEffect, useState } from "react";
import { aec, ProjectSummary, RuntimeStatus } from "../api/aec";
import { ProjectCard } from "../components/ProjectCard";
import { TemplateCard, DEFAULT_TEMPLATES, TemplateChoice } from "../components/TemplateCard";
import { HardwareProfileCard } from "../components/HardwareProfileCard";

export function Home() {
  const [recents, setRecents] = useState<ProjectSummary[]>([]);
  const [status, setStatus] = useState<RuntimeStatus | null>(null);

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

  async function createFromTemplate(t: TemplateChoice) {
    const summary = (await aec.project.createFromTemplate(
      t.key,
      `${t.name} Project`,
    )) as ProjectSummary;
    setRecents((prev) => [summary, ...prev.filter((p) => p.projectId !== summary.projectId)]);
  }

  return (
    <div className="home">
      <header className="home__header">
        <div>
          <h1 className="home__title">AEC Studio</h1>
          <p className="home__subtitle">
            Local-first design, drafting, BIM, and rendering.
          </p>
        </div>
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
              <ProjectCard key={p.projectId} project={p} />
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
