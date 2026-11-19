import { queryString, read, write } from "./client";
import type { LoadSprintsRequest, NotificationStateRequest, TrackReviewRequest } from "./generated/contracts";
import {
  isArchivePageResponse, isLoadSprintsResponse, isNotificationPageResponse, isNotificationStateResponse,
  isQueuePageResponse, isQueueSyncResponse, isRefreshResponse, isRunCardsResponse, isTrackReviewResponse,
} from "./generated/validators.mjs";

type Params = Parameters<typeof queryString>[0];

export function readQueue(params: Params, signal: AbortSignal) {
  return read(`/queue${queryString(params)}`, isQueuePageResponse, signal);
}

export function readQueueSync(signal: AbortSignal) {
  return read("/queue/sync", isQueueSyncResponse, signal);
}

export function refreshQueue() {
  return write("/queue/refresh", "POST", undefined, isRefreshResponse);
}

export function loadSprints(body: LoadSprintsRequest) {
  return write("/queue/sprints", "POST", body, isLoadSprintsResponse);
}

export function trackReview(body: TrackReviewRequest) {
  return write("/tracking", "POST", body, isTrackReviewResponse);
}

export function readRuns(params: Params, signal: AbortSignal) {
  return read(`/runs${queryString(params)}`, isRunCardsResponse, signal);
}

export function readArchive(params: Params, signal: AbortSignal) {
  return read(`/archive${queryString(params)}`, isArchivePageResponse, signal);
}

export function readNotifications(params: Params, signal: AbortSignal) {
  return read(`/notifications${queryString(params)}`, isNotificationPageResponse, signal);
}

export function assignNotifications(body: NotificationStateRequest) {
  return write("/notifications/state", "POST", body, isNotificationStateResponse);
}
