import { useState } from "react";
import { aec } from "../../api/aec";

export interface SheetTab {
  id: string;
  name: string;
}

interface Props {
  sheets: SheetTab[];
  activeId: string | null;
  onChange: (sheets: SheetTab[], activeId: string | null) => void;
}

export function SheetManager({ sheets, activeId, onChange }: Props) {
  const [creating, setCreating] = useState(false);

  const create = async () => {
    setCreating(true);
    try {
      const res = (await aec.draft.createSheet({ paper: "A3", orientation: "landscape" })) as {
        sheetId: string;
      };
      const next: SheetTab = {
        id: res.sheetId,
        name: `Sheet ${sheets.length + 1}`,
      };
      onChange([...sheets, next], next.id);
    } finally {
      setCreating(false);
    }
  };

  const rename = (id: string, name: string) => {
    onChange(
      sheets.map((s) => (s.id === id ? { ...s, name } : s)),
      activeId,
    );
  };

  const remove = (id: string) => {
    const filtered = sheets.filter((s) => s.id !== id);
    onChange(filtered, filtered[0]?.id ?? null);
  };

  return (
    <section
      className="draft-sheets"
      aria-label="Sheet manager"
      data-testid="sheet-manager"
    >
      <div className="draft-sheets__tabs" role="tablist">
        {sheets.map((s) => (
          <div
            key={s.id}
            role="tab"
            aria-selected={activeId === s.id}
            className={`draft-sheets__tab${activeId === s.id ? " draft-sheets__tab--active" : ""}`}
            data-testid={`sheet-tab-${s.id}`}
          >
            <button
              type="button"
              className="draft-sheets__select"
              onClick={() => onChange(sheets, s.id)}
            >
              {s.name}
            </button>
            <button
              type="button"
              className="draft-sheets__rename"
              onClick={() => {
                const v = window.prompt("Rename sheet", s.name);
                if (v && v.trim().length > 0) rename(s.id, v.trim());
              }}
              aria-label={`Rename ${s.name}`}
            >
              ✎
            </button>
            <button
              type="button"
              className="draft-sheets__remove"
              onClick={() => remove(s.id)}
              aria-label={`Delete ${s.name}`}
            >
              ×
            </button>
          </div>
        ))}
      </div>
      <button
        type="button"
        className="draft-sheets__create"
        onClick={create}
        disabled={creating}
        data-testid="sheet-create"
      >
        + New sheet
      </button>
    </section>
  );
}
