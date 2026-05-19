import type { AssetSummary } from "../api/aec";

interface Props {
  assets: AssetSummary[];
  onDragStart?: (e: React.DragEvent, asset: AssetSummary) => void;
}

export function AssetBrowserGrid({ assets, onDragStart }: Props) {
  if (assets.length === 0) {
    return <div className="card">No assets match your filters.</div>;
  }
  return (
    <ul className="asset-grid" data-testid="asset-grid">
      {assets.map((a) => (
        <li
          key={a.assetId}
          className="asset-grid__item"
          draggable
          onDragStart={(e) => onDragStart?.(e, a)}
          data-testid={`asset-${a.assetId}`}
        >
          <div className="asset-grid__thumb" aria-hidden />
          <div className="asset-grid__name">{a.name}</div>
          <div className="asset-grid__vendor">{a.vendor ?? "—"}</div>
        </li>
      ))}
    </ul>
  );
}
