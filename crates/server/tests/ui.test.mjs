import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';

// Exercise UI handlers with a minimal DOM and observable HTTP requests. This
// checks actions and escaping; it does not replace full-browser layout testing.
class Element {
  constructor(tag = 'div') {
    this.tagName = tag.toUpperCase(); this.children = []; this.textContent = '';
    this.value = ''; this.style = {}; this.attributes = {}; this.hidden = false;
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  setAttribute(name, value) { this.attributes[name] = value; }
  removeAttribute(name) { delete this.attributes[name]; }
  scrollIntoView() { this.scrolled = true; }
  focus() { this.focused = true; }
}
function descendants(node) { return [node, ...node.children.flatMap(descendants)]; }
function fixture() {
  const html = readFileSync(new URL('../src/index.html', import.meta.url), 'utf8');
  const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
  const ids = [...html.matchAll(/id="([^"]+)"/g)].map(match => match[1]);
  const elements = new Map(ids.map(id => [id, new Element()]));
  const hash = 'aa'.repeat(32);
  const first = {insertion: {patch: Array(32).fill(0xaa), ordinal: 0}, ordinal: 0};
  const second = {insertion: {patch: Array(32).fill(0xbb), ordinal: 0}, ordinal: 0};
  const operation = {kind: 'insert', left: 'Start', right: 'End', atoms: [[60, 120, 62]]};
  const state = {
    repository: {patches: {'1': {
      id: 1, hash, author: 2, intent: 'Concurrent graph text', dependencies: [], edits: [],
      graph_edits: [{path: 'src/graph.txt', operations: [operation]}], approvals: [], checks_passed: false
    }}, channels: {main: [1]}, events: []},
    view: {patches: [1], files: {}, conflicts: {'src/graph.txt': [{patch: 1, content: null}]}}
  };
  const graph = {
    path: 'src/graph.txt', layers: [[first, second]], nodes: [
      {id: first, bytes: [60, 120, 62]}, {id: second, bytes: [121]}
    ], conflicts: [{kind: 'order', frontier: [first, second]}], bytes: null
  };
  const requests = [], copied = [];
  const body = new Element('body');
  const context = vm.createContext({
    document: {body, getElementById: id => elements.get(id), createElement: tag => new Element(tag), createElementNS: (_, tag) => new Element(tag), addEventListener() {}},
    navigator: {clipboard: {writeText: async text => copied.push(text)}},
    TextDecoder, TextEncoder, Uint8Array,
    fetch: async (path, options) => {
      requests.push({path, options});
      const reply = path.startsWith('/api/document?') ? graph : path.startsWith('/api/state?') ? state : {patch: 9, hash};
      return {ok: true, json: async () => reply};
    }
  });
  vm.runInContext(script, context);
  elements.get('repositories').value = 'demo'; elements.get('channel').value = 'main'; elements.get('filter').value = 'all';
  context.fixtureState = state;
  vm.runInContext('data = fixtureState; actor = {tenant: 1, user: 3}; token = "test-token"; render();', context);
  return {context, elements, requests, copied, hash, graph};
}

test('graph proposals display stable hashes, actual operations and block IDs without fake deletion alternatives', async () => {
  const f = fixture();
  const cards = descendants(f.elements.get('patches'));
  assert.ok(cards.some(node => node.textContent === 'SHA-256 ' + f.hash));
  assert.ok(cards.some(node => node.textContent.includes('"kind": "insert"')));
  assert.ok(cards.some(node => node.textContent.includes('"insertion"')));
  const files = descendants(f.elements.get('files'));
  assert.ok(files.some(node => node.textContent === 'Inspect graph conflict'));
  assert.ok(!files.some(node => node.textContent === '[deleted]'));
  const build = cards.find(node => node.textContent === 'Build on graph patch');
  await build.onclick();
  assert.equal(f.elements.get('edit-mode').value, 'graph');
  assert.equal(f.elements.get('graph-operations').value, '[]');
  assert.equal(f.elements.get('path').value, 'src/graph.txt');
  assert.equal(f.elements.get('deps').value, '1');
  assert.equal(f.requests.length, 0);
});

test('document inspection exposes exact conflict frontiers and copies node IDs', async () => {
  const f = fixture();
  await vm.runInContext('inspectDocument("src/graph.txt")', f.context);
  assert.match(f.requests[0].path, /path=src%2Fgraph.txt/);
  assert.equal(f.elements.get('document-panel').hidden, false);
  assert.equal(f.elements.get('document-panel').focused, true);
  const content = descendants(f.elements.get('document-details'));
  assert.ok(content.some(node => node.textContent.includes('Concurrent blocks')));
  assert.ok(content.some(node => node.textContent.includes('order is unresolved')));
  assert.ok(content.some(node => node.textContent === '<x>')); // textContent, never HTML
  await content.find(node => node.textContent === 'Copy node ID').onclick();
  assert.deepEqual(JSON.parse(f.copied[0]), f.graph.nodes[0].id);
  await content.find(node => node.textContent === 'Compose graph operations').onclick();
  assert.equal(f.elements.get('graph-operations').value, '[]');
});

test('graph composer submits an explicit reviewed operation envelope and refuses malformed JSON', async () => {
  const f = fixture();
  vm.runInContext('composeGraph("src/graph.txt", [1], "Graph resolution")', f.context);
  f.elements.get('intent').value = 'Establish reviewed order';
  f.elements.get('graph-operations').value = '{invalid';
  await f.elements.get('proposal').onsubmit({preventDefault() {}});
  assert.equal(f.requests.length, 0);
  assert.match(f.elements.get('status').textContent, /valid JSON/);
  const operation = {kind: 'order', before: f.graph.nodes[0].id, after: f.graph.nodes[1].id};
  f.elements.get('graph-operations').value = JSON.stringify([operation]);
  await f.elements.get('proposal').onsubmit({preventDefault() {}});
  const request = f.requests.find(request => request.path === '/api/command');
  assert.deepEqual(JSON.parse(request.options.body), {
    repo: 'demo', cmd: 'propose_graph', intent: 'Establish reviewed order', dependencies: [1],
    edits: [{path: 'src/graph.txt', operations: [operation]}]
  });
  assert.equal(request.options.headers.Authorization, 'Bearer test-token');
});
