export interface CameraTile {
  id: string;
  name: string;
  preset?: string | null;
  thumbnailDataUri?: string | null;
}

interface Props {
  cameras: CameraTile[];
  selected: Set<string>;
  onToggle: (id: string) => void;
}

export function CameraSelector({ cameras, selected, onToggle }: Props) {
  if (cameras.length === 0) {
    return (
      <section
        className="render-cameras render-cameras--empty"
        data-testid="camera-selector"
      >
        <p>
          No saved cameras yet. Use "Save current view" in the 3D viewport to
          create one.
        </p>
      </section>
    );
  }
  return (
    <section
      className="render-cameras"
      aria-label="Camera selector"
      data-testid="camera-selector"
    >
      <ul>
        {cameras.map((c) => {
          const isSelected = selected.has(c.id);
          return (
            <li
              key={c.id}
              className={`render-cameras__tile${isSelected ? " render-cameras__tile--selected" : ""}`}
              data-testid={`camera-tile-${c.id}`}
            >
              <button
                type="button"
                onClick={() => onToggle(c.id)}
                aria-pressed={isSelected}
                data-testid={`camera-toggle-${c.id}`}
              >
                {c.thumbnailDataUri ? (
                  <img
                    src={c.thumbnailDataUri}
                    alt={`${c.name} preview`}
                    width={64}
                    height={36}
                  />
                ) : (
                  <span className="render-cameras__placeholder">No preview</span>
                )}
                <span className="render-cameras__name">{c.name}</span>
                {c.preset && (
                  <span className="render-cameras__preset">{c.preset}</span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
