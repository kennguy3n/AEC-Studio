import { useEffect, useRef, useState } from "react";
import { aec } from "../../api/aec";

/**
 * Property value as seen in the IFC layer. IFC types map to a small set of
 * primitive values plus length / area / volume (numbers). We keep them as
 * untagged JSON so the existing IPC contract doesn't need to change; the
 * Rust side validates that the value matches the property set schema.
 */
export type PropertyValue = string | number | boolean;

export interface PsetData {
  [psetName: string]: { [key: string]: PropertyValue };
}

interface Props {
  entityId: string | null;
  classification: string | null;
  psets: PsetData;
  onChange: (psets: PsetData) => void;
}

export function PropertyEditor({
  entityId,
  classification,
  psets,
  onChange,
}: Props) {
  if (!entityId) {
    return (
      <section
        className="bim-properties bim-properties--empty"
        data-testid="property-editor"
      >
        <p>Select an element to edit its properties.</p>
      </section>
    );
  }
  return (
    <section
      className="bim-properties"
      aria-label="Property editor"
      data-testid="property-editor"
    >
      <header>
        <span className="bim-properties__entity">{entityId}</span>
        {classification && (
          <span
            className="bim-properties__class"
            data-testid="property-editor-class"
          >
            {classification}
          </span>
        )}
      </header>
      {Object.entries(psets).length === 0 && (
        <p className="bim-properties__hint" data-testid="property-editor-empty">
          No property sets defined for this element yet.
        </p>
      )}
      {Object.entries(psets).map(([psetName, props]) => (
        <PsetSection
          key={psetName}
          entityId={entityId}
          psetName={psetName}
          properties={props}
          onChange={(updated) => onChange({ ...psets, [psetName]: updated })}
        />
      ))}
    </section>
  );
}

interface PsetProps {
  entityId: string;
  psetName: string;
  properties: { [key: string]: PropertyValue };
  onChange: (props: { [key: string]: PropertyValue }) => void;
}

function PsetSection({ entityId, psetName, properties, onChange }: PsetProps) {
  return (
    <fieldset
      className="bim-properties__pset"
      data-testid={`pset-${psetName}`}
    >
      <legend>{psetName}</legend>
      <table>
        <tbody>
          {Object.entries(properties).map(([key, value]) => (
            <PsetRow
              key={key}
              entityId={entityId}
              psetName={psetName}
              propertyKey={key}
              value={value}
              onChange={(v) => onChange({ ...properties, [key]: v })}
            />
          ))}
        </tbody>
      </table>
    </fieldset>
  );
}

interface RowProps {
  entityId: string;
  psetName: string;
  propertyKey: string;
  value: PropertyValue;
  onChange: (value: PropertyValue) => void;
}

function PsetRow({
  entityId,
  psetName,
  propertyKey,
  value,
  onChange,
}: RowProps) {
  const [draft, setDraft] = useState<string>(String(value));
  const [saving, setSaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  // Sync `draft` from the `value` prop whenever a new value arrives. This
  // covers the case where React reuses the same `PsetRow` instance for a
  // different entity (keyed by propertyKey only) or when the parent pushes
  // an externally-edited value back into this row. We guard against
  // overwriting the user's in-progress edit by skipping the sync while the
  // input is focused — a focused field implies an active edit that must
  // not be stomped by external updates.
  useEffect(() => {
    if (document.activeElement !== inputRef.current) {
      setDraft(String(value));
    }
  }, [value, entityId, psetName, propertyKey]);

  const commit = async () => {
    const typed = coerce(value, draft);
    setSaving(true);
    try {
      await aec.bim.setProperty({
        entityId,
        pset: psetName,
        key: propertyKey,
        value: typed,
      });
      onChange(typed);
    } finally {
      setSaving(false);
    }
  };

  if (typeof value === "boolean") {
    return (
      <tr>
        <th scope="row">{propertyKey}</th>
        <td>
          <input
            type="checkbox"
            checked={draft === "true"}
            disabled={saving}
            data-testid={`pset-${psetName}-${propertyKey}`}
            onChange={async (e) => {
              setDraft(e.target.checked ? "true" : "false");
              setSaving(true);
              try {
                await aec.bim.setProperty({
                  entityId,
                  pset: psetName,
                  key: propertyKey,
                  value: e.target.checked,
                });
                onChange(e.target.checked);
              } finally {
                setSaving(false);
              }
            }}
          />
        </td>
      </tr>
    );
  }
  return (
    <tr>
      <th scope="row">{propertyKey}</th>
      <td>
        <input
          ref={inputRef}
          type={typeof value === "number" ? "number" : "text"}
          step={typeof value === "number" ? "any" : undefined}
          value={draft}
          disabled={saving}
          data-testid={`pset-${psetName}-${propertyKey}`}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
          }}
        />
      </td>
    </tr>
  );
}

function coerce(original: PropertyValue, draft: string): PropertyValue {
  if (typeof original === "number") {
    const n = Number(draft);
    return Number.isFinite(n) ? n : original;
  }
  if (typeof original === "boolean") {
    return draft === "true";
  }
  return draft;
}
