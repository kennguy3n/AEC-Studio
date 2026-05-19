import type { ProjectSummary } from "../api/aec";

interface Props {
  project: ProjectSummary;
  onOpen?: (project: ProjectSummary) => void;
}

export function ProjectCard({ project, onOpen }: Props) {
  return (
    <article className="card project-card" data-testid="project-card">
      <div className="project-card__thumb" aria-hidden />
      <div>
        <div className="project-card__name">{project.name}</div>
        <div className="project-card__meta">
          {project.templateKey ? `${project.templateKey} · ` : ""}
          {formatRelative(project.modifiedAt)}
        </div>
      </div>
      <button
        type="button"
        className="button button--secondary"
        onClick={() => onOpen?.(project)}
      >
        Open
      </button>
    </article>
  );
}

function formatRelative(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  const delta = Date.now() - d.getTime();
  const minutes = Math.round(delta / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days} d ago`;
  return d.toLocaleDateString();
}
