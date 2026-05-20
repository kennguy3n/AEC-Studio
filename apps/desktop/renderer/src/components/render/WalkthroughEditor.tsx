import { useState } from "react";

export interface WalkthroughKeyframe {
  frame: number;
  position: [number, number, number];
  target: [number, number, number];
}

interface Props {
  keyframes: WalkthroughKeyframe[];
  onChange: (kf: WalkthroughKeyframe[]) => void;
  onSubmit?: (kf: WalkthroughKeyframe[]) => Promise<void> | void;
}

/**
 * Editor for a walkthrough camera path. Keyframes are added (currently
 * by typing into the inline form), edited, and removed. The list is
 * always rendered sorted by frame so the user can visually reason about
 * the resulting animation. Duplicate frames are blocked because the
 * Rust `CameraPath::from_keyframes` rejects them.
 */
export function WalkthroughEditor({ keyframes, onChange, onSubmit }: Props) {
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<WalkthroughKeyframe>({
    frame: nextDraftFrame(keyframes),
    position: [0, -5000, 1500],
    target: [0, 0, 1500],
  });
  const [error, setError] = useState<string | null>(null);

  const addKeyframe = () => {
    if (keyframes.some((k) => k.frame === draft.frame)) {
      setError(`Frame ${draft.frame} already has a keyframe`);
      return;
    }
    setError(null);
    const next = [...keyframes, draft].sort((a, b) => a.frame - b.frame);
    onChange(next);
    setDraft({
      frame: nextDraftFrame(next),
      position: draft.position,
      target: draft.target,
    });
  };

  const removeKeyframe = (frame: number) => {
    onChange(keyframes.filter((k) => k.frame !== frame));
  };

  const submit = async () => {
    if (!onSubmit) return;
    setBusy(true);
    try {
      await onSubmit(keyframes);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      className="walkthrough-editor"
      aria-label="Walkthrough editor"
      data-testid="walkthrough-editor"
    >
      <header>
        <h3>Walkthrough keyframes</h3>
        <p>
          {keyframes.length} keyframe{keyframes.length === 1 ? "" : "s"}.
          The camera will linearly interpolate between consecutive
          frames.
        </p>
      </header>

      <ol className="walkthrough-editor__list">
        {keyframes.map((k) => (
          <li
            key={k.frame}
            data-testid={`walkthrough-keyframe-${k.frame}`}
          >
            <span>Frame {k.frame}</span>
            <span>
              pos [{k.position.map(round1).join(", ")}]
            </span>
            <span>
              target [{k.target.map(round1).join(", ")}]
            </span>
            <button
              type="button"
              onClick={() => removeKeyframe(k.frame)}
              data-testid={`walkthrough-remove-${k.frame}`}
            >
              Remove
            </button>
          </li>
        ))}
      </ol>

      <form
        className="walkthrough-editor__draft"
        onSubmit={(e) => {
          e.preventDefault();
          addKeyframe();
        }}
      >
        <label>
          Frame
          <input
            type="number"
            min={0}
            value={draft.frame}
            data-testid="walkthrough-draft-frame"
            onChange={(e) =>
              setDraft({ ...draft, frame: Number(e.target.value) })
            }
          />
        </label>
        <fieldset>
          <legend>Position (mm)</legend>
          {(["x", "y", "z"] as const).map((axis, i) => (
            <label key={axis}>
              {axis}
              <input
                type="number"
                value={draft.position[i]}
                data-testid={`walkthrough-draft-position-${axis}`}
                onChange={(e) => {
                  const next = [...draft.position] as [
                    number,
                    number,
                    number,
                  ];
                  next[i] = Number(e.target.value);
                  setDraft({ ...draft, position: next });
                }}
              />
            </label>
          ))}
        </fieldset>
        <fieldset>
          <legend>Target (mm)</legend>
          {(["x", "y", "z"] as const).map((axis, i) => (
            <label key={axis}>
              {axis}
              <input
                type="number"
                value={draft.target[i]}
                data-testid={`walkthrough-draft-target-${axis}`}
                onChange={(e) => {
                  const next = [...draft.target] as [
                    number,
                    number,
                    number,
                  ];
                  next[i] = Number(e.target.value);
                  setDraft({ ...draft, target: next });
                }}
              />
            </label>
          ))}
        </fieldset>
        <button type="submit" data-testid="walkthrough-add">
          Add keyframe
        </button>
        {error && (
          <p
            className="walkthrough-editor__error"
            role="alert"
            data-testid="walkthrough-error"
          >
            {error}
          </p>
        )}
      </form>

      {onSubmit && (
        <button
          type="button"
          className="walkthrough-editor__render"
          data-testid="walkthrough-submit"
          disabled={busy || keyframes.length < 2}
          onClick={submit}
        >
          {busy ? "Submitting…" : "Render walkthrough"}
        </button>
      )}
    </section>
  );
}

function round1(n: number): string {
  return (Math.round(n * 10) / 10).toString();
}

function nextDraftFrame(kf: WalkthroughKeyframe[]): number {
  if (kf.length === 0) return 1;
  return Math.max(...kf.map((k) => k.frame)) + 30;
}
