import { expect, test } from "@playwright/test";

test("Back and Forward restore the workspace opened from the home URL", async ({ page }) => {
  await page.goto("/");
  const queue = page.getByRole("heading", { name: "Review queue", level: 1 });
  const runs = page.getByRole("heading", { name: "Review runs", level: 1 });
  await expect(queue).toBeVisible();

  await page.getByRole("button", { name: "Review runs", exact: true }).click();
  await expect(runs).toBeVisible();
  await page.goBack();
  await expect(queue).toBeVisible();
  await page.goForward();
  await expect(runs).toBeVisible();
});
