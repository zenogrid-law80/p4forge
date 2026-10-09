import test from 'node:test';
import assert from 'node:assert/strict';
import { connectionStatus, dockerStatus, matchesAgent } from './agent-status.js';
const now = Date.parse('2026-10-09T00:00:00Z');
const agent = { active: true, last_seen_at: new Date(now).toISOString(), docker_state: 'available', docker_engine_os: 'linux', docker_checked_at: new Date(now).toISOString() };
test('global agent filters combine host platform and independently reported Docker environment', () => {
  const host = {...agent, name: 'mac-mini', os: 'macos', arch: 'aarch64', pool: 'default'};
  const filters = {search: ' MAC ', os: 'macos', arch: 'aarch64', engine: 'linux', availability: 'available'};
  assert.equal(matchesAgent(host, filters, now), true);
  for (const change of [{os: 'linux'}, {arch: 'x86_64'}, {engine: 'windows'}, {availability: 'unavailable'}, {search: 'missing'}]) {
    assert.equal(matchesAgent(host, {...filters, ...change}, now), false);
  }
  const stale = {...host, docker_checked_at: new Date(now - 150001).toISOString()};
  assert.equal(matchesAgent(stale, filters, now), false);
  assert.equal(matchesAgent(stale, {...filters, availability: 'stale'}, now), true);
  assert.equal(matchesAgent({...host, active: false}, filters, now), false);
});
test('engine access differs from missing CLI and unreachable engine', () => {
  assert.equal(dockerStatus(agent, now).label, '사용 가능');
  assert.equal(dockerStatus(agent, now).detail, 'Linux 엔진');
  assert.equal(dockerStatus({...agent, docker_engine_os: 'windows'}, now).detail, 'Windows 엔진');
  assert.equal(dockerStatus({...agent, docker_state: 'cli_missing', docker_engine_os: null}, now).detail, 'CLI 없음');
  assert.equal(dockerStatus({...agent, docker_state: 'engine_unavailable', docker_engine_os: null}, now).label, '사용 불가');
  assert.equal(dockerStatus({...agent, docker_state: 'check_failed', docker_engine_os: null}, now).label, '확인 실패');
});
test('unknown legacy data never claims Docker availability', () => {
  for (const change of [{docker_state: 'unknown'}, {docker_engine_os: null}, {docker_checked_at: null}, {docker_state: 'unexpected'}]) {
    assert.equal(dockerStatus({...agent, ...change}, now).label, '확인 전');
  }
});
test('offline, revoked and outdated results are explicitly historical', () => {
  for (const change of [{active: false}, {last_seen_at: null}, {docker_checked_at: new Date(now - 150001).toISOString()}, {docker_checked_at: 'invalid'}]) {
    const state = dockerStatus({...agent, ...change}, now);
    assert.equal(state.label, '확인 지연'); assert.match(state.detail, /^마지막:/);
  }
  assert.equal(connectionStatus({...agent, active: false}, now).label, '폐기됨');
  assert.equal(connectionStatus({...agent, last_seen_at: new Date(now - 30000).toISOString()}, now).online, false);
});

test('registration filters keep retired agents out of active, offline and draining lists', () => {
  const host = {...agent, name:'compose-executor', os:'macos', arch:'aarch64', pool:'compose-demo', draining:false};
  const hosts = [host, {...host,name:'offline',last_seen_at:null}, {...host,name:'paused',draining:true}, {...host,name:'retired',active:false,draining:true}];
  const filters = {search:'',os:'',arch:'',engine:'',availability:''};
  const names = (state, change={}) => hosts.filter(item => matchesAgent(item,{...filters,state,...change},now)).map(item => item.name);
  assert.deepEqual(names(undefined), ['compose-executor','offline','paused']);
  assert.deepEqual(names('active'), ['compose-executor','offline','paused']);
  assert.deepEqual(names('online'), ['compose-executor','paused']);
  assert.deepEqual(names('offline'), ['offline']);
  assert.deepEqual(names('draining'), ['paused']);
  assert.deepEqual(names('revoked'), ['retired']);
  assert.deepEqual(names('all'), ['compose-executor','offline','paused','retired']);
  assert.deepEqual(names('revoked',{search:'retired',os:'macos',arch:'aarch64',availability:'stale'}), ['retired']);
  assert.deepEqual(names('all',{engine:'windows'}), []);
  assert.deepEqual(names('active',{search:'retired'}), []);
});
