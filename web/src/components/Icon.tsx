const paths = {
  queue: "M4 5h16v11H4z M8 16l1.5 3h5l1.5-3 M8 9h8 M8 12h5",
  runs: "M3 12h4l2-6 4 12 2-6h6",
  inbox: "M4 6h16v12H4z M4 12h5l1 2h4l1-2h5",
  settings:
    "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Z M11 2h2l1 3 2 .8 2.8-1.4 1.4 1.4L19 9l.8 2 3 1v2l-3 1-.8 2 1.4 2.8-1.4 1.4L16 20l-2 .8-1 3h-2l-1-3-2-.8-2.8 1.4-1.4-1.4L5 17l-.8-2-3-1v-2l3-1 .8-2-1.4-2.8 1.4-1.4L8 6l2-.8z",
  close: "m6 6 12 12M18 6 6 18",
  more: "M4 12h.01 M12 12h.01 M20 12h.01",
  plus: "M12 5v14M5 12h14",
  search: "M15.7 15.7 20 20 M17 11a6 6 0 1 1-12 0 6 6 0 0 1 12 0Z",
  refresh: "M20 7v5h-5 M4 17v-5h5 M5.4 8A7 7 0 0 1 17 6l3 6 M4 12l3 6a7 7 0 0 0 11.6-2",
  calendar: "M4 6h16v15H4z M8 3v6m8-6v6M4 11h16M8 15h2m4 0h2m-8 3h2",
  collapse: "M4 5h16v14H4z M9 5v14",
  sun: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8ZM12 2v2m0 16v2M2 12h2m16 0h2M5 5l2 2m10 10 2 2m0-14-2 2M7 17l-2 2",
  back: "m11 5-7 7 7 7M4 12h16",
  branch:
    "M7 6v12M7 15c0-4 10-2 10-8 M9 4a2 2 0 1 0-4 0 2 2 0 0 0 4 0Z M9 20a2 2 0 1 0-4 0 2 2 0 0 0 4 0Z M19 5a2 2 0 1 0-4 0 2 2 0 0 0 4 0Z",
  chevron: "m6 9 6 6 6-6",
  external: "M13 4h7v7m0-7-9 9 M10 5H5v14h14v-5",
  check: "m5 12 4 4L19 6",
  clearFilters: "M3 4h18l-7 7v1M10 11 3 4m7 7v8l4-2m2-3 5 5m0-5-5 5",
  warning: "m12 3 10 17H2zm0 6v5m0 3h.01",
  send: "M12 20V4m-6 6 6-6 6 6",
  terminal: "M4 5h16v14H4z m3 4 3 3-3 3m5 0h4",
} as const;

export function Icon({
  name,
  size = 16,
}: {
  readonly name: keyof typeof paths;
  readonly size?: number;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={name === "more" ? 4 : 1.65}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={paths[name]} />
    </svg>
  );
}
