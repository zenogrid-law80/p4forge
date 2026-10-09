import test from 'node:test';
import assert from 'node:assert/strict';
import { diagnosticRoute } from './workspace.js';
import { parseRoute, routeHash } from './navigation.js';

test('diagnostic links preserve exact runs and jobs instead of finding a newer run with the same CL', () => {
  const item = {run_id: 'failed-old-run', project_id: 'p', pipeline_id: 'build', job_id: 'abababab-1234-5678-9876-123456789abc', kind: 'followup_blocked'};
  const failure = diagnosticRoute(item, 'failed_24h');
  assert.equal(failure.run, 'failed-old-run');
  assert.equal(failure.job, item.job_id);
  assert.equal(failure.tab, 'graph');
  assert.deepEqual(parseRoute(routeHash(failure)), failure);
  assert.equal(diagnosticRoute(item, 'actions').tab, 'chain');
  assert.equal(diagnosticRoute(item, 'actions').run, item.run_id);
  assert.equal(diagnosticRoute(item, 'actions').job, undefined);
  for (const kind of ['awaiting_approval', 'recovery_required']) assert.equal(diagnosticRoute({...item, kind}, 'actions').tab, 'deployment');
  assert.equal(diagnosticRoute(item, 'queued').tab, 'dispatch');
  assert.equal(diagnosticRoute({...item, job_id: null}, 'failed_24h').job, undefined);
});

test('job selection is limited to graph URLs and valid UUIDs', () => {
  const id = 'ABABABAB-1234-5678-9876-123456789ABC';
  assert.equal(parseRoute(`#run?run=r&job=${id}`).job, id.toLowerCase());
  for (const hash of [`#run?run=r&tab=chain&job=${id}`, `#pipeline?pipeline=p&job=${id}`, '#run?run=r&job=<script>', '#run?run=r&job=../secret']) assert.equal(parseRoute(hash).job, undefined);
});
