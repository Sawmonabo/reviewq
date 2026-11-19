import { read, write } from "./client";
import type {
  CatalogRequest, CheckoutSkillsRequest, DisconnectSettings, FollowupPreferencesRequest, SaveRecoveryPreferencesRequest, SaveSettings, TestConnection,
} from "./generated/contracts";
import {
  isCatalogResponse, isCheckoutSkillsResponse, isFollowupPreferencesResponse, isRecoveryPreferencesResponse, isSettingsResponse, isVerifiedConnectionResponse,
} from "./generated/validators.mjs";

export function readSettings(signal: AbortSignal) {
  return read("/settings", isSettingsResponse, signal);
}

export function testConnection(body: TestConnection, signal: AbortSignal) {
  return write("/settings/test", "POST", body, isVerifiedConnectionResponse, signal);
}

export function browseCatalog(body: CatalogRequest, signal: AbortSignal) {
  return write("/settings/catalog", "POST", body, isCatalogResponse, signal);
}

export function saveSettings(body: SaveSettings) {
  return write("/settings", "PUT", body, isSettingsResponse);
}

export function disconnectSettings(body: DisconnectSettings) {
  return write("/settings/disconnect", "POST", body, isSettingsResponse);
}

export function readRecoveryPreference(signal: AbortSignal) {
  return read("/settings/agents/recovery", isRecoveryPreferencesResponse, signal);
}

export function saveRecoveryPreference(body: SaveRecoveryPreferencesRequest) {
  return write("/settings/agents/recovery", "PUT", body, isRecoveryPreferencesResponse);
}

export function readFollowupPreference(signal: AbortSignal) {
  return read("/followups/preferences", isFollowupPreferencesResponse, signal);
}

export function saveFollowupPreference(body: FollowupPreferencesRequest) {
  return write("/followups/preferences", "PUT", body, isFollowupPreferencesResponse);
}

export async function checkoutSkills(body: CheckoutSkillsRequest, signal: AbortSignal) {
  const result = await write("/settings/checkout/skills", "POST", body, isCheckoutSkillsResponse, signal);
  if (result.data.projectId !== body.projectId || result.data.provider !== body.provider || result.data.settingsRevision !== body.expectedRevision) {
    throw new Error("The checkout skill catalog changed.");
  }
  return result;
}
