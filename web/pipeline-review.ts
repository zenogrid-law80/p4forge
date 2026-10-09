import type { Route } from './navigation.js';

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Change = { path: string; kind: 'added' | 'removed' | 'changed'; before: Json | undefined; after: Json | undefined };
const object = (value: Json | undefined): value is { [key: string]: Json } => value !== null && typeof value === 'object' && !Array.isArray(value);
function equal(a: Json | undefined, b: Json | undefined): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((value, i) => equal(value, b[i]));
  if (object(a) && object(b)) return Object.keys(a).length === Object.keys(b).length && Object.keys(a).every(key => Object.hasOwn(b, key) && equal(a[key], b[key]));
  return false;
}
// Keys are never written into an object: even a job named "__proto__" is ordinary data.
export function definitionChanges(before: Json, after: Json): Change[] {
  const changes: Change[] = [];
  const visit = (a: Json | undefined, b: Json | undefined, path: string) => {
    if (equal(a, b)) return;
    if (object(a) && object(b)) {
      for (const key of [...new Set([...Object.keys(a), ...Object.keys(b)])].sort()) {
        const av = Object.hasOwn(a, key) ? a[key] : undefined, bv = Object.hasOwn(b, key) ? b[key] : undefined;
        if (path === '' && key === 'jobs' && Array.isArray(av) && Array.isArray(bv)) {
          const jobs = (values: Json[]) => new Map(values.map(value => [object(value) ? String(value.name) : '', value]));
          const left = jobs(av), right = jobs(bv);
          if (left.size !== av.length || right.size !== bv.length) { visit(av, bv, 'jobs'); continue; }
          // Job order matters for independent jobs and is reviewed separately from their contents.
          visit([...left.keys()], [...right.keys()], 'jobs / 순서');
          for (const name of [...new Set([...left.keys(), ...right.keys()])]) visit(left.get(name), right.get(name), `jobs[${JSON.stringify(name)}]`);
        } else visit(av, bv, path ? `${path}.${key}` : key);
      }
    } else changes.push({ path: path || 'definition', kind: a === undefined ? 'added' : b === undefined ? 'removed' : 'changed', before: a, after: b });
  };
  visit(before ?? {}, after ?? {}, ''); return changes;
}

