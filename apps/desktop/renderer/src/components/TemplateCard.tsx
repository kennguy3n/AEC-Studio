export interface TemplateChoice {
  key: string;
  name: string;
  description: string;
  category: "interior" | "architecture" | "drafting";
  icon: string;
}

interface Props {
  template: TemplateChoice;
  onCreate?: (template: TemplateChoice) => void;
}

export function TemplateCard({ template, onCreate }: Props) {
  return (
    <article className="card template-card" data-testid={`template-${template.key}`}>
      <div className="template-card__icon" aria-hidden>
        {template.icon}
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
  { key: "interior.apartment", name: "Apartment", description: "60 m² urban apartment.", category: "interior", icon: "Ap" },
  { key: "interior.kitchen", name: "Kitchen", description: "Modern kitchen with island.", category: "interior", icon: "Kt" },
  { key: "interior.bathroom", name: "Bathroom", description: "Bathroom with wet areas.", category: "interior", icon: "Bt" },
  { key: "interior.renovation", name: "Renovation", description: "Demo/keep/new overlay.", category: "interior", icon: "Rn" },
  { key: "architecture.cafe", name: "Café", description: "120 m² café with banquette.", category: "architecture", icon: "Cf" },
  { key: "architecture.office", name: "Office", description: "Open floor with meeting rooms.", category: "architecture", icon: "Of" },
  { key: "architecture.villa", name: "Villa", description: "Multi-storey villa.", category: "architecture", icon: "Vl" },
  { key: "architecture.retail", name: "Retail", description: "Boutique storefront.", category: "architecture", icon: "Rt" },
];
