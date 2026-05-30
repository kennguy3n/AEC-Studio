/**
 * Toolbar at the top of Deliver mode. Triggers the three top-level
 * actions: Export the configured pack, tag a new revision, compare
 * the currently-selected revision pair.
 *
 * The toolbar is intentionally state-less — it only invokes callbacks
 * the page owns. Buttons are disabled when the underlying action
 * isn't valid (e.g. compare needs two distinct revisions).
 */
import { Icon } from "../../icons/Icon";

export interface DeliverToolbarProps {
  onExportPack: () => void;
  onTagRevision: () => void;
  onCompareRevisions: () => void;
  exporting: boolean;
  canExport: boolean;
  canTag: boolean;
  canCompare: boolean;
}

export function DeliverToolbar({
  onExportPack,
  onTagRevision,
  onCompareRevisions,
  exporting,
  canExport,
  canTag,
  canCompare,
}: DeliverToolbarProps): JSX.Element {
  return (
    <div role="toolbar" aria-label="Deliver actions" data-testid="deliver-toolbar">
      <button
        type="button"
        onClick={onExportPack}
        disabled={exporting || !canExport}
        data-testid="toolbar-export"
      >
        <Icon name="exportPack" size={16} />
        <span className="deliver-toolbar__label">
          {exporting ? "Exporting…" : "Export pack"}
        </span>
      </button>
      <button
        type="button"
        onClick={onTagRevision}
        disabled={!canTag}
        data-testid="toolbar-tag"
      >
        <Icon name="tagRevision" size={16} />
        <span className="deliver-toolbar__label">Tag revision</span>
      </button>
      <button
        type="button"
        onClick={onCompareRevisions}
        disabled={!canCompare}
        data-testid="toolbar-compare"
      >
        <Icon name="compareRevisions" size={16} />
        <span className="deliver-toolbar__label">Compare revisions</span>
      </button>
    </div>
  );
}
