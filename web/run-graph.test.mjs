import test from 'node:test';
import assert from 'node:assert/strict';
import { layoutJobs, NODE_WIDTH, NODE_HEIGHT, graphScale, fitGraphScale, adjacentFailure } from './run-graph.js';

function job(name, needs = [], stage = 'build') {
  return { id: name, name, needs, stage, status: 'queued', shared_workspace: false,
    attempt_id: null, agent_name: null, reason: null, exit_code: null, cancel_requested: false };
}
const node = (layout, name) => layout.nodes.find(value => value.job.name === name);

test('failure navigation cycles actual failures and lost jobs, excluding blocked descendants', () => {
  const jobs = ['succeeded', 'failed', 'blocked', 'lost', 'skipped', 'running'].map((status, i) => ({...job(String(i)), status}));
  assert.equal(adjacentFailure(jobs, '0', 1), '1');
  assert.equal(adjacentFailure(jobs, '0', -1), '3');
  assert.equal(adjacentFailure(jobs, '1', 1), '3');
  assert.equal(adjacentFailure(jobs, '3', 1), '1');
  assert.equal(adjacentFailure(jobs, '1', -1), '3');
  assert.equal(adjacentFailure([jobs[1]], '1', 1), '1');
  assert.equal(adjacentFailure(jobs.filter(j => !['failed', 'lost'].includes(j.status)), '1', 1), undefined);
});

test('fit accounts for both axes and leaves a gutter without enlarging small graphs', () => {
  for (const [width, height, availableWidth, availableHeight] of [[4000, 900, 740, 500], [600, 2500, 390, 340], [21000, 232, 290, 340]]) {
    const scale = fitGraphScale(width, height, availableWidth, availableHeight);
    assert.ok(width * scale <= availableWidth - 24 + 0.001);
    assert.ok(height * scale <= availableHeight - 24 + 0.001);
  }
  assert.equal(fitGraphScale(244, 138, 740, 500), 1);
  assert.equal(fitGraphScale(0, 0, 0, 0), 1);
  assert.equal(graphScale(0), 0.01);
  assert.equal(graphScale(50), 2);
  assert.equal(graphScale(NaN), 1);
});

test('fork and join preserve parallel jobs even when input is not topologically ordered', () => {
  const layout = layoutJobs([job('publish', ['linux', 'windows']), job('windows', ['checkout']), job('checkout'), job('linux', ['checkout'])]);
  assert.equal(node(layout, 'linux').x, node(layout, 'windows').x);
  assert.notEqual(node(layout, 'linux').y, node(layout, 'windows').y);
  assert.ok(node(layout, 'checkout').x < node(layout, 'linux').x);
  assert.ok(node(layout, 'publish').x > node(layout, 'windows').x);
  assert.deepEqual(layout.edges.map(edge => `${edge.from.job.name}->${edge.to.job.name}`).sort(), ['checkout->linux', 'checkout->windows', 'linux->publish', 'windows->publish']);
});

test('stage labels group independent jobs without inventing execution dependencies', () => {
  const layout = layoutJobs([job('test', [], 'test'), job('build', [], 'build'), job('utility', [], null)]);
  assert.equal(layout.edges.length, 0);
  assert.equal(new Set(layout.nodes.map(value => value.x)).size, 1);
  assert.equal(layout.lanes.length, 1);
  assert.match(layout.lanes[0].stage, /test.*build.*단계 미지정/);
});

test('shared workspace uses recorded stage barrier edges', () => {
  const jobs = [job('prepare', [], 'prepare'), job('build', ['prepare'], 'build'), job('health', ['build'], 'verify')].map(value => ({ ...value, shared_workspace: true, attempt_id: 'shared' }));
  const layout = layoutJobs(jobs);
  assert.equal(layout.nodes.length, 3);
  assert.equal(layout.edges.length, 2);
  assert.ok(node(layout, 'health').x > node(layout, 'build').x);
});

test('empty and single job graphs need no synthetic nodes or edges', () => {
  assert.deepEqual(layoutJobs([]), { nodes: [], edges: [], lanes: [], width: 0, height: 0 });
  const layout = layoutJobs([job('only')]);
  assert.equal(layout.nodes.length, 1);
  assert.equal(layout.edges.length, 0);
  assert.ok(layout.width >= NODE_WIDTH && layout.height >= NODE_HEIGHT);
});

test('invalid dependencies fail explicitly so the UI can show a usable list', () => {
  assert.throws(() => layoutJobs([job('a', ['missing'])]), /선행 작업/);
  assert.throws(() => layoutJobs([job('a', ['b']), job('b', ['a'])]), /순환/);
  assert.throws(() => layoutJobs([job('a', ['a'])]), /순환/);
  assert.throws(() => layoutJobs([job('a'), job('a')]), /중복/);
});

test('duplicate dependency names do not hide ready jobs', () => {
  const layout = layoutJobs([job('a'), job('b', ['a', 'a'])]);
  assert.equal(layout.edges.length, 1);
  assert.ok(node(layout, 'a').x < node(layout, 'b').x);
});

test('wide and deep DAGs fit their canvas, never overlap nodes, and preserve the input', () => {
  const jobs = Array.from({ length: 128 }, (_, i) => job(`job-${i}`, i < 8 ? [] : [`job-${i - 8}`, `job-${Math.floor(i / 2)}`], `stage-${i % 5}`));
  const before = structuredClone(jobs);
  const layout = layoutJobs(jobs);
  assert.deepEqual(jobs, before);
  for (const current of layout.nodes) {
    assert.ok(current.x >= 0 && current.y >= 0);
    assert.ok(current.x + NODE_WIDTH <= layout.width && current.y + NODE_HEIGHT <= layout.height);
    for (const other of layout.nodes) {
      if (current === other) continue;
      assert.ok(Math.abs(current.x - other.x) >= NODE_WIDTH || Math.abs(current.y - other.y) >= NODE_HEIGHT);
    }
  }
  for (const edge of layout.edges) assert.ok(edge.from.x + NODE_WIDTH < edge.to.x);
});
