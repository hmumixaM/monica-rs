import {test, expect} from '@playwright/test';

test('dates do not wait for insights or suggestions and failed reads can be retried', async ({page}) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  let fail = false;
  let release!: () => void;
  const suggestions = new Promise<void>(resolve => { release = resolve; });
  let releaseInsights!: () => void;
  let blockInsights = true;
  const insights = new Promise<void>(resolve => { releaseInsights = resolve; });
  await page.route('**/api/v1/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/reconnect') {
      await suggestions;
      return route.fulfill({json: {enabled: false, people: []}});
    }
    if (path === '/api/v1/insights') {
      if (blockInsights) await insights;
      return route.fulfill(fail
      ? {status: 503, json: {detail: 'Synthetic read failure'}}
      : {json: {dormant_contacts: [], upcoming_dates: []}});
    }
    if (path === '/api/v1/home/dates') return route.fulfill(fail
      ? {status: 503, json: {detail: 'Synthetic date failure'}}
      : {json: {upcoming_dates: [{profile_id: 'synthetic-person', name: 'Synthetic birthday', kind: 'birthday', date: '2026-11-16', days_until: 1}]}});
    if (path === '/api/v1/insights/calendar') return route.fulfill({json: {year: 2026, years: [2026], days: [], max: 0}});
    if (path === '/api/v1/timeline') return route.fulfill({json: []});
    if (path === '/api/v1/metrics/social') return route.fulfill({json: {providers: {}}});
    return route.fulfill({json: {enabled: false, people: [], digests: [], key_numbers: {contacts: 1, groups: 0, active_contacts_30d: 0, messages_30d: 0}}});
  });
  try {
    await page.goto('/dashboard');
    await expect(page.getByText('Synthetic birthday', {exact: true})).toBeVisible();
  } finally { release(); blockInsights = false; releaseInsights(); }

  fail = true;
  await page.goto('/insights?dormant=180');
  const error = page.getByRole('alert').filter({hasText: '暂时无法读取联系情况和重要日子。'});
  await expect(error).toBeVisible({timeout: 10000});
  fail = false;
  await error.getByRole('button', {name: '重试', exact: true}).click();
  await expect(page.getByRole('heading', {name: '重要日子', exact: true})).toBeVisible();
  await expect(error).toHaveCount(0);

  fail = true;
  await page.goto('/dashboard');
  await page.reload();
  await expect(page.getByRole('alert').filter({hasText: '暂时无法读取重要日子。'})).toBeVisible({timeout: 10000});
  expect(errors).toEqual([]);
});
