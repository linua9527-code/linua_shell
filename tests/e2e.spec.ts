import { test, expect } from '@playwright/test';

// Test credentials - Replace with your own test server credentials
const TEST_HOST = 'localhost'; // Replace with your test SSH server
const TEST_USERNAME = 'testuser'; // Replace with your test username
const TEST_PASSWORD = 'testpass'; // Replace with your test password

test.describe('r-shell E2E Tests', () => {
  test('should connect to SSH server and create connection', async ({ page }) => {
    // Navigate to the app
    await page.goto('http://localhost:1420');

    // Wait for app to load
    await page.waitForSelector('button:has-text("New Connection")', { timeout: 10000 });

    // Click new connection button
    await page.click('button:has-text("New Connection")');

    // Wait for connection dialog
    await page.waitForSelector('input[placeholder*="name" i]', { timeout: 5000 });

    // Fill in connection details
    await page.fill('input[placeholder*="name" i]', 'Test Server');
    await page.fill('input[placeholder*="host" i]', TEST_HOST);
    await page.fill('input[placeholder*="username" i]', TEST_USERNAME);
    await page.fill('input[type="password"]', TEST_PASSWORD);

    // Click connect button
    await page.click('button:has-text("Connect")');

    // Wait for connection to succeed and dialog to close
    await page.waitForSelector('button:has-text("Connect")', { state: 'hidden', timeout: 10000 });

    // Verify connection tab was created
    await expect(page.locator('text=Test Server')).toBeVisible();

    // Verify terminal is visible
    await expect(page.locator('.xterm-viewport')).toBeVisible();
  });

  test('should execute commands in terminal', async ({ page }) => {
    await page.goto('http://localhost:1420');

    // Connect to server
    await page.click('button:has-text("New Connection")');
    await page.fill('input[placeholder*="name" i]', 'Test Server');
    await page.fill('input[placeholder*="host" i]', TEST_HOST);
    await page.fill('input[placeholder*="username" i]', TEST_USERNAME);
    await page.fill('input[type="password"]', TEST_PASSWORD);
    await page.click('button:has-text("Connect")');
    await page.waitForSelector('button:has-text("Connect")', { state: 'hidden', timeout: 10000 });

    // Wait for terminal to be ready
    await page.waitForSelector('.xterm-viewport', { timeout: 5000 });
    await page.waitForTimeout(2000); // Wait for shell prompt

    // Type a command
    await page.keyboard.type('echo "E2E Test Success"');
    await page.keyboard.press('Enter');

    // Wait for command output
    await page.waitForTimeout(1000);

    // Verify output appears in terminal
    await expect(page.locator('text=E2E Test Success')).toBeVisible();
  });

  test('should save and load connection profile', async ({ page }) => {
    await page.goto('http://localhost:1420');

    // Open connection dialog
    await page.click('button:has-text("New Connection")');

    // Fill in connection details
    await page.fill('input[placeholder*="name" i]', 'Saved Profile');
    await page.fill('input[placeholder*="host" i]', TEST_HOST);
    await page.fill('input[placeholder*="username" i]', TEST_USERNAME);
    await page.fill('input[type="password"]', TEST_PASSWORD);

    // Save profile (look for save button or checkbox)
    const saveButton = page.locator('button:has-text("Save")').first();
    if (await saveButton.isVisible()) {
      await saveButton.click();
    }

    // Close dialog
    await page.keyboard.press('Escape');

    // Reopen connection dialog
    await page.click('button:has-text("New Connection")');

    // Verify saved profile appears
    await expect(page.locator('text=Saved Profile')).toBeVisible({ timeout: 2000 });
  });
});
