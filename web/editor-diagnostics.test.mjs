import test from 'node:test';
import assert from 'node:assert/strict';
import {DiagnosticGuard,issueLocation,diagnosticFields} from './editor-diagnostics.js';

test('diagnostics discard responses after edits, undo to the same text, and a newer inspection',()=>{
  const guard=new DiagnosticGuard();const first=guard.begin('TOML A');
  assert.equal(guard.accepts(first,'TOML B'),false);
  guard.invalidate();assert.equal(guard.accepts(first,'TOML A'),false);
  const second=guard.begin('TOML A'),third=guard.begin('TOML A');
  assert.equal(guard.accepts(second,'TOML A'),false);assert.equal(guard.accepts(third,'TOML A'),true);
});
test('saved/edited mode, draft generation and disposal invalidate earlier diagnostics',()=>{
  const guard=new DiagnosticGuard();const saved=guard.begin('saved:g2');
  assert.equal(guard.accepts(saved,'editor:g2'),false);assert.equal(guard.accepts(saved,'saved:g3'),false);
  const editor=guard.begin('editor:g3');guard.dispose();assert.equal(guard.accepts(editor,'editor:g3'),false);
});
test('error navigation uses exact job indices and syntax positions, independent of duplicate names',()=>{
  assert.equal(issueLocation({field:'timeout_seconds',job_index:0}),'작업 1 · 제한 시간 (초)');
  assert.equal(issueLocation({field:'timeout_seconds',job_index:1}),'작업 2 · 제한 시간 (초)');
  assert.equal(issueLocation({field:'toml',line:4,column:8}),'TOML 4행 8열');
  assert.equal(diagnosticFields['source_root_id'],'소스 루트');
  assert.equal(issueLocation({field:'needs',job_index:null}),'선행 작업');
});
