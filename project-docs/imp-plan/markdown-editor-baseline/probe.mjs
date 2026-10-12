import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const directory = dirname(fileURLToPath(import.meta.url));
const [published, source, executable, output = join(directory, 'actual.json')] = process.argv.slice(2);
assert(published && source && executable, 'Usage: node probe.mjs <installed-extension> <pinned-source> <Chromium-executable> [output.json]');
const corpus = JSON.parse(await readFile(join(directory, 'fixtures.json'), 'utf8'));
assert.deepEqual(corpus.acceptance.map(row => row.id), Array.from({length: 22}, (_, index) => `P${String(index + 1).padStart(2, '0')}`));
assert(corpus.roundTrips.find(fixture => fixture.id === 'unicode').markdown.includes('\u{1f600}'));
const runtime = join(source, 'media-src/node_modules/vditor');
const digest = (bytes) => createHash('sha256').update(bytes).digest('hex');
const mime = (path) => ({ '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.wasm': 'application/wasm' }[path.slice(path.lastIndexOf('.'))] || 'application/octet-stream');
const server = createServer(async (request, response) => {
  try {
    const pathname = new URL(request.url, 'http://localhost').pathname;
    if (pathname === '/') {
      response.setHeader('Content-Type', 'text/html');
      response.end('<html><head><link rel="stylesheet" href="/main.css"></head><body data-vmd-css-loaded="1"><div id="app"></div><script>window.messages=[];window.acquireVsCodeApi=()=>({postMessage:message=>messages.push(message)});</script><script src="/main.js"></script></body></html>');
    } else {
      const path = join(server.assetRoot, pathname.slice(1));
      assert(resolve(path).startsWith(resolve(server.assetRoot) + sep));
      response.setHeader('Content-Type', mime(path));
      response.end(await readFile(path));
    }
  } catch (error) {
    response.statusCode = 404;
    response.end(String(error));
  }
});
await new Promise((resolveListening) => server.listen(0, '127.0.0.1', resolveListening));
const browser = await chromium.launch({ executablePath: executable, headless: true });
const result = { browser: browser.version(), vditor: JSON.parse(await readFile(join(runtime, 'package.json'), 'utf8')).version, comparison: {}, baselines: {} };
try {
  const map = JSON.parse(await readFile(join(published, 'media/dist/main.js.map'), 'utf8'));
  for (const [index, name] of map.sources.entries()) {
    if (!name.includes('media-src/src/')) continue;
    const relative = name.slice(name.indexOf('media-src/src/'));
    const installedText = map.sourcesContent[index].replaceAll('\r\n', '\n');
    const sourceText = (await readFile(join(source, relative), 'utf8')).replaceAll('\r\n', '\n');
    result.comparison[relative] = { equal: installedText === sourceText, publishedSha256: digest(installedText), sourceSha256: digest(sourceText) };
  }
  result.publishedBundleSha256 = digest(await readFile(join(published, 'media/dist/main.js')));
  result.hostEqual = (await readFile(join(published, 'out/extension.js'), 'utf8')).replaceAll('\r\n', '\n') === (await readFile(join(source, 'out/extension.js'), 'utf8')).replaceAll('\r\n', '\n');
  result.manifest = JSON.parse(await readFile(join(published, 'package.json'), 'utf8')).contributes;
  for (const [name, assetRoot] of [['published', join(published, 'media/dist')], ['pinned-source', join(source, 'media/dist')]]) {
    server.assetRoot = assetRoot;
    const context = await browser.newContext();
    const requests = new Set();
    await context.route('**/*', async (route) => {
      const url = new URL(route.request().url());
      if (url.hostname === '127.0.0.1') return route.continue();
      requests.add(url.href);
      if (url.hostname === 'unpkg.com' && url.pathname.startsWith('/vditor@3.11.2/')) {
        const path = join(runtime, decodeURIComponent(url.pathname.slice('/vditor@3.11.2/'.length)));
        assert(resolve(path).startsWith(resolve(runtime) + sep));
        try { return await route.fulfill({ body: await readFile(path), contentType: mime(path) }); }
        catch (error) { return route.abort(); }
      }
      return route.abort();
    });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForFunction(() => window.messages?.some(message => message.command === 'ready'));
    const initialize = async (content, mode = 'ir') => {
      await page.evaluate(({content, mode}) => {
        document.body.removeAttribute('data-vmd-ready');
        window.postMessage({ command: 'update', type: 'init', content, options: { mode, showLineNumbers: true, outline: { enable: false } }, theme: 'light' }, '*');
      }, {content, mode});
      await page.waitForFunction(() => document.body.getAttribute('data-vmd-ready') === '1');
    };
    await initialize('# Toolbar\n');
    const toolbar = await page.locator('.vditor-toolbar [data-type]').evaluateAll(elements => [...new Set(elements.map(element => element.dataset.type))]);
    const options = await page.evaluate(() => ({ mode: vditor.getCurrentMode(), preview: vditor.vditor.options.preview, toolbar: vditor.vditor.options.toolbar, methods: Object.getOwnPropertyNames(Object.getPrototypeOf(vditor)) }));
    const roundTrips = [];
    for (const fixture of corpus.roundTrips) {
      for (const mode of ['ir', 'wysiwyg', 'sv']) {
        await initialize(fixture.markdown, mode);
        const value = await page.evaluate(() => vditor.getValue());
        const html = await page.evaluate(() => vditor.getHTML());
        roundTrips.push({ id: fixture.id, mode, input: fixture.markdown, output: value, identical: value === fixture.markdown, html });
      }
    }
    await initialize('Paste here\n');
    const paste = await page.evaluate((html) => {
      vditor.focus();
      const transfer = new DataTransfer();
      transfer.setData('text/html', html);
      transfer.setData('text/plain', 'Pasted bold link');
      document.activeElement.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer }));
      return true;
    }, corpus.htmlPaste);
    await page.waitForTimeout(250);
    const pasteValue = await page.evaluate(() => vditor.getValue());
    assert.equal(errors.length, 0, `Unexpected initialization/round-trip errors in ${name}`);
    await initialize('Find find FIND\n\n| long cell | B |\n| --- | --- |\n| repeated repeated repeated | B |\n');
    const beforeInteraction = await page.evaluate(() => vditor.getValue());
    await page.keyboard.press('Control+f');
    const findOpen = await page.evaluate(() => getComputedStyle(document.querySelector('#vmd-search-bar')).display !== 'none');
    assert(findOpen);
    await page.locator('#vmd-search-input').fill('find');
    await page.waitForTimeout(200);
    const findCount = await page.locator('#vmd-search-count').textContent();
    await page.keyboard.press('Escape');
    await page.evaluate(() => document.querySelector('[data-type="table-wrap"]').click());
    const afterInteraction = await page.evaluate(() => ({ value: vditor.getValue(), nowrap: document.body.classList.contains('vmd-table-nowrap') }));
    assert.equal(afterInteraction.value, beforeInteraction);
    const modeSwitches = [];
    for (const mode of ['wysiwyg', 'sv', 'ir']) {
      await page.evaluate(mode => document.querySelector(`[data-mode="${mode}"]`).click(), mode);
      await page.waitForTimeout(150);
      modeSwitches.push(await page.evaluate(() => ({ mode: vditor.getCurrentMode(), value: vditor.getValue() })));
    }
    const renderers = [];
    for (const fixture of corpus.fences) {
      for (const validity of ['valid', 'invalid']) {
        await page.goto(`http://127.0.0.1:${server.address().port}/`);
        await page.waitForFunction(() => window.messages?.some(message => message.command === 'ready'));
        const errorStart = errors.length;
        await initialize(fixture[validity], 'sv');
        await page.waitForTimeout(1800);
        const rendered = await page.locator('.vditor-preview .vditor-reset').evaluate(element => ({
          svg: element.querySelectorAll('svg').length,
          canvas: element.querySelectorAll('canvas').length,
          objects: [...element.querySelectorAll('object')].map(object => object.data),
          errors: [...element.querySelectorAll('.vditor-reset--error')].map(error => error.textContent),
          processed: element.querySelectorAll('[data-processed="true"]').length,
          text: element.textContent.slice(0, 1000),
          html: element.innerHTML.slice(0, 2000),
          versions: { echarts: window.echarts?.version, katex: window.katex?.version, abcjs: window.ABCJS?.signature }
        }));
        renderers.push({ language: fixture.language, validity, enabled: fixture.enabled, ...rendered, pageErrors: errors.slice(errorStart) });
      }
    }
    result.baselines[name] = { toolbar, options, roundTrips, htmlPaste: { dispatched: paste, output: pasteValue }, interactions: { findOpen, findCount, tableTogglePreservedSource: true, modeSwitches }, renderers, requests: [...requests].sort(), errors };
    await context.close();
  }
  assert.equal(result.vditor, '3.11.2');
  assert.deepEqual(result.baselines.published.roundTrips, result.baselines['pinned-source'].roundTrips);
  assert(result.baselines.published.toolbar.includes('find'));
  assert(result.baselines.published.toolbar.includes('table-wrap'));
  await writeFile(output, JSON.stringify(result, null, 2) + '\n');
  console.log(`Recorded ${result.baselines.published.roundTrips.length} round trips per baseline in ${output}`);
} finally {
  await browser.close();
  await new Promise((resolveClosed) => server.close(resolveClosed));
}
