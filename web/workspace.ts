import { type Route } from './navigation.js';

type Project = { id: string; name: string };
type Run = { id: string; project_id: string; project_name: string; pipeline_id: string; pipeline_name: string; version: number; status: string; cancel_requested: boolean; changelist: number; created_at: string; finished_at: string | null; deployment_status: string | null };
type Totals = { projects: number; pipelines: number; queued: number; running: number; awaiting_approval: number; recovery_required: number; succeeded_24h: number; failed_24h: number };
type ProjectSummary = Project & Omit<Totals, 'projects' | 'pipelines'> & { pipeline_count: number };
export type Diagnostic = {run_id: string; project_id: string; project_name: string; pipeline_id: string; pipeline_name: string; version: number; changelist: number; created_at: string; finished_at?: string | null; key?: string; kind?: string; target_pipeline_id?: string | null; target_name?: string | null; target_version?: number | null; reason?: string | null; job_id?: string | null; job_name?: string | null; job_status?: string | null; failed_jobs?: number; exit_code?: number | null; cancel_requested?: boolean};
type DiagnosticGroup = {total: number; items: Diagnostic[]};
type Diagnostics = {actions: DiagnosticGroup; queued: DiagnosticGroup; failed_24h: DiagnosticGroup};
type Overview = { checked_at: string; summary: Totals; projects: ProjectSummary[]; recent: Run[]; attention: Run[]; diagnostics: Diagnostics };
type RunPage = { items: Run[]; next_cursor: string | null };
type Issue = { kind: string; title: string; detail: string; agent_id: string | null; project_id: string | null; pipeline_id: string | null; run_id: string | null; checked_at: string | null };
type Operations = { counts: { checked_at: string; active_agents: number; online_agents: number; draining_agents: number; busy_agents: number; docker_available: number }; issues: { total: number; offline_agents: number; credential_attention: number; trigger_attention: number; recovery_required: number; items: Issue[] } };
type Context = {
  route: Route; projects: Project[]; isAdmin: boolean;
  api: <T>(path: string) => Promise<T>;
  link: (text: string, route: Route, className?: string) => HTMLAnchorElement;
  navigate: (route: Route) => Promise<void>;
};
const states: Record<string, string> = { queued: '배정 대기', running: '실행 중', succeeded: '성공', failed: '실패', canceled: '취소', awaiting_approval: '승인 대기', recovery_required: '복구 확인 필요' };
function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] {
  const result = document.createElement(tag); if (text !== undefined) result.textContent = text; if (className) result.className = className; return result;
}
function time(value: string) { return new Date(value).toLocaleString('ko-KR'); }
function option(select: HTMLSelectElement, value: string, title: string) { const node = el('option', title); node.value = value; select.append(node); }
function runRoute(run: Run): Route { return {page: 'run', project: run.project_id, pipeline: run.pipeline_id, run: run.id, tab: run.deployment_status === 'awaiting_approval' || run.deployment_status === 'recovery_required' ? 'deployment' : 'graph'}; }
function runTable(items: Run[], context: Context, label: string) {
  const wrap = el('div', undefined, 'scroll');
  if (!items.length) { wrap.append(el('p', '표시할 실행이 없습니다.', 'empty')); return wrap; }
  const table = el('table', undefined, 'workspace-runs'); table.setAttribute('aria-label', label);
  const head = el('thead'); const heading = el('tr');
  for (const name of ['상태', '프로젝트 / 파이프라인', '실행', '버전 · CL', '등록 / 완료']) { const th = el('th', name); th.scope = 'col'; heading.append(th); }
  head.append(heading); const body = el('tbody');
  for (const run of items) {
    const row = el('tr'); const state = el('td');
    const status = ['awaiting_approval', 'recovery_required'].includes(run.deployment_status ?? '') ? run.deployment_status! : run.status;
    state.append(el('span', states[status] ?? status, `badge ${status === 'recovery_required' ? 'failed' : status === 'awaiting_approval' ? 'queued' : status}`));
    if (run.cancel_requested && ['queued', 'running'].includes(run.status)) state.append(el('div', '취소 요청됨', 'caption'));
    const names = el('td'); names.append(context.link(run.pipeline_name, {page: 'pipeline', project: run.project_id, pipeline: run.pipeline_id, tab: 'runs'}), el('div', run.project_name, 'caption'));
    const execution = el('td'); execution.append(context.link(run.id.slice(0, 8), runRoute(run))); const version = el('td', `v${run.version} · CL ${run.changelist}`);
    const dates = el('td', undefined, 'caption'); dates.append(el('div', time(run.created_at)), el('div', run.finished_at ? time(run.finished_at) : ['queued', 'running'].includes(run.status) ? '진행 전 또는 진행 중' : '완료 시각 없음'));
    row.append(state, names, execution, version, dates); body.append(row);
  }
  table.append(head, body); wrap.append(table); return wrap;
}
function metric(context: Context, label: string, value: number | string, detail: string, route: Route, tone = '') {
  const card = context.link('', route, `workspace-metric ${tone}`); card.append(el('span', label, 'caption'), el('strong', String(value)), el('span', detail, 'caption')); return card;
}
export function diagnosticRoute(item: Diagnostic, group: keyof Diagnostics): Route {
  return {page: 'run', project: item.project_id, pipeline: item.pipeline_id, run: item.run_id,
    tab: group === 'actions' ? (item.kind === 'followup_blocked' ? 'chain' : 'deployment') : group === 'queued' ? 'dispatch' : 'graph',
    ...(group === 'failed_24h' && item.job_id ? {job: item.job_id} : {})};
}
function diagnosticSection(group: keyof Diagnostics, data: DiagnosticGroup, context: Context) {
  const titles = {actions: '현재 확인할 항목', queued: '배정 대기', failed_24h: '최근 24시간 실패 이력'};
  const captions = {
    actions: '복구 확인 → 후속 등록 차단 → 승인 대기 순, 같은 상태에서는 선행 또는 해당 실행의 오래된 등록 순으로 최대 8건입니다.',
    queued: '승인 대기를 제외한 오래된 등록 순으로 최대 8건입니다. 대기 원인은 배정 점검에서 확인하세요.',
    failed_24h: '최근 완료 순으로 최대 8건입니다. 이후 성공 여부와 별개인 과거 실패 기록입니다.',
  };
  const panel = el('section', undefined, `card diagnostic-section diagnostic-${group}`);
  panel.setAttribute('aria-label', titles[group]);
  panel.append(el('h2', `${titles[group]} (${data.total})`), el('p', captions[group], 'caption'));
  if (!data.items.length) panel.append(el('p', group === 'actions' ? '확인이 필요한 후속 차단·배포 항목이 없습니다.' : group === 'queued' ? '배정 대기 중인 실행이 없습니다.' : '최근 24시간에 완료된 실패 실행이 없습니다.', 'empty'));
  for (const item of data.items) {
    const row = el('article', undefined, 'diagnostic-item');
    const header = el('div', undefined, 'row');
    const label = group === 'queued' ? (item.cancel_requested ? '취소 처리 대기' : '배정 대기') : group === 'failed_24h' ? '실패 이력' : item.kind === 'followup_blocked' ? '후속 등록 차단' : states[item.kind!] ?? item.kind!;
    header.append(el('span', label, `badge ${group === 'failed_24h' || item.kind === 'recovery_required' || item.kind === 'followup_blocked' ? 'failed' : 'queued'}`), el('strong', `${item.project_name} / ${item.pipeline_name}`));
    row.append(header, el('p', `v${item.version} · CL ${item.changelist} · 실행 ${item.run_id.slice(0,8)}`, 'caption'));
    if (item.kind === 'followup_blocked') {
      row.append(el('p', `후속 대상: ${item.target_name} · v${item.target_version}`), el('p', item.reason || '기록된 차단 사유가 없습니다.', 'diagnostic-reason'), el('p', '후속 실행이 생성되지 않았으며 자동 재시도하지 않습니다.', 'caption'));
    } else if (group === 'failed_24h') {
      if (item.job_id) row.append(el('p', `${item.job_status === 'lost' ? '연결 유실' : '실패'} 작업: ${item.job_name}${(item.failed_jobs ?? 0) > 1 ? ` · 실패·유실 ${item.failed_jobs}개 중 설정 순 첫 작업` : ''}${item.exit_code == null ? '' : ` · 종료 코드 ${item.exit_code}`}`));
      row.append(el('p', item.reason || '기록된 실패 사유가 없습니다. 실행 상세에서 상태와 로그를 확인하세요.', 'diagnostic-reason'));
    } else if (item.reason) row.append(el('p', item.reason, 'diagnostic-reason'));
    const dateLabel = group === 'failed_24h' ? '실행 완료' : item.kind === 'followup_blocked' && item.finished_at ? '선행 실행 완료' : '실행 등록';
    row.append(el('p', `${dateLabel} ${time(group === 'failed_24h' || dateLabel === '선행 실행 완료' ? item.finished_at! : item.created_at)}`, 'caption'));
    const action = context.link(group === 'queued' ? '배정 점검 →' : group === 'failed_24h' ? '실패 작업·로그 →' : item.kind === 'followup_blocked' ? '부모 실행의 체인 →' : '배포 승인·복구 상세 →', diagnosticRoute(item, group));
    action.dataset.diagnosticKey = `${group}:${item.key ?? item.run_id}`;
    row.append(action); panel.append(row);
  }
  if (data.total > data.items.length) panel.append(el('p', `전체 ${data.total}건 중 ${data.items.length}건 표시`, 'caption'));
  if (group !== 'actions') panel.append(context.link('전체 실행에서 더 보기 →', {page: 'runs', state: group === 'queued' ? 'queued' : 'failed'}));
  return panel;
}
function overview(body: HTMLElement, data: Overview, context: Context) {
  const stats = el('div', undefined, 'workspace-metrics'); stats.setAttribute('aria-label', '워크스페이스 실행 현황');
  const s = data.summary;
  stats.append(metric(context, '실행 중', s.running, '현재 실행 중인 파이프라인', {page: 'runs', state: 'running'}), metric(context, '배정 대기', s.queued, '승인 대기 제외', {page: 'runs', state: 'queued'}), metric(context, '승인 대기', s.awaiting_approval, '배포 입력 검토', {page: 'runs', state: 'awaiting_approval'}, s.awaiting_approval ? 'attention' : ''), metric(context, '복구 확인 필요', s.recovery_required, '배포 환경 잠금 확인', {page: 'runs', state: 'recovery_required'}, s.recovery_required ? 'attention' : ''));
  const note = el('p', `접근 가능한 프로젝트 ${s.projects}개 · 파이프라인 ${s.pipelines}개 · 최근 24시간 종료: 성공 ${s.succeeded_24h}건 / 실패 ${s.failed_24h}건`, 'muted');
  const recent = el('section', undefined, 'card'); const recentHeading = el('div', undefined, 'row space'); recentHeading.append(el('h2', '최근 실행'), context.link('전체 실행 보기 →', {page: 'runs'})); recent.append(recentHeading, runTable(data.recent, context, '최근 실행'));
  const attention = el('div', undefined, 'diagnostic-grid');
  for (const group of ['actions', 'queued', 'failed_24h'] as const) attention.append(diagnosticSection(group, data.diagnostics[group], context));
  if (context.isAdmin) attention.append(context.link('에이전트·인증·자동 빌드 운영 상태 →', {page: 'operations'}, 'diagnostic-operations'));
  const projects = el('section'); const projectHeading = el('div', undefined, 'row space'); projectHeading.append(el('h2', '프로젝트 현황'), context.link('프로젝트 관리 →', {page: 'projects'})); projects.append(projectHeading);
  const cards = el('div', undefined, 'project-cards');
  for (const p of data.projects) {
    const card = context.link('', {page: 'pipelines', project: p.id}, 'card project-card'); card.append(el('h3', p.name), el('p', `파이프라인 ${p.pipeline_count}개`, 'caption'), el('div', `실행 중 ${p.running} · 배정 대기 ${p.queued}`, 'caption'), el('div', `승인 대기 ${p.awaiting_approval} · 복구 확인 ${p.recovery_required}`, 'caption')); cards.append(card);
  }
  if (!data.projects.length) cards.append(el('p', '접근 가능한 프로젝트가 없습니다. 프로젝트 권한을 받은 뒤 실행 현황을 확인할 수 있습니다.', 'card'));
  projects.append(cards);
  const focused = document.activeElement instanceof HTMLElement ? document.activeElement.dataset.diagnosticKey : undefined;
  body.replaceChildren(stats, note, attention, recent, projects);
  if (focused) {
    const target = Array.from(body.querySelectorAll<HTMLElement>('[data-diagnostic-key]')).find(node => node.dataset.diagnosticKey === focused);
    body.tabIndex = -1; (target ?? body).focus({preventScroll: true});
  }
}
function operations(body: HTMLElement, data: Operations, context: Context) {
  const {counts: c, issues: i} = data; const stats = el('div', undefined, 'workspace-metrics'); stats.setAttribute('aria-label', '운영 상태 요약');
  stats.append(metric(context, '온라인 에이전트', `${c.online_agents} / ${c.active_agents}`, `오프라인 ${i.offline_agents}대`, {page: 'agents', state: 'online'}, i.offline_agents ? 'attention' : ''), metric(context, 'Perforce 인증 확인', i.credential_attention, '관리형 인증을 사용하는 에이전트', {page: 'agents'}, i.credential_attention ? 'attention' : ''), metric(context, '자동 빌드 확인', i.trigger_attention, '오류 또는 150초 이상 지연', {page: 'operations'}, i.trigger_attention ? 'attention' : ''), metric(context, '복구 확인 필요', i.recovery_required, '환경 잠금이 유지된 배포', {page: 'runs', state: 'recovery_required'}, i.recovery_required ? 'attention' : ''));
  const detail = el('p', `작업 수행 ${c.busy_agents}대 · 새 작업 중지 ${c.draining_agents}대 · Docker 사용 가능 ${c.docker_available}대`, 'muted');
  const panel = el('section', undefined, 'card'); panel.append(el('h2', `확인할 항목 (${i.total})`));
  if (!i.items.length) panel.append(el('p', '기록된 에이전트·인증·자동 빌드·배포 상태에 확인할 항목이 없습니다.', 'empty'));
  const issueNames: Record<string, string> = {agent_offline: '에이전트 연결', credential: 'Perforce 인증', trigger: '자동 빌드', deployment: '배포 복구'};
  for (const issue of i.items) {
    const row = el('article', undefined, 'operation-issue');
    const route: Route = issue.kind === 'deployment' ? {page: 'run', project: issue.project_id!, pipeline: issue.pipeline_id!, run: issue.run_id!, tab: 'deployment'} : issue.kind === 'trigger' ? {page: 'pipeline', project: issue.project_id!, pipeline: issue.pipeline_id!, tab: 'trigger'} : {page: 'agents', agent: issue.agent_id ?? undefined, state: 'all'};
    row.append(el('span', issueNames[issue.kind] ?? issue.kind, 'badge queued'), context.link(issue.title, route), el('p', issue.detail), el('span', issue.checked_at ? `마지막 확인 ${time(issue.checked_at)}` : '확인 시각 없음 · 상세 화면에서 확인', 'caption')); panel.append(row);
  }
  if (i.total > i.items.length) panel.append(el('p', `우선 확인할 ${i.items.length}건을 표시합니다. 전체 ${i.total}건은 각 관리 화면에서 확인하세요.`, 'caption'));
  const limit = el('p', '서버에 마지막으로 기록된 상태입니다. 배포 대상 서비스의 실시간 응답은 이 화면에서 직접 검사하지 않습니다. Docker 미설치만으로 에이전트 장애로 판단하지 않습니다.', 'caption');
  body.replaceChildren(stats, detail, context.link('실행 실패·후속 차단·승인 대기 확인 →', {page: 'overview'}, 'button small'), panel, limit);
}

