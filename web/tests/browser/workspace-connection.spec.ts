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
  const failReads = holdReads();
  await page.route("**/api/v1/**", async (route) => {
    const { pathname } = new URL(route.request().url());
    // This test controls snapshot availability independently of the event feed.
    if (pathname === "/api/v1/events") return;
    await failReads.promise;
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
  const title = page.getByRole("heading", { name: "Review queue", level: 1 });
  const newReview = page.getByRole("button", { name: "New review" });
  const drafts = page.getByRole("button", { name: "Unsent drafts" });
  const filters = page.locator(".queue-filters");
  const warning = page.getByRole("button", { name: "Workspace connection warning" });
  await expect(filters).toBeVisible();
  const layout = () =>
    Promise.all([title, newReview, drafts, filters].map((item) => item.boundingBox()));
  const before = await layout();

  failReads.release();
  await expect(warning).toHaveCount(1);
  expect(await layout()).toEqual(before);
  await newReview.focus();
  await page.keyboard.press("Shift+Tab");
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
  await expect(title).toBeFocused();
});

test.describe("phone detail", () => {
  test.use({ viewport: { width: 320, height: 844 }, hasTouch: true });

  test("a loading review reserves the warning slot before Close", async ({ page }) => {
    const failReads = holdReads();
    await page.route("**/api/v1/**", async (route) => {
      const { pathname } = new URL(route.request().url());
      if (pathname === "/api/v1/events" || pathname.startsWith("/api/v1/reviews/")) return;
      await failReads.promise;
      await route.abort("connectionreset");
    });

    await page.goto("/?reviewId=review-opening");
    const close = page.getByRole("button", { name: "Close review", exact: true });
    await expect(close).toBeVisible();
    const before = await close.boundingBox();
    expect(before).not.toBeNull();

    failReads.release();
    const warning = page.getByRole("button", { name: "Workspace connection warning" });
    await expect(warning).toHaveCount(1);
    expect(await close.boundingBox()).toEqual(before);
    const slot = await warning.boundingBox();
    expect(slot?.width).toBe(44);
    expect((slot?.x ?? 0) + (slot?.width ?? 0)).toBeLessThan(before?.x ?? 0);
    await close.focus();
    await page.keyboard.press("Shift+Tab");
    await expect(warning).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(close).toBeFocused();

    await warning.click();
    const details = page.getByRole("dialog", { name: "Workspace connection" });
    await expect(details).toBeVisible();
    const popup = await details.boundingBox();
    expect(popup?.x).toBeGreaterThanOrEqual(0);
    expect((popup?.x ?? Infinity) + (popup?.width ?? 0)).toBeLessThanOrEqual(320);
  });
});

function holdReads() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
