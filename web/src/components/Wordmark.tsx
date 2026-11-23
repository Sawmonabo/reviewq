export function Wordmark({ className = "brand-wordmark" }: { readonly className?: string }) {
  return (
    <span className={className} aria-hidden="true">
      <span>Review</span>
      <svg className="brand-q" viewBox="7 3 20 26">
        <path
          d="M22.5 17V12a6.5 6.5 0 0 0-13 0v7a6.5 6.5 0 0 0 6.5 6.5h2"
          fill="none"
          stroke="currentColor"
          strokeWidth="3"
          strokeLinecap="square"
        />
        <path
          d="m17 19 7 7"
          fill="none"
          stroke="var(--link)"
          strokeWidth="3"
          strokeLinecap="square"
        />
      </svg>
    </span>
  );
}