type Check = { level: 'error' | 'warning' | 'info'; code: string; message: string };
type Summary = {
  source: { id: string; name: string; kind: string } | null; workspace: string; max_parallel_jobs: number;
  jobs: { name: string; runner: { os: string; arch: string; pool: string; docker?: string }; required_protocol: number; active: number; available: number; blockers: Record<string, number> }[];
  artifact_inputs: string[]; artifacts: { name: string; path: string }[];
  followups: { pipeline_id: string; name: string; version: number; artifact_inputs: string[] }[];
  deployment: { id: string; name: string; generation: number; health: string; active_run_id: string | null; executor_configured: boolean } | null;
  publishes_generated: boolean;
};
type Snapshot = { label: string; version?: number; generation?: number; base_version?: number; definition: Json; definition_hash?: string };
type Comparison = { checked_at: string; current_version: number; before: Snapshot; after: Snapshot; summary: Summary | null; checks: Check[]; can_publish: boolean };
type Preflight = { checked_at: string; version: number; definition_hash: string; changelist: number; source_run_id: string | null; summary: Summary; artifacts: { id: string; name: string; size: number; sha256: string }[]; checks: Check[]; can_queue: boolean };
type Api = <T>(path: string, method?: string, body?: unknown) => Promise<T>;
type Context = { route: Route; revisions: { version: number }[]; api: Api; link: (title: string, route: Route, className?: string) => HTMLAnchorElement; navigate: (route: Route) => Promise<void>; message: (text: string, error?: boolean) => void };
const blockers: Record<string,string> = { offline:'오프라인', draining:'비우기 중', busy:'사용 중', protocol_unknown:'프로토콜 확인 필요', protocol_old:'프로토콜 업데이트 필요', docker_stale:'Docker 상태 확인 필요', docker_unavailable:'Docker 사용 불가', docker_engine:'Docker 엔진 불일치' };
function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string) { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; }
function button(text: string, className = '') { const node = el('button', text, className); node.type = 'button'; return node; }
function errorText(error: unknown) { return error instanceof Error ? error.message : String(error); }
function date(value: string) { return new Date(value).toLocaleString('ko-KR'); }
function renderChecks(host: HTMLElement, checks: Check[]) {
  const list = el('ul', undefined, 'review-checks');
  for (const check of checks) { const item = el('li', undefined, `review-check ${check.level}`); item.append(el('strong', `${{ error:'수정 필요', warning:'참고', info:'안내' }[check.level]} · `), document.createTextNode(check.message)); list.append(item); }
  host.append(list);
}
function renderSummary(host: HTMLElement, data: Summary, context: Context) {
  const summary = el('section', undefined, 'review-summary'); summary.append(el('h3', '실행 조건 요약'));
  summary.append(el('p', `소스: ${data.source?.name ?? '찾을 수 없음'}${data.source ? ` (${data.source.kind})` : ''} · workspace: ${data.workspace} · 최대 동시 작업 ${data.max_parallel_jobs}개`, 'caption'));
  const list = el('ul', undefined, 'review-jobs');
  for (const job of data.jobs) {
    const runner = job.runner, item = el('li');
    item.append(el('strong', job.name), el('p', `${runner.os} / ${runner.arch} · ${runner.pool} · ${runner.docker ? `Docker ${runner.docker}` : '호스트 실행'} · 최소 프로토콜 ${job.required_protocol} (관리형 인증은 최소 6)`, 'caption'));
    item.append(el('p', `활성 후보 ${job.active}개 · 현재 조건 통과 ${job.available}개${Object.keys(job.blockers).length ? ` · ${Object.entries(job.blockers).map(([reason,count])=>`${blockers[reason] ?? reason} ${count}`).join(', ')}` : ''}`, 'caption'));
    list.append(item);
  }
  summary.append(list,el('p', `입력 산출물: ${data.artifact_inputs.join(', ') || '없음'}`),el('p', `보관할 산출물: ${data.artifacts.map(a=>`${a.name} (${a.path})`).join(', ') || '없음'}`));
  if (data.followups.length) {
    const followups = el('ul');
    for (const child of data.followups) { const item = el('li'); item.append(context.link(`${child.name} · 고정 v${child.version}`, {page:'pipeline', project:context.route.project, pipeline:child.pipeline_id, tab:'connections', version:String(child.version)}),document.createTextNode(` · 입력: ${child.artifact_inputs.join(', ') || '없음'}`)); followups.append(item); }
    summary.append(el('h4','성공 후 실행'),followups);
  } else summary.append(el('p','성공 후 실행: 없음'));
  if (data.deployment) { const env = data.deployment; summary.append(el('p',`배포 환경: ${env.name} · g${env.generation} · ${env.health} · ${env.executor_configured ? '실행 에이전트 지정됨' : '실행 에이전트 미지정'} · ${env.active_run_id ? '환경 사용 중' : '환경 잠금 없음'}`)); }
  else summary.append(el('p','배포 환경: 없음'));
  if (data.publishes_generated) summary.append(el('p','생성 파일을 Perforce에 게시합니다. 후속 실행은 게시 결과 CL 또는 변경이 없을 때 입력 CL을 사용합니다.','caption'));
  host.append(summary);
}

