import test from 'node:test';
import assert from 'node:assert/strict';
import { parseRoute, routeHash, matchesPipeline } from './navigation.js';

test('project graph URLs restore view, selection, search and pinned version independently',()=>{
  const route={page:'pipeline-graph',project:'p',pipeline:'build',view:'published',q:'한국어 & test',version:'2'};
  assert.deepEqual(parseRoute(routeHash(route)),route);
  assert.equal(parseRoute('#pipeline-graph?view=configuration&version=2').version,undefined);
  assert.equal(parseRoute('#pipeline-graph?view=invalid').view,'published');
  for(const version of ['0','-1','1.5','9007199254740992','invalid']) assert.equal(parseRoute(`#pipeline-graph?version=${version}`).version,undefined);
});

test('CI workspace restores pipeline, view, job name, search and pinned connection version', () => {
  const route = {page:'ci-settings',project:'project-a',pipeline:'pipeline-a',view:'connections',job:'health-check',q:'server / build',version:'3'};
  assert.deepEqual(parseRoute(routeHash(route)),route);
  const visual = parseRoute('#ci-settings?project=p&pipeline=x&view=visual&job=compile&version=2');
  assert.equal(visual.view,'visual'); assert.equal(visual.job,'compile'); assert.equal(visual.version,undefined);
  assert.equal(parseRoute('#ci-settings?project=p&view=invalid&job=../bad').view,'visual');
  assert.equal(parseRoute('#ci-settings?project=p&view=invalid&job=../bad').job,undefined);
  assert.equal(parseRoute('#ci-settings?view=toml&job=package').view,'toml');
});

test('CI job names and view state never leak into execution or pipeline routes', () => {
  assert.equal(parseRoute('#run?run=r&tab=graph&job=build&view=visual').job,undefined);
  assert.equal(parseRoute('#run?run=r&tab=graph&view=visual').view,undefined);
  assert.equal(parseRoute('#pipeline?pipeline=p&tab=settings&job=build&view=toml').view,undefined);
});

test('shared execution URLs preserve project, pipeline, execution and selected tab', () => {
  const route = { page: 'run', project: 'project-1', pipeline: 'pipeline-2', run: 'old-run-outside-latest-100', tab: 'artifacts' };
  assert.deepEqual(parseRoute(routeHash(route)), route);
  assert.deepEqual(parseRoute(routeHash({...route, tab: 'dispatch'})), {...route, tab: 'dispatch'});
  assert.equal(parseRoute('#run?run=old-run').tab, 'graph');
  assert.equal(parseRoute('#pipeline?pipeline=build').tab, 'runs');
});

test('legacy URLs and unrecognized tabs have usable defaults', () => {
  assert.deepEqual(parseRoute('#runners'), { page: 'agents' });
  assert.deepEqual(parseRoute(''), { page: 'overview' });
  assert.deepEqual(parseRoute('#unknown?project=private'), { page: 'overview' });
  assert.equal(parseRoute('#settings?project=p&tab=not-a-tab').tab, 'sources');
  assert.equal(parseRoute('#pipeline?pipeline=p&tab=deployment').tab, 'runs');
});

test('connection links retain a pinned version only in the connection tab', () => {
  const route = {page:'pipeline',project:'p',pipeline:'build',tab:'connections',version:'2'};
  assert.deepEqual(parseRoute(routeHash(route)), route);
  for (const version of ['0','-1','1.5','9007199254740992','not-a-version']) assert.equal(parseRoute(`#pipeline?pipeline=p&tab=connections&version=${version}`).version, undefined);
  assert.equal(parseRoute('#pipeline?pipeline=p&tab=settings&version=2').version, undefined);
  assert.equal(parseRoute('#run?run=r&version=2').version, undefined);
});

test('search URLs round trip reserved characters and do not retain another screen context', () => {
  const route = { page: 'pipelines', project: 'p', q: 'build & test / 한국어? #1', state: 'failed' };
  assert.deepEqual(parseRoute(routeHash(route)), route);
  assert.deepEqual(parseRoute('#agents?project=p&pipeline=x&run=y&tab=settings'), { page: 'agents' });
  assert.equal(parseRoute('#pipelines?project=p&state=unknown').state, undefined);
});

test('pipeline filters distinguish unpublished, failed, active and automatic builds', () => {
  const items = [
    { name: 'Windows CI', current_version: 8, latest_run_status: 'succeeded', trigger_enabled: true },
    { name: 'Mac CI', current_version: 2, latest_run_status: 'lost' },
    { name: 'Deploy', current_version: 1, latest_run_status: 'queued' },
    { name: 'Draft', current_version: 0 },
  ];
  const names = (query, state) => items.filter(item => matchesPipeline(item, query, state)).map(item => item.name);
  assert.deepEqual(names('  WINDOWS ', 'automatic'), ['Windows CI']);
  assert.deepEqual(names('', 'failed'), ['Mac CI']);
  assert.deepEqual(names('', 'running'), ['Deploy']);
  assert.deepEqual(names('', 'draft'), ['Draft']);
  assert.deepEqual(names('CI', 'published'), ['Windows CI', 'Mac CI']);
  assert.deepEqual(names('missing', ''), []);
});


test('workspace history links keep filters and keyset cursor without leaking them to other pages', () => {
  const route = {page: 'runs', project: 'visible', q: 'build & QA', state: 'awaiting_approval', cursor: 'opaque_cursor'};
  assert.deepEqual(parseRoute(routeHash(route)), route);
  assert.deepEqual(parseRoute('#operations?project=private&cursor=old'), {page: 'operations'});
  assert.deepEqual(parseRoute('#overview?state=failed'), {page: 'overview'});
  assert.equal(parseRoute('#runs?state=unexpected').state, undefined);
  assert.equal(parseRoute('#runs?state=recovery_required').state, 'recovery_required');
});

test('agent links preserve registration state, platform filters and a selected agent', () => {
  const route = {page:'agents',q:'Mac & Linux / 한글',state:'revoked',agent:'retired-id',os:'macos',arch:'aarch64',engine:'linux',availability:'stale'};
  assert.deepEqual(parseRoute(routeHash(route)), route);
  assert.deepEqual(parseRoute('#runners?agent=retired-id&state=revoked'), {page:'agents',agent:'retired-id',state:'revoked'});
  assert.deepEqual(parseRoute('#agents?project=private&pipeline=x&cursor=old&state=invalid&os=invalid&arch=armv7&engine=macos&availability=ready'), {page:'agents'});
  assert.deepEqual(parseRoute('#operations?agent=id&os=macos&state=revoked'), {page:'operations'});
  assert.deepEqual(parseRoute('#agents?agent=retired-id'), {page:'agents',agent:'retired-id'});
});
