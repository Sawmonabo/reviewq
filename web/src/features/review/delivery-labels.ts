import type { RecordedState } from "../../api/generated/contracts/delivery";

export const deliveryLabels = {
  queued: "Queued",
  dispatching: "Delivering",
  started: "Delivered",
  uncertain: "Delivery uncertain",
  rejected: "Rejected",
} as const;

export function pendingDelivery(state: RecordedState) {
  return state === "queued" || state === "dispatching";
}
