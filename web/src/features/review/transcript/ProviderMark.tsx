import type { HostProviderDto } from "../../../api/generated/contracts/common";
import { Icon } from "../../../components/Icon";

export function ProviderMark({ provider }: { readonly provider: HostProviderDto }) {
  if (provider === "codex") return <Icon name="terminal" size={14} />;
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      aria-hidden="true"
    >
      <path d="m12 2 1.5 6.5L19 5l-3.5 5.5L22 12l-6.5 1.5L19 19l-5.5-3.5L12 22l-1.5-6.5L5 19l3.5-5.5L2 12l6.5-1.5L5 5l5.5 3.5Z" />
    </svg>
  );
}
