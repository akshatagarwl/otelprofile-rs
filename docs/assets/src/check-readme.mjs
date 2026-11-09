import {chromium} from 'playwright-core';

const browser = await chromium.launch({channel: 'chrome', headless: true});
try {
  const page = await browser.newPage({viewport: {width: 1440, height: 1020}, deviceScaleFactor: 1});
  await page.goto('https://github.com/akshatagarwl/otelprofile-rs', {waitUntil: 'domcontentloaded', timeout: 60000});
  const images = page.locator('article.markdown-body').first().locator('img');
  await images.first().waitFor();
  await page.waitForFunction(() => [...document.querySelectorAll('article.markdown-body img')]
    .every(img => img.complete && img.naturalWidth > 0), null, {timeout: 60000});
  const loaded = await images.evaluateAll(nodes => nodes.map(n => ({
    alt: n.alt, src: n.currentSrc, width: n.naturalWidth, height: n.naturalHeight,
  })));
  if (loaded.length !== 7) throw Error(`Expected seven README images, got ${loaded.length}`);
  if (loaded.some(img => img.width < 900)) throw Error('An image is missing its readable intrinsic width');
  await page.waitForTimeout(4000);
  await page.mouse.move(0, 0);
  const first = await images.first().screenshot();
  await page.waitForTimeout(1100);
  const second = await images.first().screenshot();
  if (first.equals(second)) throw Error('README flow animation did not change between captures');
  console.log(JSON.stringify({url: page.url(), images: loaded, animation_changed_pixels: true}, null, 2));
} finally {
  await browser.close();
}
