import { useEffect, useMemo, useState } from "react";
import { aec, AssetSummary } from "../api/aec";
import { AssetBrowserFilters } from "./AssetBrowserFilters";
import { AssetBrowserGrid } from "./AssetBrowserGrid";
import { AssetBrowserListItem } from "./AssetBrowserListItem";

export type AssetView = "grid" | "list";

export function AssetBrowser() {
  const [tags, setTags] = useState<string[]>([]);
  const [styleTags, setStyleTags] = useState<string[]>([]);
  const [search, setSearch] = useState("");
  const [view, setView] = useState<AssetView>("grid");
  const [assets, setAssets] = useState<AssetSummary[]>([]);
  const [page, setPage] = useState(1);
  const pageSize = 24;

  useEffect(() => {
    let alive = true;
    // Whenever the filter set changes, refetch AND reset to page 1 so the
    // user never lands on an empty page beyond the new result set.
    setPage(1);
    void aec.design
      .listAssets({ tags, styleTags, search, limit: 100 })
      .then((rows) => {
        if (alive) setAssets(rows as AssetSummary[]);
      });
    return () => {
      alive = false;
    };
  }, [tags, styleTags, search]);

  // Defense-in-depth: if assets shrink below the current page window
  // (e.g. native backend returns fewer rows for the same filters), clamp.
  const lastPage = Math.max(1, Math.ceil(assets.length / pageSize));
  useEffect(() => {
    if (page > lastPage) setPage(lastPage);
  }, [page, lastPage]);

  const paged = useMemo(
    () => assets.slice((page - 1) * pageSize, page * pageSize),
    [assets, page],
  );

  function onDragStart(e: React.DragEvent, asset: AssetSummary) {
    e.dataTransfer.setData("application/aec-asset-id", asset.assetId);
    e.dataTransfer.effectAllowed = "copy";
  }

  return (
    <div className="asset-browser" data-testid="asset-browser">
      <AssetBrowserFilters
        tags={tags}
        styleTags={styleTags}
        onTagsChange={setTags}
        onStyleTagsChange={setStyleTags}
      />
      <div>
        <header style={{ display: "flex", gap: 8, marginBottom: 12 }}>
          <input
            type="search"
            placeholder="Search…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            data-testid="asset-search"
            aria-label="Search assets"
            style={{ flex: 1, padding: 6 }}
          />
          <button
            type="button"
            className={`button button--${view === "grid" ? "secondary" : "ghost"}`}
            onClick={() => setView("grid")}
            data-testid="asset-view-grid"
          >
            Grid
          </button>
          <button
            type="button"
            className={`button button--${view === "list" ? "secondary" : "ghost"}`}
            onClick={() => setView("list")}
            data-testid="asset-view-list"
          >
            List
          </button>
        </header>

        {view === "grid" ? (
          <AssetBrowserGrid assets={paged} onDragStart={onDragStart} />
        ) : (
          <ul className="asset-list">
            {paged.map((a) => (
              <AssetBrowserListItem
                key={a.assetId}
                asset={a}
                onDragStart={(e) => onDragStart(e, a)}
              />
            ))}
          </ul>
        )}

        <Pagination
          total={assets.length}
          pageSize={pageSize}
          page={page}
          onPage={setPage}
        />
      </div>
    </div>
  );
}

function Pagination({
  total,
  pageSize,
  page,
  onPage,
}: {
  total: number;
  pageSize: number;
  page: number;
  onPage: (p: number) => void;
}) {
  const lastPage = Math.max(1, Math.ceil(total / pageSize));
  if (lastPage <= 1) return null;
  return (
    <nav
      style={{ marginTop: 12, display: "flex", gap: 8 }}
      aria-label="Asset pagination"
    >
      <button
        type="button"
        className="button button--ghost"
        disabled={page <= 1}
        onClick={() => onPage(Math.max(1, page - 1))}
        data-testid="page-prev"
      >
        Prev
      </button>
      <span style={{ alignSelf: "center", fontSize: 12 }}>
        Page {page} / {lastPage}
      </span>
      <button
        type="button"
        className="button button--ghost"
        disabled={page >= lastPage}
        onClick={() => onPage(Math.min(lastPage, page + 1))}
        data-testid="page-next"
      >
        Next
      </button>
    </nav>
  );
}
