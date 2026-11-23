import { expect, test } from "@playwright/test";
import type { QueuePage } from "../../src/api/generated/contracts";

const emptyQueue: QueuePage = {
  items: [],
  nextCursor: null,
  settingsRevision: "1",
  totalMrs: 0,
  totalRows: 0,
  facets: {
    authors: [],
    boards: [],
    projects: [],
    sprints: [],
    hideReviewed: 0,
    hideInProgress: 0,
  },
  sync: {
    mode: "connected",
    active: false,
    stale: false,
    lastAttemptAt: null,
    lastSuccessAt: null,
    error: null,
  },
};

test("simultaneous workspace read failures share one recoverable warning", async ({ page }) => {
  let unreachable = true;
  await page.route("**/api/v1/**", async (route) => {
    const { pathname } = new URL(route.request().url());
    // This test controls snapshot availability independently of the event feed.
    if (pathname === "/api/v1/events") return;
    if (unreachable) await route.abort("connectionreset");
    else if (pathname === "/api/v1/queue") {
      await route.fulfill({ json: { cursor: "1", data: emptyQueue } });
    } else {
      await route.fulfill({
        status: 404,
        json: {
          cursor: null,
          error: { code: "not_found", message: "No data for this fixture.", retryable: false },
        },
      });
    }
  });

  await page.goto("/");
  const warning = page.getByRole("button", { name: "Workspace connection warning" });
  await expect(warning).toHaveCount(1);
  await page.getByRole("button", { name: "Unsent drafts" }).focus();
  await page.keyboard.press("Tab");
  await expect(warning).toBeFocused();
  await page.keyboard.press("Enter");
  const details = page.getByRole("dialog", { name: "Workspace connection" });
  await expect(details.getByText("Review queue", { exact: true })).toBeVisible();
  await expect(details.getByText("Review filters", { exact: true })).toBeVisible();
  await expect(details.locator("time")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(warning).toBeFocused();
  await expect(details).toBeHidden();

  unreachable = false;
  await expect(page.getByText("No reviews match these filters")).toBeVisible();
  await expect(warning).toHaveCount(0);
});
