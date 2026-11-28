// The screens of the user guide. One entry per image: `name` becomes `docs/guide/images/<lang>/<name>.webp`.
//   query   extra mock-mode URL parameters (`?screen=...` and friends, see src/App.tsx and src/api/mock.ts)
//   ready   selector to wait for before the shot
//   act     async (page) => {} to open a menu or click a tab first
//   height  taller window for a long form (default: the fixed 1280x800)
//   dialog  true: capture the open dialog only (cropped to its box) instead of the whole window
// Keep the list in step with the pages that show the image (see the README in this folder).

const tab = (id) => async (page) => {
  await page.getByTestId(`bot-panel-tab-${id}`).click();
  await page.waitForTimeout(400);
};
const composerMenu = (text) => async (page) => {
  await page.locator('[data-composer-input]').fill(text);
  await page.getByTestId('composer-menu').waitFor({ timeout: 15000 });
};

export const SCREENS = [
  // Chat and composer
  { name: 'chat', query: '', ready: '[data-testid="bot-chip"]' },
  { name: 'composer-commands', query: 'screen=empty', act: composerMenu('/') },
  { name: 'composer-model', query: 'screen=empty', ready: '[data-testid="model-picker"]', act: async (page) => {
    await page.getByTestId('model-picker').click();
    await page.waitForTimeout(500);
  } },
  { name: 'approval', query: 'screen=approval', ready: '[data-testid^="approval"], [class*="approval"]' },
  // First run
  { name: 'first-run', query: 'screen=onboarding', ready: '[data-testid="onboarding-view"]' },
  // The bot panel, one image per tab
  { name: 'bot-panel-activity', query: 'panel=1', ready: '[data-testid="bot-panel"]' },
  { name: 'bot-panel-automation', query: 'panel=1', ready: '[data-testid="bot-panel"]', act: tab('automation') },
  { name: 'bot-panel-library', query: 'panel=1', ready: '[data-testid="bot-panel"]', act: tab('library') },
  // Other routes
  { name: 'automations', query: 'screen=schedules', ready: '#root > *' },
  { name: 'new-schedule', height: 1180, query: 'screen=schedules', ready: '[data-testid="schedules-view-list"]', act: async (page) => {
    await page.getByRole('button', { name: /^(New schedule|Yeni zamanlama)/ }).click();
    await page.waitForTimeout(500);
  } },
  {
    name: 'schedule-standing',
    height: 1280,
    query: 'screen=schedules',
    ready: '[data-testid=schedule-standing-fields]',
    act: async (page) => {
      await page.getByRole('button', { name: /^(New schedule|Yeni zamanlama)/ }).click();
      await page.getByTestId('schedule-task-kind-standing').click();
      await page.waitForTimeout(400);
    }
  },
  { name: 'notifications', query: 'screen=notifications', ready: '#root > *' },
  { name: 'live-view', query: 'lv=open', ready: '[data-testid="live-view-takeover"]' },
  { name: 'computer-menu', query: 'screen=empty', ready: '[data-testid="computer-picker"]', act: async (page) => {
    await page.getByTestId('computer-picker').click();
    await page.getByTestId('computer-add').waitFor({ timeout: 15000 });
  } },
  // Dialogs
  { name: 'new-bot', query: 'screen=newbot', ready: '[role="dialog"]', dialog: true },
  { name: 'bot-editor', query: 'screen=botedit', ready: '[role="dialog"]', dialog: true },
  { name: 'settings-general', query: 'screen=settings&section=general', ready: '[role="dialog"]', dialog: true },
  { name: 'settings-models', query: 'screen=settings&section=models', ready: '[role="dialog"]', dialog: true },
  { name: 'settings-safety', query: 'screen=settings&section=safety', ready: '[role="dialog"]', dialog: true },
  // Apps
  { name: 'apps/apps-main', query: 'screen=apps', ready: '[role="dialog"]', dialog: true },
  { name: 'apps/apps-advanced', query: 'screen=apps', ready: '[data-testid="apps-advanced"]', dialog: true, act: async (page) => {
    await page.getByTestId('apps-advanced').click();
    await page.waitForTimeout(400);
  } },
  { name: 'apps/apps-manage', query: 'screen=apps', ready: '[data-testid="apps-advanced"]', dialog: true, act: async (page) => {
    await page.getByTestId('apps-advanced').click();
    await page.getByTestId('mcp-manage-gmail').click();
    await page.getByTestId('mcp-server-gmail').evaluate((el) => el.scrollIntoView({ block: 'start' }));
    await page.waitForTimeout(400);
  } },
  { name: 'apps/apps-error', query: 'screen=apps', ready: '[data-testid="apps-advanced"]', dialog: true, act: async (page) => {
    await page.getByTestId('apps-advanced').click();
    await page.getByTestId('mcp-fix-sqlite').click();
    await page.getByTestId('mcp-error-toggle-sqlite').click();
    await page.getByTestId('mcp-error-sqlite').scrollIntoViewIfNeeded();
    await page.waitForTimeout(400);
  } }
];
