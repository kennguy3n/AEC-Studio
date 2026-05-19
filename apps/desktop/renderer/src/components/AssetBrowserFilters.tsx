const TAG_OPTIONS = ["furniture", "sofa", "chair", "table", "lighting", "kitchen", "bath"];
const STYLE_OPTIONS = [
  "scandinavian",
  "japandi",
  "modern",
  "industrial",
  "classic",
  "minimal",
];

interface Props {
  tags: string[];
  styleTags: string[];
  onTagsChange: (t: string[]) => void;
  onStyleTagsChange: (t: string[]) => void;
}

export function AssetBrowserFilters({
  tags,
  styleTags,
  onTagsChange,
  onStyleTagsChange,
}: Props) {
  function toggle(arr: string[], v: string, setter: (next: string[]) => void) {
    setter(arr.includes(v) ? arr.filter((x) => x !== v) : [...arr, v]);
  }

  return (
    <aside className="asset-filters" aria-label="Asset filters">
      <div>
        <div className="asset-filters__group-title">Category</div>
        {TAG_OPTIONS.map((t) => (
          <label key={t} className="asset-filters__check">
            <input
              type="checkbox"
              checked={tags.includes(t)}
              onChange={() => toggle(tags, t, onTagsChange)}
              data-testid={`filter-tag-${t}`}
            />
            {t}
          </label>
        ))}
      </div>
      <div>
        <div className="asset-filters__group-title">Style</div>
        {STYLE_OPTIONS.map((t) => (
          <label key={t} className="asset-filters__check">
            <input
              type="checkbox"
              checked={styleTags.includes(t)}
              onChange={() => toggle(styleTags, t, onStyleTagsChange)}
              data-testid={`filter-style-${t}`}
            />
            {t}
          </label>
        ))}
      </div>
    </aside>
  );
}
