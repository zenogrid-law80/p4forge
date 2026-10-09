import test from 'node:test';
import assert from 'node:assert/strict';
import {chainLabel, chainRunRoute} from './run-chain.js';
import {parseRoute, routeHash} from './navigation.js';

const run = {kind:'run', key:'old-run', run_id:'old-run', pipeline_id:'build', status:'queued', version:1};
test('chain actions follow exact execution identity and approval/recovery state', () => {
  assert.deepEqual(chainRunRoute(run,'project'),{page:'run',project:'project',pipeline:'build',run:'old-run',tab:'graph'});
  for (const state of ['awaiting_approval','recovery_required']) {
    const node={...run,deployment_status:state};
    assert.equal(chainRunRoute(node,'project').tab,'deployment');
    assert.match(chainLabel(node),/배포/);
  }
  assert.equal(chainLabel({...run,cancel_requested:true}),'취소 처리 중');
  assert.equal(chainLabel({...run,status:'succeeded',cancel_requested:true}),'성공');
});
test('uncreated followups never navigate to guessed runs', () => {
  for(const status of ['pending','blocked','awaiting_parent','not_scheduled']) {
    const node={...run,kind:'followup',run_id:null,status};
    assert.equal(chainRunRoute(node,'project'),undefined);
    assert.notEqual(chainLabel(node),status);
  }
});
test('chain deep links round trip without retaining pipeline version selection', () => {
  const route={page:'run',project:'p',pipeline:'pipeline',run:'actual-run',tab:'chain'};
  assert.deepEqual(parseRoute(routeHash(route)),route);
  assert.deepEqual(parseRoute(routeHash({...route,version:'3'})),route);
});
