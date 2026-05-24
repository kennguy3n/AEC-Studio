export type BimAction =
  | "importIfc"
  | "attachIfc"
  | "exportIfc"
  | "validate"
  | "classify"
  | "generateSchedule"
  | "diff"
  | "boq";

const ACTIONS: { id: BimAction; label: string }[] = [
  { id: "importIfc", label: "Import IFC" },
  // PR-P: `Attach` runs `bim_attach_ifc` after a preview parse,
  // folding the snapshot into the active project's SQLCipher DB.
  { id: "attachIfc", label: "Attach IFC" },
  { id: "exportIfc", label: "Export IFC" },
  { id: "validate", label: "Validate" },
  { id: "classify", label: "Classify" },
  { id: "generateSchedule", label: "Schedule" },
  { id: "diff", label: "Diff" },
  { id: "boq", label: "BOQ" },
];

interface Props {
  busyAction: BimAction | null;
  onInvoke: (action: BimAction) => void;
}

export function BimToolbar({ busyAction, onInvoke }: Props) {
  return (
    <nav
      className="bim-toolbar"
      role="toolbar"
      aria-label="BIM tools"
      data-testid="bim-toolbar"
    >
      {ACTIONS.map((a) => {
        const isBusy = busyAction === a.id;
        return (
          <button
            key={a.id}
            type="button"
            className={`bim-toolbar__btn${isBusy ? " bim-toolbar__btn--busy" : ""}`}
            data-testid={`bim-action-${a.id}`}
            disabled={busyAction !== null && busyAction !== a.id}
            onClick={() => onInvoke(a.id)}
          >
            {isBusy ? "…" : a.label}
          </button>
        );
      })}
    </nav>
  );
}
