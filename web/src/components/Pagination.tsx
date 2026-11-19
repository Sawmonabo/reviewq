export function Pagination({
  trail,
  next,
  onPage,
}: {
  readonly trail: readonly string[];
  readonly next: string | null | undefined;
  readonly onPage: (trail: readonly string[]) => void;
}) {
  if (!trail.length && !next) return null;
  return (
    <nav className="pagination" aria-label="Result pages">
      {trail.length > 0 && (
        <button type="button" onClick={() => onPage([])}>
          First page
        </button>
      )}
      <button type="button" disabled={!trail.length} onClick={() => onPage(trail.slice(0, -1))}>
        Previous
      </button>
      <span className="meta">Page {trail.length + 1}</span>
      <button
        type="button"
        disabled={!next}
        onClick={() => {
          if (next) onPage([...trail, next]);
        }}
      >
        Next
      </button>
    </nav>
  );
}
