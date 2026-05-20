interface Props {
  imageDataUri: string | null;
  caption?: string;
}

export function RenderPreview({ imageDataUri, caption }: Props) {
  return (
    <figure
      className="render-preview"
      aria-label="Render preview"
      data-testid="render-preview"
    >
      {imageDataUri ? (
        <img
          src={imageDataUri}
          alt={caption ?? "Latest render"}
          data-testid="render-preview-image"
        />
      ) : (
        <div className="render-preview__empty" data-testid="render-preview-empty">
          <p>No preview yet. Queue a render or load an EEVEE preview.</p>
        </div>
      )}
      {caption && <figcaption>{caption}</figcaption>}
    </figure>
  );
}
