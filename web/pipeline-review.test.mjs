import test from 'node:test';
import assert from 'node:assert/strict';
import {definitionChanges} from './pipeline-review.js';
import {parseRoute,routeHash} from './navigation.js';

test('comparison routes retain first-publish and historical selections only in review',()=>{
 const route={page:'pipeline',pipeline:'p',tab:'review',from:'0',to:'draft'};
 assert.deepEqual(parseRoute(routeHash(route)),{...route,project:undefined});
 const old=parseRoute('#pipeline?pipeline=p&tab=review&from=12&to=9');assert.equal(old.from,'12');assert.equal(old.to,'9');
 for(const value of ['-1','1.5','NaN','9007199254740992','01'])assert.equal(parseRoute(`#pipeline?tab=review&from=${value}&to=${value}`).from,undefined);
 assert.equal(parseRoute('#pipeline?tab=review&to=0').to,undefined);
 assert.equal(parseRoute('#pipeline?tab=settings&from=1&to=2').to,undefined);
});
test('semantic diff ignores object-key order but reviews scripts, runner and all fields',()=>{
 const a={jobs:[{name:'build',script:['first','second'],runner:{os:'linux',arch:'aarch64'},environment:{MODE:'test'}}],on_success:[{pipeline_id:'p',version:1}],artifacts:[{name:'a',path:'a'}]};
 const b={artifacts:[{path:'a',name:'a'}],on_success:[{version:2,pipeline_id:'p'}],jobs:[{environment:{MODE:'release'},runner:{arch:'aarch64',os:'linux',docker:'linux'},script:['second','first'],name:'build'}]};
 const changes=definitionChanges(a,b);
 assert.deepEqual(changes.map(c=>c.path),['jobs["build"].environment.MODE','jobs["build"].runner.docker','jobs["build"].script','on_success']);
 assert.equal(changes[1].kind,'added');assert.deepEqual(changes[2].before,['first','second']);
 assert.deepEqual(definitionChanges(a,JSON.parse(JSON.stringify(a))),[]);
});
test('job additions and reorder do not misattribute existing job contents',()=>{
 const a={jobs:[{name:'a',script:['a']},{name:'b',script:['b']}]};
 const b={jobs:[{name:'c',script:['c']},{name:'b',script:['b']},{name:'a',script:['new']}]};
 const changes=definitionChanges(a,b);assert.deepEqual(changes.map(c=>c.path),['jobs / 순서','jobs["a"].script','jobs["c"]']);
 assert.equal(changes[2].kind,'added');assert.equal(definitionChanges(b,a).find(c=>c.path==='jobs["c"]').kind,'removed');
});
test('first publication, removed fields, hostile keys and null remain visible',()=>{
 const data=JSON.parse('{"jobs":[{"name":"__proto__","script":["<img src=x onerror=alert(1)>"]}],"__proto__":{"setting":true},"constructor":null}');
 const added=definitionChanges(null,data);assert.equal(added.length,3);assert.ok(added.every(c=>c.kind==='added'));
 assert.equal(definitionChanges({setting:null},{})[0].kind,'removed');
 const changed=structuredClone(data);changed.jobs[0].script=['safe'];
 assert.equal(definitionChanges(data,changed)[0].path,'jobs["__proto__"].script');
 assert.equal({}.setting,undefined);
});