export function createReviewView(content: HTMLElement, context: Context): () => void {
  const panel = el('section', undefined, 'card pipeline-review'); panel.append(el('h2', '발행 변경 비교'),el('p', '저장된 설정의 작업·스크립트·실행 조건과 연결을 비교합니다. 주석과 TOML 표기 차이는 제외합니다.', 'caption'));
  const controls = el('div', undefined, 'row review-controls');
  const from = el('select'), to = el('select');from.setAttribute('aria-label','기준');to.setAttribute('aria-label','비교 대상');
  const add = (select: HTMLSelectElement, value: string, text: string) => { if (!Array.from(select.options).some(o=>o.value === value)) { const option = el('option',text); option.value=value;select.append(option); } };
  add(from,'0','빈 기준 (첫 발행)');add(to,'draft','저장된 초안');
  for (const revision of context.revisions) for (const select of [from,to]) add(select,String(revision.version),`v${revision.version}`);
  if (context.route.from) add(from,context.route.from,context.route.from === '0' ? '빈 기준' : `v${context.route.from}`);
  if (context.route.to) add(to,context.route.to,context.route.to === 'draft' ? '저장된 초안' : `v${context.route.to}`);
  from.value = context.route.from ?? String(context.route.to && context.route.to !== 'draft' ? Number(context.route.to) - 1 : context.revisions[0]?.version ?? 0);
  // Old versions may fall outside the latest 100 entries.
  if (!from.value) { const value=String(Number(context.route.to)-1);add(from,value,`v${value}`);from.value=value; }
  to.value = context.route.to ?? 'draft';
  const baseline = el('label','기준'), target = el('label','비교 대상');baseline.append(from);target.append(to);
  const refresh = button('비교 새로고침','small'), status = el('p','불러오는 중…','caption');status.setAttribute('role','status');
  controls.append(baseline,target,refresh);panel.append(controls,status);const body=el('div');panel.append(body);content.append(panel);
  let disposed=false, loading=false;
  const load = async () => {
    if (disposed || loading) return; loading=true;refresh.disabled=true;
    // Never leave a previous result publishable when refresh fails or is in flight.
    body.replaceChildren();status.textContent='불러오는 중…';status.className='caption';
    try {
      const params=new URLSearchParams({from:from.value,to:to.value});
      const report=await context.api<Comparison>(`/api/pipelines/${context.route.pipeline}/review?${params}`);
      if (disposed || !panel.isConnected) return;
      status.textContent=`${date(report.checked_at)} 기준 · 현재 발행 v${report.current_version}`;
      const changes=definitionChanges(report.before.definition,report.after.definition);
      body.append(el('h3',`${report.before.label} → ${report.after.label}${report.after.generation !== undefined ? ` g${report.after.generation} (기준 v${report.after.base_version})` : ''}`),el('p',changes.length ? `변경 항목 ${changes.length}개` : '설정 변경이 없습니다.','caption'));
      if (report.after.definition_hash) body.append(el('p',`대상 SHA-256: ${report.after.definition_hash}`,'caption review-hash'));
      for (const change of changes) {
        const entry=el('details',undefined,'review-change');entry.open=changes.length<=6;
        entry.append(el('summary',`${{added:'추가',removed:'삭제',changed:'변경'}[change.kind]} · ${change.path}`));
        const columns=el('div',undefined,'review-columns');
        for (const [title,value] of [['이전',change.before],['이후',change.after]] as const) {
          const column=el('section');column.append(el('h4',title),el('pre',value === undefined ? '없음' : typeof value === 'string' ? value : JSON.stringify(value,null,2)));columns.append(column);
        }
        entry.append(columns);body.append(entry);
      }
      if (report.summary) renderSummary(body,report.summary,context);renderChecks(body,report.checks);
      if (report.can_publish) {
        const publish=button('검토한 초안 발행','primary');
        publish.addEventListener('click',async()=>{
          if (publish.disabled) return;publish.disabled=true;refresh.disabled=true;from.disabled=true;to.disabled=true;
          try {
            const result=await context.api<{version:number}>(`/api/pipelines/${context.route.pipeline}/publish`,'POST',{generation:report.after.generation,base_version:report.after.base_version});
            context.message(`v${result.version}을 발행했습니다.`);
            if (!disposed && panel.isConnected) await context.navigate({...context.route,tab:'versions',from:undefined,to:undefined});
          } catch (error) {context.message(`${errorText(error)} 비교를 새로고침해 주세요.`,true);}
          finally {refresh.disabled=false;from.disabled=false;to.disabled=false;}
        });
        body.append(el('p','표시된 초안 세대와 기준 버전으로 발행합니다. 다른 편집이 있으면 서버가 발행을 중단합니다.','caption'),publish);
      }
    } catch (error) {if (!disposed && panel.isConnected) {status.textContent=`비교 조회 실패: ${errorText(error)}`;status.className='graph-error';}}
    finally {loading=false;refresh.disabled=false;}
  };
  const change = () => { void context.navigate({...context.route,from:from.value,to:to.value}).catch(error=>context.message(errorText(error),true)); };
  from.addEventListener('change',change);to.addEventListener('change',change);refresh.addEventListener('click',()=>void load());void load();
  return ()=>{disposed=true;};
}

