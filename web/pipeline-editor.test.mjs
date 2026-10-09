import test from 'node:test';
import assert from 'node:assert/strict';
import {renameJob, removeJob, editorGraph} from './pipeline-editor.js';
import {layoutJobs} from './run-graph.js';
const job = (name, stage='', needs=[]) => ({name, stage, needs, runner:{os:'linux',arch:'aarch64',pool:'default',docker:'linux'},shell:'sh',script:['echo first\necho second','echo third'],working_directory:'.',timeout_seconds:120,environment:{MODE:'release'}});
const fixture = () => ({schema_version:1,source_root_id:'root',max_parallel_jobs:1,workspace:'pipeline',stages:['build','test','deploy'],jobs:[job('build','build'),job('test','test'),job('deploy','deploy',['test'])],on_success:[{pipeline_id:'child',version:3}],artifacts:[{name:'bin',path:'dist/bin'}],artifact_inputs:['source'],deployment:{environment_id:'env',health_check:'deploy'}});

test('renaming preserves advanced fields and multiline command boundaries while updating references',()=>{
 const d=fixture(), original=structuredClone(d);renameJob(d,1,'smoke');renameJob(d,2,'health');
 assert.equal(d.jobs[1].name,'smoke');assert.deepEqual(d.jobs[2].needs,['smoke']);assert.equal(d.deployment.health_check,'health');
 for(const key of ['on_success','artifacts','artifact_inputs'])assert.deepEqual(d[key],original[key]);
 for(let i=0;i<d.jobs.length;i++){assert.deepEqual(d.jobs[i].script,original.jobs[i].script);assert.deepEqual(d.jobs[i].environment,original.jobs[i].environment);}
 const before=structuredClone(d);assert.throws(()=>renameJob(d,0,'smoke'));assert.throws(()=>renameJob(d,0,'invalid name'));assert.deepEqual(d,before);
});
test('deletion protects dependencies, health check and last job without partial mutations',()=>{
 const d=fixture();const before=structuredClone(d);assert.throws(()=>removeJob(d,1),/참조/);assert.throws(()=>removeJob(d,2),/참조/);assert.deepEqual(d,before);
 removeJob(d,0);assert.deepEqual(d.jobs.map(j=>j.name),['test','deploy']);
 assert.throws(()=>removeJob({jobs:[job('only')]},0),/한 개/);
});
test('shared workspace graph includes all previous stages, without persisting inferred needs',()=>{
 const d=fixture(), original=structuredClone(d);const jobs=editorGraph(d);layoutJobs(jobs);
 assert.deepEqual(jobs[1].needs,['build']);assert.deepEqual(jobs[2].needs,['test','build']);assert.deepEqual(d,original);
 d.workspace='job';d.stages=[];assert.deepEqual(editorGraph(d).map(j=>j.needs),[[],[],['test']]);
});
test('graph rejects cycles including backward stage edges, missing names, duplicates and invalid shared conditions',()=>{
 const d=fixture();d.jobs[0].needs=['deploy'];assert.throws(()=>layoutJobs(editorGraph(d)),/순환/);
 d.jobs[0].needs=['missing'];assert.throws(()=>layoutJobs(editorGraph(d)),/찾을 수/);
 d.jobs[0].needs=['build'];assert.throws(()=>editorGraph(d),/자기/);
 d.jobs[0].needs=['test','test'];assert.throws(()=>editorGraph(d),/중복/);
 d.jobs[0].needs=[];d.jobs[1].runner.docker=undefined;assert.throws(()=>editorGraph(d),/실행 조건/);
 d.jobs[1].runner.docker='linux';d.jobs[0].stage='unknown';assert.throws(()=>editorGraph(d),/단계 순서/);
 d.jobs[0].stage='build';d.max_parallel_jobs=2;assert.throws(()=>editorGraph(d),/동시/);
 d.max_parallel_jobs=1;d.stages=['build','build'];assert.throws(()=>editorGraph(d),/중복/);
 d.workspace='job';assert.throws(()=>editorGraph(d),/비워/);
});
