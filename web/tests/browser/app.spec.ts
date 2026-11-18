import { expect, test } from "@playwright/test";

test("mounts the application from the browser entry point", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "ReviewQ", level: 1 })).toBeVisible();
});
