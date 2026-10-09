import test from 'node:test';
import assert from 'node:assert/strict';
import {layoutProjectGraph, selectProjectNode, nodeLabel} from './project-graph.js';
import {CONNECTION_WIDTH as W, CONNECTION_HEIGHT as H} from './pipeline-connections.js';

function noOverlap(layout) {
  for (const a of layout.nodes) {
    assert.ok(a.x>=0 && a.y>=0 && a.x+W<=layout.width && a.y+H<=layout.height);
    for (const b of layout.nodes) if (a!==b) assert.ok(a.x+W<=b.x || b.x+W<=a.x || a.y+H<=b.y || b.y+H<=a.y);
  }
}
test('project graph includes disconnected drafts and both current and pinned old versions',()=>{
  const nodes=['build:v2','package:v1','package:v2','deploy:v1','draft:v0','other:v1'].map(key=>({key}));
  const edges=[{from:'build:v2',to:'package:v1'},{from:'package:v1',to:'deploy:v1'}];
  const layout=layoutProjectGraph(nodes,edges); noOverlap(layout);
  assert.deepEqual(new Set(layout.nodes.map(n=>n.key)),new Set(nodes.map(n=>n.key)));
  const x=key=>layout.nodes.find(n=>n.key===key).x;
  assert.ok(x('build:v2')<x('package:v1') && x('package:v1')<x('deploy:v1'));
});
test('independent components wrap into rows and every supported node remains visible',()=>{
  const nodes=Array.from({length:1000},(_,i)=>({key:String(i)}));
  const layout=layoutProjectGraph(nodes,[]); noOverlap(layout);
  assert.equal(layout.nodes.length,1000); assert.ok(layout.width<=1120);
  assert.deepEqual(layoutProjectGraph([],[]),{nodes:[],width:0,height:0});
});
test('invalid or cyclic graphs fail explicitly for a complete list fallback',()=>{
  assert.throws(()=>layoutProjectGraph([{key:'a'},{key:'a'}],[]));
  assert.throws(()=>layoutProjectGraph([{key:'a'}],[{from:'a',to:'missing'}]));
  assert.throws(()=>layoutProjectGraph([{key:'a'},{key:'b'}],[{from:'a',to:'b'},{from:'b',to:'a'}]));
});
test('selection and labels preserve revision identity and distinguish configuration from execution',()=>{
  const old={key:'p:v1',pipeline_id:'p',version:1,current_version:2};
  const current={...old,key:'p:v2',version:2}; const draft={key:'d:v0',pipeline_id:'d',version:0,current_version:0};
  const config={...current,key:'p',version:null,configuration:{generation:3}};
  assert.equal(selectProjectNode([old,current,draft],'p'),current);
  assert.equal(selectProjectNode([old,current,draft],'p','1'),old);
  assert.equal(selectProjectNode([old,current,draft],'d'),draft);
  assert.equal(selectProjectNode([config],'p'),config);
  assert.equal(selectProjectNode([old,current],'missing'),old);
  assert.equal(selectProjectNode([]),undefined);
  assert.match(nodeLabel(old,'published'),/고정 과거 버전/);
  assert.equal(nodeLabel(draft,'published'),'미발행');
  assert.equal(nodeLabel(config,'configuration'),'가져온 구성 g3');
});
