import type { AssetSummary } from "../api/aec";

interface Props {
  asset: AssetSummary;
  onDragStart?: (e: React.DragEvent) => void;
}

export function AssetBrowserListItem({ asset, onDragStart }: Props) {
  return (
    <li
      className="asset-list__item"
      draggable
      onDragStart={onDragStart}
      data-testid={`asset-list-${asset.assetId}`}
    >
      <span className="asset-list__thumb" aria-hidden />
      <div>
        <div style={{ fontSize: 13, fontWeight: 600 }}>{asset.name}</div>
        <div style={{ fontSize: 11, color: "var(--aec-color-text-muted)" }}>
          {asset.vendor ?? "—"} · {asset.tags.join(", ")}
        </div>
      </div>
      <button type="button" className="button button--secondary">
        Place
      </button>
    </li>
  );
}
