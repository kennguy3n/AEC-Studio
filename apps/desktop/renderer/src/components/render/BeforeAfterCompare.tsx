import { useState } from "react";

interface Props {
  before: string | null;
  after: string | null;
}

export function BeforeAfterCompare({ before, after }: Props) {
  const [position, setPosition] = useState(50);

  if (!before || !after) {
    return (
      <section
        className="render-compare render-compare--empty"
        data-testid="before-after-compare"
      >
        <p>Pick two renders to compare.</p>
      </section>
    );
  }
  return (
    <section
      className="render-compare"
      aria-label="Before / after compare"
      data-testid="before-after-compare"
    >
      <div className="render-compare__wrap">
        <img
          src={before}
          alt="Before"
          className="render-compare__before"
          data-testid="render-compare-before"
        />
        <img
          src={after}
          alt="After"
          className="render-compare__after"
          style={{ clipPath: `inset(0 0 0 ${position}%)` }}
          data-testid="render-compare-after"
        />
        <div
          className="render-compare__divider"
          style={{ left: `${position}%` }}
        />
      </div>
      <input
        type="range"
        min={0}
        max={100}
        value={position}
        onChange={(e) => setPosition(Number(e.target.value))}
        aria-label="Comparison slider"
        data-testid="render-compare-slider"
      />
    </section>
  );
}
