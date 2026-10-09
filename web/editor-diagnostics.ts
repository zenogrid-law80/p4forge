import type {EditorDefinition} from './pipeline-editor.js';

export type EditorIssue = {level:'error'|'warning'|'info';category:string;code:string;message:string;field?:string;job_index?:number|null;line?:number;column?:number;offset?:number};
export type DiagnosticReport = {pipeline_id:string;source:'saved'|'editor';checked_at:string;current_version:number;generation:number;base_version:number;valid:boolean;issues:EditorIssue[];definition:EditorDefinition|null;toml:string|null;definition_hash:string|null;graph:{order:string[];edges:string[][]}|null};
type Snapshot = {key:string;generation:number;base_version:number;body?:unknown};

// Every edit invalidates in-flight results, including edit-and-revert and mode changes.
export class DiagnosticGuard {
  private epoch=0; private sequence=0; private disposed=false;
  begin(key:string) {return {key,epoch:this.epoch,sequence:++this.sequence};}
  invalidate() {this.epoch++;}
  accepts(ticket:ReturnType<DiagnosticGuard['begin']>,key:string) {return !this.disposed && ticket.sequence===this.sequence && ticket.epoch===this.epoch && ticket.key===key;}
  dispose() {this.disposed=true;this.sequence++;}
}
export const diagnosticFields:Record<string,string>={source_root_id:'소스 루트',workspace:'Workspace',max_parallel_jobs:'동시 작업 수',stages:'단계 순서 (한 줄에 하나 · 공유 workspace만)',name:'새 작업 이름',stage:'단계 이름',working_directory:'작업 디렉터리','runner.os':'운영체제','runner.arch':'아키텍처','runner.pool':'실행 풀',runner:'운영체제',shell:'셸',timeout_seconds:'제한 시간 (초)',needs:'선행 작업',script:'명령 1'};
const categories:Record<string,string>={state:'초안·발행 상태',syntax:'TOML 구문·형식',definition:'정의·의존 관계',reference:'소스·후속·배포 참조',availability:'에이전트 가용성'};
export function issueLocation(issue:EditorIssue) {return issue.line?`TOML ${issue.line}행 ${issue.column}열`:[issue.job_index!==undefined&&issue.job_index!==null?`작업 ${issue.job_index+1}`:'',diagnosticFields[issue.field??'']??issue.field??''].filter(Boolean).join(' · ');}
function el<K extends keyof HTMLElementTagNameMap>(tag:K,text?:string,className='') {const node=document.createElement(tag);if(text!==undefined)node.textContent=text;node.className=className;return node;}

export function createEditorDiagnostics(host:HTMLElement,context:{id:string;api:<T>(path:string,method?:string,body?:unknown)=>Promise<T>;snapshot:()=>Snapshot;canCheck:()=>boolean;jump:(issue:EditorIssue,report:DiagnosticReport)=>void;changed:()=>void}) {
  const guard=new DiagnosticGuard();let report:DiagnosticReport|undefined;let current=false;let pending=false;let disposed=false;let sequence=0;let edited=false;let failure='';
  const panel=el('section',undefined,'editor-diagnostics');panel.setAttribute('aria-label','설정 진단');
  const row=el('div',undefined,'row space');row.append(el('h3','설정 진단'));
  const check=el('button','설정 검사','small');check.type='button';row.append(check);
  const state=el('p','','caption');state.setAttribute('role','status');const issues=el('div',undefined,'editor-issues');
  panel.append(row,state,issues);host.prepend(panel);
  const label=()=>pending?'검사 중':failure?'검사 실패':current?(report?.valid?'검사 통과':'오류 있음'):report||edited?'재검사 필요':'미검사';
  function render() {
    check.disabled=pending;panel.classList.toggle('diagnostics-stale',!current);issues.replaceChildren();
    state.textContent=pending?'현재 설정을 검사하는 중…':failure?`검사 실패: ${failure}${report?' · 아래는 이전 검사 결과입니다.':''}`:report?`${current?'현재 입력 검사':'이전 검사 결과 · 재검사가 필요합니다.'} · ${new Date(report.checked_at).toLocaleString('ko-KR')} · 오류 ${report.issues.filter(i=>i.level==='error').length}개 · 주의 ${report.issues.filter(i=>i.level==='warning').length}개`:'정의·참조·에이전트 상태를 조회합니다. 저장·발행·실행은 수행하지 않습니다.';
    if(report){state.textContent+=` · 검사 시 발행 v${report.current_version} / 초안 g${report.generation} / 기준 v${report.base_version}`;}
    if(report)for(const [category,title]of Object.entries(categories)) {
      const items=report.issues.filter(issue=>issue.category===category);if(!items.length)continue;
      const group=el('section');group.append(el('h4',title));
      for(const issue of items) {
        const item=el('div',undefined,`editor-issue ${issue.level}`);item.append(el('span',`${{error:'오류',warning:'주의',info:'안내'}[issue.level]} · ${issue.message}`));
        if(issue.field && !['draft','configuration'].includes(issue.field)) {
          const target=el('button',`${issueLocation(issue)||'설정'} 위치 보기`,'small');target.type='button';target.disabled=!current||pending;
          target.addEventListener('click',()=>{if(current&&!pending&&report)context.jump(issue,report);});item.append(target);
        }
        group.append(item);
      }issues.append(group);
    }
    context.changed();
  }
  async function inspect() {
    if(disposed||!context.canCheck())return null;
    const snapshot=context.snapshot(),ticket=guard.begin(snapshot.key),request=++sequence;
    pending=true;current=false;failure='';render();
    try {
      const next=await context.api<DiagnosticReport>(`/api/pipelines/${context.id}/diagnostics`,snapshot.body===undefined?'GET':'POST',snapshot.body);
      if(disposed||!panel.isConnected||!guard.accepts(ticket,context.snapshot().key))return null;
      if(next.pipeline_id!==context.id||next.source!==(snapshot.body===undefined?'saved':'editor'))throw new Error('검사 대상이 응답과 일치하지 않습니다.');
      report=next;current=true;edited=false;
      if(snapshot.body===undefined&&(next.generation!==snapshot.generation||next.base_version!==snapshot.base_version)) {current=false;failure='서버 초안이 변경되었습니다. 화면을 다시 열어 최신 내용을 확인하세요.';}
      return next;
    }catch(error) {
      if(disposed||!guard.accepts(ticket,context.snapshot().key))return null;
      failure=error instanceof Error?error.message:String(error);current=false;
      if([401,403,404].includes((error as {status?:number}).status??0))report=undefined;
      return null;
    }finally {if(!disposed&&request===sequence){pending=false;render();}}
  }
  check.addEventListener('click',()=>void inspect());
  // Initial state is rendered without notifying the not-yet-built editor.
  state.textContent='아직 검사하지 않았습니다. 설정 검사로 오류 위치와 현재 실행 준비 상태를 확인하세요.';
  return {check:inspect,label,invalidate:()=>{guard.invalidate();current=false;edited=true;failure='';render();},clear:()=>{guard.invalidate();report=undefined;current=false;edited=false;failure='';render();},
    currentIssues:()=>current?report?.issues??[]:[],dispose:()=>{disposed=true;guard.dispose();}};
}