export type RunSelection = {version:number;changelist:number;source_run_id?:string};
export function createRunPreflight(content: HTMLElement, context: Context, currentVersion: number, queue: (input: RunSelection)=>Promise<void>) {
  const details=el('details'), heading=el('summary','파이프라인 실행');details.append(heading);
  const form=el('form',undefined,'stack'), version=el('select'), cl=el('input'), source=el('input');
  for (const revision of context.revisions) {const option=el('option',`v${revision.version}`);option.value=String(revision.version);version.append(option);}version.value=String(currentVersion);version.name='version';version.setAttribute('aria-label','발행 버전');
  cl.type='number';cl.min='1';cl.step='1';cl.required=true;cl.name='changelist';source.name='source_run_id';source.placeholder='예: 00000000-0000-4000-8000-000000000000';
  for (const [text,input] of [['발행 버전',version],['제출된 CL',cl],['산출물 제공 실행 ID (입력 산출물이 있을 때)',source]] as const) {const label=el('label',text);label.append(input);form.append(label);}
  form.append(el('p','입력 산출물이 있는 버전은 성공한 제공 실행 ID와 최종 제출 CL을 지정하세요.','caption'));
  const actions=el('div',undefined,'row'), inspect=button('실행 전 점검'), submit=button('검토한 조건으로 큐에 등록','primary');submit.type='submit';submit.disabled=true;actions.append(inspect,submit);
  const status=el('p','버전과 CL을 입력한 뒤 실행 전 점검을 진행하세요.','caption');status.setAttribute('role','status');const reportHost=el('div',undefined,'preflight-report');form.append(actions,status,reportHost);details.append(form);content.append(details);
  let sequence=0, inspected:RunSelection|undefined, queueing=false;
  const selection=():RunSelection=>{
    const changelist=Number(cl.value), selected=Number(version.value), sourceId=source.value.trim();
    if (!Number.isSafeInteger(changelist)||changelist<=0||!Number.isSafeInteger(selected)||selected<=0) throw new Error('양의 정수 CL과 발행 버전을 지정하세요.');
    if (sourceId&&!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(sourceId)) throw new Error('올바른 산출물 제공 실행 UUID를 입력하세요.');
    return {version:selected,changelist,...(sourceId?{source_run_id:sourceId.toLowerCase()}: {})};
  };
  const invalidate=()=>{sequence++;inspected=undefined;submit.disabled=true;inspect.disabled=false;reportHost.replaceChildren();status.className='caption';status.textContent='입력 조건이 변경되었습니다. 실행 전 점검을 다시 진행하세요.';};
  form.addEventListener('input',invalidate);version.addEventListener('change',invalidate);source.addEventListener('change',invalidate);cl.addEventListener('change',invalidate);
  inspect.addEventListener('click',async()=>{
    if (queueing || !form.reportValidity()) return;
    const ticket=++sequence;inspected=undefined;submit.disabled=true;inspect.disabled=true;reportHost.replaceChildren();status.textContent='실행 조건을 확인하고 있습니다…';status.className='caption';
    try {
      const input=selection(), params=new URLSearchParams({version:String(input.version),changelist:String(input.changelist),...(input.source_run_id?{source_run_id:input.source_run_id}:{})});
      const report=await context.api<Preflight>(`/api/pipelines/${context.route.pipeline}/preflight?${params}`);
      if (ticket!==sequence || !details.isConnected) return;
      status.textContent=`${date(report.checked_at)} 점검 · v${report.version} · CL ${report.changelist}`;
      reportHost.append(el('p',`산출물 제공 실행: ${report.source_run_id ?? '없음'}`,'review-hash'),el('p',`설정 SHA-256: ${report.definition_hash}`,'caption review-hash'));
      renderSummary(reportHost,report.summary,context);renderChecks(reportHost,report.checks);
      if (report.artifacts.length) {const list=el('ul');for(const artifact of report.artifacts)list.append(el('li',`${artifact.name} · ${artifact.size.toLocaleString()} bytes · SHA-256 ${artifact.sha256}`,'review-hash'));reportHost.append(el('h4','확인한 입력 산출물'),list);}
      if (report.can_queue) {inspected=input;submit.disabled=false;}
    } catch(error) {if(ticket===sequence&&details.isConnected){status.textContent=`점검 실패: ${errorText(error)}`;status.className='graph-error';}}
    finally {if(ticket===sequence)inspect.disabled=false;}
  });
  form.addEventListener('submit',async event=>{
    event.preventDefault();if(queueing||!inspected||submit.disabled)return;
    try {
      if(JSON.stringify(selection())!==JSON.stringify(inspected)){invalidate();return;}
      queueing=true;submit.disabled=true;inspect.disabled=true;version.disabled=true;cl.disabled=true;source.disabled=true;
      await queue(inspected);
    } catch(error){context.message(errorText(error),true);}
    finally {queueing=false;version.disabled=false;cl.disabled=false;source.disabled=false;inspect.disabled=false;submit.disabled=!inspected;}
  });
}
