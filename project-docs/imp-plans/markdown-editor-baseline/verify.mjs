import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const read = async (name) => JSON.parse(await readFile(new URL(name, import.meta.url), 'utf8'));
const [fixtures, evidence, runtime] = await Promise.all(['fixtures.json', 'evidence.json', 'runtime.json'].map(read));
assert.deepEqual(fixtures.acceptance.map(row => row.id), Array.from({ length: 22 }, (_, index) => `P${String(index + 1).padStart(2, '0')}`));
assert(fixtures.acceptance.every(row => row.procedure && row.latheStatus === 'pending implementation'));
assert.equal(runtime.sourceCommit, '033c6242cf96615fd0c88bed73d5428b76e3a44b');
assert.equal(evidence.vditor, '3.11.2');
assert(evidence.hostEqual);
assert.equal(Object.keys(evidence.comparison).length, 7);
assert(Object.values(evidence.comparison).every(row => row.equal && row.publishedSha256 === row.sourceSha256));
assert.deepEqual(evidence.baselines.published.roundTrips, evidence.baselines['pinned-source'].roundTrips);
assert.equal(fixtures.roundTrips.find(row => row.id === 'crlf').markdown.split('\r\n').length, 4);
assert(!fixtures.roundTrips.find(row => row.id === 'no-final-newline').markdown.endsWith('\n'));
assert(fixtures.roundTrips.find(row => row.id === 'unicode').markdown.includes('\u{1f600}'));
assert.deepEqual(fixtures.fences.filter(row => row.enabled).map(row => row.language).sort(), ['abc', 'echarts', 'flowchart', 'graphviz', 'markmap', 'math', 'mermaid', 'mindmap', 'plantuml', 'smiles']);
assert.deepEqual(fixtures.fences.filter(row => !row.enabled).map(row => row.language).sort(), ['sequence', 'wavedrom']);
for (const baseline of Object.values(evidence.baselines)) {
  assert.equal(baseline.roundTrips.length, fixtures.roundTrips.length * 3);
  for (const fixture of fixtures.roundTrips) {
    for (const mode of ['ir', 'wysiwyg', 'sv']) {
      const observation = baseline.roundTrips.find(row => row.id === fixture.id && row.mode === mode);
      assert(observation);
      assert.equal(observation.input, fixture.markdown);
      assert.equal(observation.identical, observation.input === observation.output);
      assert.equal(typeof observation.html, 'string');
    }
  }
  assert.equal(baseline.renderers.length, fixtures.fences.length * 2);
  const mindmap = baseline.renderers.find(row => row.language === 'mindmap' && row.validity === 'valid');
  const mindmapSource = JSON.parse(decodeURIComponent(mindmap.html.match(/data-code="([^"]+)"/)[1]));
  assert.equal(mindmapSource.name, 'Root');
  assert.equal(mindmapSource.children[0].name, 'Child');
  assert.deepEqual(mindmap.mindmapTree, [mindmapSource]);
  assert.equal(mindmap.canvas, 1);
  assert.equal(mindmap.processed, 1);
  assert.deepEqual(mindmap.errors, []);
  assert.deepEqual(mindmap.pageErrors, []);
  for (const fixture of fixtures.fences) {
    for (const validity of ['valid', 'invalid']) {
      const observation = baseline.renderers.find(row => row.language === fixture.language && row.validity === validity);
      assert(observation);
      assert.equal(observation.enabled, fixture.enabled);
    }
  }
  assert(baseline.toolbar.includes('find') && baseline.toolbar.includes('table-wrap'));
  assert.equal(baseline.options.toolbar.find(item => item.name === 'redo').hotkey, '\u2318Y');
  assert(!baseline.toolbar.includes('record') && !baseline.toolbar.includes('export'));
  assert(baseline.interactions.findOpen && baseline.interactions.tableTogglePreservedSource);
  assert.equal(baseline.interactions.modeSwitches.length, 3);
  assert(baseline.htmlPaste.output.includes('**bold** [link](https://example.com)'));
  assert.equal(baseline.options.preview.math.engine, 'KaTeX');
  assert(baseline.options.preview.math.inlineDigit);
  assert.equal(baseline.options.preview.markdown.toc, false);
  assert(baseline.requests.some(request => request.startsWith('https://www.plantuml.com/plantuml/svg/')));
}
assert(runtime.rendererAssets.every(asset => /^[0-9a-f]{64}$/.test(asset.sha256) && asset.bytes > 0));
assert.equal(runtime.contentThemes.length, 4);
assert(runtime.codeThemes.length > 100);
assert.equal(evidence.manifest.keybindings[0].key, fixtures.keys.open);
assert.equal(fixtures.keys.redo, 'ctrl+y / cmd+y');
for (const [setting, value] of Object.entries(fixtures.settings)) {
  assert.equal(evidence.manifest.configuration.properties[`markdown-editor.${setting}`].default, value);
}
console.log('Verified P01–P22 coverage, published/source correspondence, 60 round trips, 48 renderer observations, settings, keys, and runtime inventory');
