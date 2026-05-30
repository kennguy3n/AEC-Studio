import { useEffect, useState } from "react";
import { Icon, type IconName } from "../icons/Icon";

/**
 * Phase 17 Group B Tasks 7 + 13 — template cards with SVG icons and
 * optional pre-rendered preview images.
 *
 * Each template optionally ships a 512×320 PNG preview in
 * `apps/desktop/renderer/public/templates/<key>/preview.png`. When
 * the file exists the card shows that as the hero; when missing (or
 * the fetch fails — e.g. headless test environments) the card falls
 * back to the SVG icon glyph in the same hero slot, so the gallery
 * never has a broken-image state.
 *
 * The preview detection uses a `HEAD`-style probe — actually a
 * `fetch(...)` with `cache: "force-cache"`, then inspects
 * `response.ok`. We do this from a `useEffect` (not eagerly) so SSR
 * / vitest runs that don't have a real fetch implementation degrade
 * to "no preview" without throwing. Subsequent renders of the same
 * template share the browser's HTTP cache.
 */
export interface TemplateChoice {
  key: string;
  name: string;
  description: string;
  category: "interior" | "architecture" | "drafting" | "general";
  /** Icon name to render when the preview image isn't available. */
  icon: IconName;
  /**
   * Optional override for the preview path. Defaults to
   * `/templates/<key>/preview.png`. Tests inject a known URL to
   * exercise the hover-zoom / fallback branches deterministically.
   */
  previewPath?: string;
}

interface Props {
  template: TemplateChoice;
  onCreate?: (template: TemplateChoice) => void;
}

function defaultPreviewPath(key: string): string {
  // Templates live under `public/templates/<key>/preview.png`. The
  // `public/` directory is served at the renderer root by Vite, so
  // the URL is just `/templates/<key>/preview.png`.
  return `/templates/${key}/preview.png`;
}

export function TemplateCard({ template, onCreate }: Props) {
  const candidate = template.previewPath ?? defaultPreviewPath(template.key);
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);

  useEffect(() => {
    // In test environments (jsdom) `fetch` may be a vi.fn or undefined.
    // Guard against both — failure to detect a preview means "no
    // preview, show icon" which is the safe default.
    if (typeof fetch !== "function") {
      setPreviewSrc(null);
      return;
    }
    let alive = true;
    void fetch(candidate, { cache: "force-cache" })
      .then((r) => {
        if (!alive) return;
        // 2xx → preview exists; otherwise fall back to icon. We
        // intentionally don't read the body — the URL is what we want
        // on the `<img src>` element.
        setPreviewSrc(r.ok ? candidate : null);
      })
      .catch(() => {
        if (alive) setPreviewSrc(null);
      });
    return () => {
      alive = false;
    };
  }, [candidate]);

  return (
    <article
      className="card template-card"
      data-testid={`template-${template.key}`}
    >
      <div
        className="template-card__hero"
        data-testid={`template-hero-${template.key}`}
        data-has-preview={previewSrc !== null}
        aria-hidden
      >
        {previewSrc !== null ? (
          <img
            src={previewSrc}
            alt=""
            className="template-card__preview"
            data-testid={`template-preview-${template.key}`}
          />
        ) : (
          <span className="template-card__icon">
            <Icon name={template.icon} size={28} />
          </span>
        )}
      </div>
      <div>
        <div className="template-card__name">{template.name}</div>
        <div className="template-card__desc">{template.description}</div>
      </div>
      <span className="pill">{template.category}</span>
      <button
        type="button"
        className="button"
        onClick={() => onCreate?.(template)}
      >
        New Project
      </button>
    </article>
  );
}

// eslint-disable-next-line react-refresh/only-export-components
export const DEFAULT_TEMPLATES: TemplateChoice[] = [
  // The "general.empty" template is the canonical starting point for
  // users who want to shape a project from scratch. The Phase 17
  // Group C onboarding modal's "Start with the Empty template" CTA
  // looks up this exact key (`Home.tsx::startEmptyFromOnboarding`),
  // so removing or renaming it would silently divert that flow to
  // whichever template happens to land at index 0 — the original bug
  // Devin Review flagged. The matching JSON definition lives at
  // `templates/general/empty.json` so `project.createFromTemplate`
  // can resolve the key through `TemplateLoader::load`.
  {
    key: "general.empty",
    name: "Empty",
    description: "Blank project — no rooms, walls, or presets.",
    category: "general",
    icon: "design",
  },
  {
    key: "interior.apartment",
    name: "Apartment",
    description: "60 m² urban apartment.",
    category: "interior",
    icon: "furniture",
  },
  {
    key: "interior.kitchen",
    name: "Kitchen",
    description: "Modern kitchen with island.",
    category: "interior",
    icon: "furniture",
  },
  {
    key: "interior.bathroom",
    name: "Bathroom",
    description: "Bathroom with wet areas.",
    category: "interior",
    icon: "window",
  },
  {
    key: "interior.renovation",
    name: "Renovation",
    description: "Demo/keep/new overlay.",
    category: "interior",
    icon: "wall",
  },
  {
    key: "architecture.cafe",
    name: "Café",
    description: "120 m² café with banquette.",
    category: "architecture",
    icon: "design",
  },
  {
    key: "architecture.office",
    name: "Office",
    description: "Open floor with meeting rooms.",
    category: "architecture",
    icon: "floor",
  },
  {
    key: "architecture.villa",
    name: "Villa",
    description: "Multi-storey villa.",
    category: "architecture",
    icon: "bim",
  },
  {
    key: "architecture.retail",
    name: "Retail",
    description: "Boutique storefront.",
    category: "architecture",
    icon: "deliver",
  },
];
