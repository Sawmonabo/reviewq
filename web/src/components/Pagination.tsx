export function Pagination({
  trail,
  next,
  onPage,
}: {
  readonly trail: readonly string[];
  readonly next: string | null | undefined;
  readonly onPage: (trail: readonly string[]) => void;
}) {
  return (
    <PageNavigation
      page={trail.length + 1}
      first={trail.length ? () => onPage([]) : undefined}
      previous={trail.length ? () => onPage(trail.slice(0, -1)) : undefined}
      next={next ? () => onPage([...trail, next]) : undefined}
    />
  );
}

export function PageNavigation({
  page,
  first,
  previous,
  next,
}: {
  readonly page: number;
  readonly first: (() => void) | undefined;
  readonly previous: (() => void) | undefined;
  readonly next: (() => void) | undefined;
}) {
  if (page === 1 && !next) return null;
  return (
    <nav className="pagination" aria-label="Result pages">
      {first && (
        <button type="button" onClick={first}>
          First page
        </button>
      )}
      <button type="button" disabled={!previous} onClick={previous}>
        Previous
      </button>
      <span className="meta">Page {page}</span>
      <button type="button" disabled={!next} onClick={next}>
        Next
      </button>
    </nav>
  );
}
