import test from 'node:test';
import assert from 'node:assert/strict';
import {layoutConnections,CONNECTION_WIDTH,CONNECTION_HEIGHT} from './pipeline-connections.js';

test('version graph keeps pinned older versions distinct and directs forks forward',()=>{
  const nodes=['child:v1','parent:v2','parent:v1','sibling:v1'].map(key=>({key}));
  const edges=[{from:'parent:v2',to:'parent:v1'},{from:'parent:v2',to:'sibling:v1'},{from:'parent:v1',to:'child:v1'}];
  const copy=structuredClone({nodes,edges});
  const result=layoutConnections(nodes,edges), byKey=new Map(result.nodes.map(n=>[n.key,n]));
  assert.equal(result.nodes.length,4);
  for (const edge of edges) assert.ok(byKey.get(edge.to).x>byKey.get(edge.from).x+CONNECTION_WIDTH);
  assert.equal(byKey.get('parent:v1').x,byKey.get('sibling:v1').x);
  assert.deepEqual({nodes,edges},copy);
});
test('empty, single, wide and deep graphs fit without overlapping nodes',()=>{
  assert.deepEqual(layoutConnections([],[]),{nodes:[],width:0,height:0});
  for (const count of [1,65]) for (const shape of ['chain','fan']) {
    const nodes=Array.from({length:count},(_,i)=>({key:String(i)}));
    const edges=nodes.slice(1).map((n,i)=>({from:shape==='chain'?String(i):'0',to:n.key}));
    const graph=layoutConnections(nodes,edges);
    for (const n of graph.nodes) {assert.ok(n.x>=0 && n.y>=0);assert.ok(n.x+CONNECTION_WIDTH<=graph.width && n.y+CONNECTION_HEIGHT<=graph.height);}
    for (let i=0;i<count;i++) for(let j=i+1;j<count;j++) {
      const a=graph.nodes[i],b=graph.nodes[j];
      assert.ok(a.x+CONNECTION_WIDTH<=b.x || b.x+CONNECTION_WIDTH<=a.x || a.y+CONNECTION_HEIGHT<=b.y || b.y+CONNECTION_HEIGHT<=a.y);
    }
  }
});
test('invalid edges fail explicitly and repeated edges cannot hide reachable nodes',()=>{
  const nodes=[{key:'a'},{key:'b'}],edge={from:'a',to:'b'};
  assert.deepEqual(layoutConnections(nodes,[edge,edge]),layoutConnections(nodes,[edge]));
  assert.throws(()=>layoutConnections([{key:'a'},{key:'a'}],[]),/중복/);
  assert.throws(()=>layoutConnections(nodes,[{from:'a',to:'missing'}]),/찾을 수/);
  assert.throws(()=>layoutConnections(nodes,[edge,{from:'b',to:'a'}]),/순환/);
});
