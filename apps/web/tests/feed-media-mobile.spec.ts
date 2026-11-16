import {expect, test, type Page} from '@playwright/test';
import type {FeedMedia} from '../components/feed/types';

const ids = [1, 2, 3].map(n => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`);
const event = {
  id: '00000000-0000-4000-8000-000000000004', provider: 'instagram',
  profile_name: '一个名字很长的朋友 Long profile name', title: 'Instagram 动态',
  summary: '这些照片后面是正文，滚动时不应该叠在图片上。', occurred_at: '2026-11-15T08:30:00Z', cursor: 'end',
  media: ids.map((id, i) => ({id, media_type: 'image/jpeg', width: i === 1 ? 1200 : 600, height: i === 1 ? 600 : 1200, ready: true})),
  event_metadata: {event_type: 'story', locations: [], mentions: []},
};

async function feed(page: Page, media: FeedMedia[] = event.media) {
  await page.route('**/api/v1/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/v1/timeline') return route.fulfill({json: [{...event, media}]});
    if (path === `/api/v1/timeline/${event.id}`) return route.fulfill({json: {...event, media, content: {caption: event.summary}}});
    if (path.startsWith('/api/v1/media/')) {
      const i = ids.indexOf(path.split('/').at(-1)!);
      if (i < 0) return route.fulfill({status: 404});
      const image = event.media[i];
      return route.fulfill({contentType: 'image/svg+xml', body: `<svg xmlns="http://www.w3.org/2000/svg" width="${image.width}" height="${image.height}"><rect width="100%" height="100%" fill="#b0c6d5"/></svg>`});
    }
    return route.fulfill({json: {enabled: false, people: [], faces: []}});
  });
  await page.emulateMedia({reducedMotion: 'reduce'});
  await page.goto('/timeline');
}

test('source slots remain visible and a failed thumbnail falls back to its registered original', async ({page}) => {
  await page.setViewportSize({width: 390, height: 844});
  await feed(page, [{media_type: 'image', ready: false}, {...event.media[0], thumbnail_id: '00000000-0000-4000-8000-999999999999'}]);
  await expect(page.locator('article [data-thumb]')).toContainText('暂时无法读取');
  await page.locator('article [data-thumb]').click();
  const image = page.getByRole('dialog', {name: '动态详情'}).getByAltText('动态照片 2');
  await expect(image).toHaveAttribute('src', `/api/v1/media/${ids[0]}`);
  await expect.poll(() => image.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
  expect(await page.getByRole('group', {name: '动态来源'}).getByRole('button').evaluateAll(buttons =>
    buttons.every(button => button.getBoundingClientRect().height >= 44))).toBeTruthy();
});

test('detail preserves missing and failed slots in their original order alongside real photos', async ({page}) => {
  await feed(page, [{media_type: 'image', ready: false}, {id: '00000000-0000-4000-8000-999999999999', media_type: 'image', ready: true}, ...event.media]);
  await page.locator('article [data-thumb]').click();
  const dialog = page.getByRole('dialog', {name: '动态详情'});
  await expect(dialog.getByAltText(/^动态照片/)).toHaveCount(3);
  await expect(dialog.getByText('动态照片 1 · 暂时无法读取', {exact: true})).toBeVisible();
  await expect(dialog.getByText('动态照片 2 · 暂时无法读取', {exact: true})).toBeVisible();
  await expect(dialog.getByText(/已从图片列表隐藏/)).toHaveCount(0);
  await expect(dialog.getByAltText('动态照片 3')).toHaveAttribute('src', `/api/v1/media/${ids[0]}`);
});

test('mobile photos and text never overlap and the close button stays reachable', async ({page}) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.setViewportSize({width: 390, height: 844});
  await feed(page);
  await page.locator('article [data-thumb]').click();
  const dialog = page.getByRole('dialog', {name: '动态详情'});
  const photos = dialog.getByAltText(/^动态照片/);
  await expect(photos).toHaveCount(3);
  await expect.poll(() => photos.evaluateAll(images => images.every(img => (img as HTMLImageElement).naturalWidth > 0))).toBeTruthy();
  expect(await dialog.evaluate(d => d.scrollTop)).toBe(0);
  await expect(dialog.getByRole('button', {name: '关闭动态详情'})).toBeInViewport();
  for (const [width, height] of [[390, 844], [320, 568], [667, 375]]) {
    await page.setViewportSize({width, height});
    const layout = await dialog.evaluate(d => {
      const rect = (e: Element) => {const r = e.getBoundingClientRect(); return {x: r.x, right: r.right, y: r.y, bottom: r.bottom};};
      return {dialog: rect(d), header: rect(d.querySelector('header')!), images: [...d.querySelectorAll('img')].map(rect), body: rect(d.querySelector('p')!), overflow: d.scrollWidth - d.clientWidth};
    });
    expect(layout.overflow).toBeLessThanOrEqual(1);
    expect(layout.header.bottom).toBeLessThanOrEqual(layout.images[0].y);
    for (const [i, image] of layout.images.entries()) {
      expect(image.x).toBeGreaterThanOrEqual(layout.dialog.x);
      expect(image.right).toBeLessThanOrEqual(layout.dialog.right);
      if (i) expect(image.y).toBeGreaterThanOrEqual(layout.images[i - 1].bottom);
    }
    expect(layout.body.y).toBeGreaterThanOrEqual(layout.images.at(-1)!.bottom);
    await dialog.evaluate(d => {d.scrollTop = d.scrollHeight;});
    await expect(dialog.getByRole('button', {name: '关闭动态详情'})).toBeInViewport();
    await dialog.evaluate(d => {d.scrollTop = 0;});
  }
  await dialog.getByRole('button', {name: '关闭动态详情'}).click();
  await expect(dialog).toHaveCount(0);
  await page.setViewportSize({width: 1440, height: 1000});
  await page.locator('article [data-thumb]').click();
  const desktop = await dialog.evaluate(d => {
    const photo = d.querySelector('img')!.getBoundingClientRect();
    const text = d.querySelector('p')!.getBoundingClientRect();
    return {photoRight: photo.right, textLeft: text.left};
  });
  expect(desktop.textLeft).toBeGreaterThanOrEqual(desktop.photoRight);
  expect(errors).toEqual([]);
});

test('photo details keep a usable viewport while the original loads and the mobile viewport changes', async ({page}) => {
  const profile = {id: ids[2], profile_type: 'person', display_name: '照片布局测试', fields: [], identities: []};
  const photo = {media_id: ids[0], occurred_at: event.occurred_at, width: null, height: null, score: 0.9,
    pending: true, confirmed: false, sources: [],
    faces: [{id: ids[1], media_id: ids[0], box: [0.2, 0.1, 0.3, 0.3], status: 'pending', score: 0.9, profile_id: profile.id, name: profile.display_name}]};
  let release!: () => void;
  const original = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/v1/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === `/api/v1/profiles/${profile.id}/summary`) return route.fulfill({json: {profile, overview: {}, has_posts: false}});
    if (path === `/api/v1/profiles/${profile.id}`) return route.fulfill({json: profile});
    if (path.endsWith('/photos') || path.endsWith('/photos/status')) return route.fulfill({json: {items: [photo], next_cursor: null, counts: {all: 1, pending: 1, confirmed: 0}, revision: 'one'}});
    if (path.startsWith('/api/v1/media/')) {
      if (path === `/api/v1/media/${ids[0]}`) await original;
      return route.fulfill({contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" width="600" height="1200"><rect width="100%" height="100%" fill="#b0c6d5"/></svg>'});
    }
    return route.fulfill({json: {enabled: false, people: [], readings: []}});
  });
  await page.emulateMedia({reducedMotion: 'reduce'});
  await page.setViewportSize({width: 390, height: 844});
  try {
    await page.goto(`/profiles/${profile.id}?tab=photos`);
    await page.getByRole('button', {name: /^查看人脸/}).click();
    const dialog = page.getByRole('dialog', {name: '照片详情', exact: true});
    await expect(dialog).toBeVisible();
    expect((await dialog.boundingBox())!.height).toBeGreaterThan(844 * 0.65);
    release();
    await expect.poll(() => dialog.getByAltText('完整照片').evaluate((img: HTMLImageElement) => img.naturalWidth)).toBeGreaterThan(0);
    for (const height of [650, 844]) {
      await page.setViewportSize({width: 390, height});
      expect((await dialog.boundingBox())!.height).toBeGreaterThan(height * 0.65);
      await expect(dialog.getByAltText('完整照片')).toBeInViewport();
      await dialog.getByRole('heading', {name: '照片来源'}).scrollIntoViewIfNeeded();
      await expect(dialog.getByRole('button', {name: '关闭', exact: true})).toBeInViewport();
    }
    await dialog.getByRole('button', {name: '关闭', exact: true}).click();
    await expect(dialog).toHaveCount(0);
  } finally { release(); }
});