/** Starts polling immediately; disposal prevents late replies from repainting another route. */
export function createWorkspaceView(content: HTMLElement, context: Context): () => void {
  const page = context.route.page;
  const titles: Record<string, [string, string]> = {overview: ['개요', '접근 가능한 프로젝트의 실행 현황과 확인할 작업을 한눈에 봅니다.'], runs: ['전체 실행', '프로젝트를 넘나들며 실행을 검색하고, 과거 기록까지 확인합니다.'], operations: ['운영 상태', '공용 에이전트와 인증·자동 빌드·배포 상태를 확인합니다.']};
  const [title, description] = titles[page]; content.append(el('h1', title), el('p', description, 'muted page-description'));
  if (page === 'operations' && !context.isAdmin) { content.append(el('p', '서버 관리자만 조회할 수 있습니다.', 'card')); return () => {}; }
  const controls = el('div', undefined, 'row workspace-refresh'); const status = el('span', '불러오는 중…', 'caption'); status.setAttribute('role', 'status');
  const refresh = el('button', '새로고침', 'small'); refresh.type = 'button';
  const toggleLabel = el('label', '15초 자동 갱신', 'retention-toggle'); const toggle = el('input'); toggle.type = 'checkbox'; toggle.checked = page !== 'runs'; toggleLabel.prepend(toggle);
  controls.append(refresh); if (page !== 'runs') controls.append(toggleLabel); controls.append(status);
  if (page === 'runs') {
    const filters = el('form', undefined, 'agent-toolbar card'); const qLabel = el('label', '실행 검색'); const search = el('input'); search.type = 'search'; search.placeholder = '프로젝트 · 파이프라인 · 실행 ID'; search.maxLength = 120; search.value = context.route.q ?? ''; qLabel.append(search);
    const projectLabel = el('label', '프로젝트'); const project = el('select'); option(project, '', '모든 접근 가능한 프로젝트'); for (const p of context.projects) option(project, p.id, p.name); project.value = context.route.project ?? ''; projectLabel.append(project);
    const stateLabel = el('label', '실행 상태'); const state = el('select'); option(state, '', '전체 상태'); for (const [value, label] of Object.entries(states)) option(state, value, label); state.value = context.route.state ?? ''; stateLabel.append(state);
    const apply = el('button', '조회', 'primary'); apply.type = 'submit'; filters.append(qLabel, projectLabel, stateLabel, apply);
    filters.addEventListener('submit', event => { event.preventDefault(); void context.navigate({page: 'runs', project: project.value || undefined, q: search.value.trim() || undefined, state: state.value || undefined}).catch(error => {status.textContent = String(error);}); }); content.append(filters);
  }
  content.append(controls); const body = el('div'); content.append(body);
  let disposed = false; let loading = false; let checked: string | undefined;
  const refreshData = async () => {
    if (disposed || loading) return;
    loading = true; refresh.disabled = true; status.textContent = checked ? `${time(checked)} 기준 · 갱신 중…` : '불러오는 중…';
    try {
      if (page === 'overview') {
        const result = await context.api<Overview>('/api/workspace/overview'); if (disposed || !content.isConnected) return; overview(body, result, context); checked = result.checked_at;
      } else if (page === 'operations') {
        const result = await context.api<Operations>('/api/operations'); if (disposed || !content.isConnected) return; operations(body, result, context); checked = result.counts.checked_at;
      } else {
        const params = new URLSearchParams(); for (const key of ['project', 'q', 'state', 'cursor'] as const) if (context.route[key]) params.set(key, context.route[key]!);
        const result = await context.api<RunPage>(`/api/workspace/runs?${params}`); if (disposed || !content.isConnected) return;
        const panel = el('section', undefined, 'card'); panel.append(runTable(result.items, context, '전체 실행'));
        const paging = el('nav', undefined, 'row space workspace-pagination'); paging.setAttribute('aria-label', '실행 페이지'); paging.append(el('span', `${result.items.length}건 표시 · 최신 등록 순`, 'caption'));
        if (context.route.cursor) paging.append(context.link('최신 실행으로', {...context.route, cursor: undefined}, 'button small'));
        if (result.next_cursor) paging.append(context.link('이전 실행 보기 →', {...context.route, cursor: result.next_cursor}, 'button small'));
        panel.append(paging); body.replaceChildren(panel); checked = new Date().toISOString();
      }
      status.className = 'caption'; status.textContent = `${time(checked!)} 기준`;
    } catch (error) {
      if (!disposed && content.isConnected) { status.className = 'graph-error'; status.textContent = `갱신 실패${checked ? ` · ${time(checked)}의 마지막 결과입니다` : ''}: ${error instanceof Error ? error.message : String(error)}`; }
    } finally { loading = false; refresh.disabled = false; }
  };
  refresh.addEventListener('click', () => {void refreshData();});
  const visible = () => { if (!document.hidden && toggle.checked) void refreshData(); };
  const timer = page === 'runs' ? undefined : window.setInterval(visible, 15000);
  toggle.addEventListener('change', visible); document.addEventListener('visibilitychange', visible); void refreshData();
  return () => {disposed = true; if (timer !== undefined) clearInterval(timer); document.removeEventListener('visibilitychange', visible);};
}
