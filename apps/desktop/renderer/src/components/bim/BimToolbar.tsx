import { Icon, type IconName } from "../../icons/Icon";

export type BimAction =
  | "importIfc"
  | "attachIfc"
  | "exportIfc"
  | "validate"
  | "classify"
  | "generateSchedule"
  | "diff"
  | "boq";

const ACTIONS: { id: BimAction; label: string; icon: IconName }[] = [
  { id: "importIfc", label: "Import IFC", icon: "importIfc" },
  // PR-P: `Attach` runs `bim_attach_ifc` after a preview parse,
  // folding the snapshot into the active project's SQLCipher DB.
  { id: "attachIfc", label: "Attach IFC", icon: "attachIfc" },
  { id: "exportIfc", label: "Export IFC", icon: "exportIfc" },
  { id: "validate", label: "Validate", icon: "validate" },
  { id: "classify", label: "Classify", icon: "classify" },
  { id: "generateSchedule", label: "Schedule", icon: "schedule" },
  { id: "diff", label: "Diff", icon: "diff" },
  { id: "boq", label: "BOQ", icon: "boq" },
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
            title={a.label}
          >
            <Icon name={a.icon} size={16} />
            <span className="bim-toolbar__label">
              {isBusy ? "…" : a.label}
            </span>
          </button>
        );
      })}
    </nav>
  );
}
