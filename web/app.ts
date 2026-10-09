import { parseRoute, routeHash, matchesPipeline, type Route } from './navigation.js';
import { connectionStatus, dockerStatus, matchesAgent, type AgentStatus, type AgentFilters } from './agent-status.js';
import { createRunGraph, type GraphJob, type GraphLog } from './run-graph.js';
import { createWorkspaceView } from './workspace.js';
import { createDispatchView } from './dispatch.js';
import { createConnectionsView } from './pipeline-connections.js';
import { createProjectGraph } from './project-graph.js';
import { createRunChainView } from './run-chain.js';
import { createReviewView, createRunPreflight } from './pipeline-review.js';
import { createPipelineEditor, type EditorHandle, type EditorDefinition } from './pipeline-editor.js';

type Project = { id: string; name: string; role: string };
type Pipeline = { id: string; project_id: string; name: string; current_version: number; latest_run_id?: string | null; latest_run_status?: string | null; latest_run_at?: string | null; trigger_enabled?: boolean };
type SourceRoot = { id: string; name: string; connection_id: string; definition: { kind: string; stream?: string; view?: string[] } };
type Connection = { id: string; name: string; definition: { port: string; user: string } };
type User = { id: string; email: string; name: string; is_admin: boolean };
type Draft = { generation: number; base_version: number; definition?: EditorDefinition | null };
type Configuration = { name: string; source_root_id?: string; category: string; runner_os: string; runner_arch: string; needs: string[]; checkout_view?: string; changes: string[]; working_directory: string; stages: string[]; workspace: string; max_parallel_jobs: number; jobs: { name: string; stage: string; needs: string[]; script: string[]; timeout_seconds: number }[] };
type ConfigurationDraft = { generation: number; toml: string; definition: Configuration; source_name: string; source_hash: string; readiness: string[]; execution_ready: boolean };
type Detail = { pipeline: Pipeline; draft: Draft; draft_toml: string | null; current_revision: { version: number; definition_hash: string } | null; configuration_draft: ConfigurationDraft | null };
type Revision = { version: number; definition_hash: string; created_at: string };
type Run = { id: string; pipeline_id: string; version: number; status: string; cancel_requested: boolean; definition?: { deployment?: { environment_id: string } }; source_snapshot: { changelist: number }; created_at: string };
type BuildAgent = AgentStatus & { id: string; name: string; pool: string; os: string; arch: string; active: boolean; draining: boolean; last_seen_at: string | null; binary_sha256: string | null; agent_version: string | null; self_patch_ready: boolean; perforce_managed: boolean };
type PerforceHealth = { agent_id: string; credential_id: string; role: string; connection: { port: string; user: string }; server_state: string; server_checked_at: string | null; server_expires_at: string | null; renewed_at: string | null; agent_state: string; agent_checked_at: string | null; agent_expires_at: string | null };
type AgentRelease = { id: string; version: string; os: string; arch: string; sha256: string; size: number };
type AgentUpdate = { id: string; agent_id: string; release_id: string; status: string; reason: string; created_at: string };

const app = document.querySelector<HTMLElement>('#app')!;
const identity = document.querySelector<HTMLElement>('#identity')!;
const notice = document.querySelector<HTMLElement>('#notice')!;
let user: User;
let projects: Project[] = [];
let connections: Connection[] = [];
let roots: SourceRoot[] = [];
let project: Project | undefined;
let selected: Detail | undefined;
let pendingRun: { request_id: string; changelist: number; version: number; source_run_id?: string } | undefined;
let currentRoute: Route = { page: 'overview' };
let currentRun: Run | undefined;
let disposeRunGraph: (() => void) | undefined;
let disposeAgentsView: (() => void) | undefined;
let disposeWorkspaceView: (() => void) | undefined;
let disposeDispatchView: (() => void) | undefined;
let disposeConnectionsView: (() => void) | undefined;
let disposeProjectGraph: (() => void) | undefined;
let disposeRunChainView: (() => void) | undefined;
let disposeReviewView: (() => void) | undefined;
let dashboardGeneration = 0;
let pipelineEditor: EditorHandle | undefined;
let editorHash = '';
function leaveEditor() {
  if (pipelineEditor && !pipelineEditor.canLeave()) {
    history.replaceState(null, '', editorHash);
    const picker = document.querySelector<HTMLSelectElement>('[aria-label="프로젝트 선택"]');
    if (picker) picker.value = project?.id ?? '';
    return false;
  }
  pipelineEditor?.dispose(); pipelineEditor = undefined; return true;
}

function link(text: string, route: Route, className = '') {
  const node = el('a', text, className); node.href = routeHash(route); return node;
}
async function navigate(route: Route) {
  const hash = routeHash(route);
  if (location.hash !== hash) history.pushState(null, '', hash);
  await dashboard();
}
function sectionHeading(content: HTMLElement, title: string, description?: string) {
  content.append(el('h1', title));
  if (description) content.append(el('p', description, 'muted page-description'));
}
function tabs(content: HTMLElement, items: [string, string][], route: Route, label: string) {
  const nav = el('nav', undefined, 'detail-tabs'); nav.setAttribute('aria-label', label);
  for (const [tab, title] of items) {
    const item = link(title, { ...route, tab });
    if (route.tab === tab) item.setAttribute('aria-current', 'page');
    nav.append(item);
  }
  content.append(nav);
}
function navigation() {
  const shell = el('div', undefined, 'app-shell');
  const sidebar = el('aside', undefined, 'sidebar');
  const nav = el('nav', undefined, 'sidebar-nav'); nav.setAttribute('aria-label', '주 메뉴');
  const add = (label: string, route: Route, active: boolean) => {
    const item = link(label, route); if (active) item.setAttribute('aria-current', 'page'); nav.append(item);
  };
  nav.append(el('p', '워크스페이스', 'nav-group'));
  add('개요', { page: 'overview' }, currentRoute.page === 'overview');
  add('프로젝트', { page: 'projects' }, currentRoute.page === 'projects');
  add('전체 실행', { page: 'runs' }, currentRoute.page === 'runs');
  nav.append(el('p', '선택한 프로젝트', 'nav-group'));
  const label = el('label', '프로젝트 선택', 'project-picker'); const select = el('select');
  option(select, '', '프로젝트 선택'); for (const item of projects) option(select, item.id, item.name);
  select.value = project?.id ?? '';
  select.addEventListener('change', () => {
    const page = ['settings', 'environments', 'ci-settings', 'pipeline-graph'].includes(currentRoute.page) ? currentRoute.page : 'pipelines';
    void navigate(select.value ? { page, project: select.value, tab: page === 'settings' ? currentRoute.tab : undefined } : { page: 'projects' }).catch(reportError);
  });
  label.append(select); nav.append(label);
  if (project) {
    add('파이프라인', { page: 'pipelines', project: project.id }, ['pipelines', 'pipeline', 'run'].includes(currentRoute.page));
    add('전체 파이프라인 뷰', { page: 'pipeline-graph', project: project.id }, currentRoute.page === 'pipeline-graph');
    add('CI 설정', { page: 'ci-settings', project: project.id }, currentRoute.page === 'ci-settings');
    add('배포 환경', { page: 'environments', project: project.id }, currentRoute.page === 'environments');
    add('프로젝트 설정', { page: 'settings', project: project.id }, currentRoute.page === 'settings');
  } else nav.append(el('p', '프로젝트를 선택하면 작업 메뉴가 표시됩니다.', 'caption'));
  if (user.is_admin) {
    nav.append(el('p', '관리', 'nav-group'));
    add('에이전트', { page: 'agents' }, currentRoute.page === 'agents');
    add('Perforce 연결', { page: 'connections' }, currentRoute.page === 'connections');
    add('운영 상태', { page: 'operations' }, currentRoute.page === 'operations');
  }
  sidebar.append(nav); const content = el('section', undefined, 'page-content'); content.id = 'content';
  shell.append(sidebar, content); app.replaceChildren(shell); return content;
}
function breadcrumbs(content: HTMLElement) {
  const nav = el('nav', undefined, 'breadcrumbs'); nav.setAttribute('aria-label', '현재 위치');
  nav.append(link('프로젝트', { page: 'projects' }));
  if (project) nav.append(el('span', '/'), link(project.name, { page: 'pipelines', project: project.id }));
  if (selected) nav.append(el('span', '/'), link(selected.pipeline.name, { page: 'pipeline', project: project?.id, pipeline: selected.pipeline.id }));
  if (currentRun) nav.append(el('span', '/'), el('span', `실행 ${currentRun.id.slice(0, 8)}`));
  content.append(nav);
}
function reportError(error: unknown) { message(error instanceof Error ? error.message : String(error), true); }

function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function message(text: string, error = false) {
  notice.textContent = text; notice.className = error ? 'error' : ''; notice.hidden = false;
}
async function api<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET') {
    const cookie = document.cookie.split('; ').find(value => value.startsWith('p4forge_csrf='));
    if (cookie) headers['X-CSRF-Token'] = decodeURIComponent(cookie.slice('p4forge_csrf='.length));
  }
  const response = await fetch(path, { method, credentials: 'same-origin', headers, body: body === undefined ? undefined : JSON.stringify(body) });
  if (!response.ok) {
    let error = `요청 실패 (${response.status})`;
    try { error = (await response.json()).error ?? error; } catch { /* non-JSON framework errors */ }
    if (response.status === 401) error = '로그인이 만료되었습니다. 새로고침 후 다시 로그인해 주세요.';
    throw Object.assign(new Error(error), {status: response.status});
  }
  return response.json() as Promise<T>;
}
function button(text: string, action: () => Promise<void>, className?: string) {
  const node = el('button', text, className); node.type = 'button';
  node.addEventListener('click', async () => {
    node.disabled = true;
    try { await action(); } catch (error) { message(String(error instanceof Error ? error.message : error), true); }
    finally { node.disabled = false; }
  });
  return node;
}
function field(form: HTMLFormElement, text: string, name: string, value = '', type = 'text') {
  const label = el('label', text); const input = el('input');
  input.name = name; input.value = value; input.type = type; input.required = true;
  label.append(input); form.append(label); return input;
}
function form(title: string, submit: (data: FormData) => Promise<void>) {
  const details = el('details'); details.append(el('summary', title));
  const node = el('form', undefined, 'stack'); details.append(node);
  node.addEventListener('submit', async event => {
    event.preventDefault(); const submitButton = node.querySelector<HTMLButtonElement>('button')!; submitButton.disabled = true;
    try { await submit(new FormData(node)); } catch (error) { message(String(error instanceof Error ? error.message : error), true); }
    finally { submitButton.disabled = false; }
  });
  return { details, node };
}
function submit(node: HTMLFormElement, text: string) { const button = el('button', text, 'primary'); button.type = 'submit'; node.append(button); }
function option(select: HTMLSelectElement, value: string, name: string) { const node = el('option', name); node.value = value; select.append(node); }
function date(value: string) { return new Date(value).toLocaleString('ko-KR'); }
function writable() { return user.is_admin || ['editor', 'admin'].includes(project?.role ?? ''); }
function runnable() { return user.is_admin || ['operator', 'editor', 'admin'].includes(project?.role ?? ''); }

async function dashboard() {
  if (!leaveEditor()) return;
  const generation = ++dashboardGeneration;
  disposeRunGraph?.(); disposeRunGraph = undefined;
  disposeAgentsView?.(); disposeAgentsView = undefined;
  disposeWorkspaceView?.(); disposeWorkspaceView = undefined;
  disposeDispatchView?.(); disposeDispatchView = undefined;
  disposeConnectionsView?.(); disposeConnectionsView = undefined;
  disposeProjectGraph?.(); disposeProjectGraph = undefined;
  disposeReviewView?.(); disposeReviewView = undefined;
  disposeRunChainView?.(); disposeRunChainView = undefined;
  app.replaceChildren(el('p', '화면을 불러오는 중…', 'empty'));
  const route = parseRoute(location.hash);
  try {
    const loadedProjects = await api<Project[]>('/api/projects');
    let detail: Detail | undefined; let run: Run | undefined;
    if (route.page === 'run') {
      if (!route.run) throw new Error('실행 ID가 없는 주소입니다.');
      run = await api<Run>(`/api/runs/${encodeURIComponent(route.run)}`);
      if (route.pipeline && route.pipeline !== run.pipeline_id) throw new Error('실행과 파이프라인이 일치하지 않습니다.');
      route.pipeline = run.pipeline_id;
    }
    if (route.page === 'pipeline' || route.page === 'run' || route.page === 'ci-settings' && route.pipeline) {
      if (!route.pipeline) throw new Error('파이프라인 ID가 없는 주소입니다.');
      detail = await api<Detail>(`/api/pipelines/${encodeURIComponent(route.pipeline)}`);
      if (route.project && route.project !== detail.pipeline.project_id) throw new Error('파이프라인과 프로젝트가 일치하지 않습니다.');
      route.project = detail.pipeline.project_id;
    }
    if (generation !== dashboardGeneration) return;
    if (selected?.pipeline.id !== detail?.pipeline.id) pendingRun = undefined;
    projects = loadedProjects; selected = detail; currentRun = run; currentRoute = route;
    project = projects.find(item => item.id === route.project); roots = []; connections = [];
    if (route.project && !project) throw new Error('프로젝트가 없거나 접근 권한이 없습니다.');
    const content = navigation();
    if (project) breadcrumbs(content);
    if (['agents', 'connections', 'operations'].includes(route.page) && !user.is_admin) {
      sectionHeading(content, '접근 권한이 없습니다', '서버 관리자만 사용할 수 있는 메뉴입니다.'); return;
    }
    if (['overview', 'runs', 'operations'].includes(route.page)) disposeWorkspaceView = createWorkspaceView(content, {route, projects, isAdmin: user.is_admin, api, link, navigate});
    else if (route.page === 'agents') await agentsView(content);
    else if (route.page === 'connections') {
      const loadedConnections = await api<Connection[]>('/api/connections');
      if (generation === dashboardGeneration) { connections = loadedConnections; connectionsView(content); }
    } else if (route.page === 'projects' || !project) projectsView(content);
    else await projectView(content);
    if (generation === dashboardGeneration) document.title = `${content.querySelector('h1')?.textContent ?? '프로젝트'} · P4Forge`;
  } catch (error) {
    if (generation !== dashboardGeneration) return;
    const panel = el('section', undefined, 'card'); panel.append(el('h1', '화면을 불러올 수 없습니다'), el('p', error instanceof Error ? error.message : String(error)), link('프로젝트로 이동', { page: 'projects' }, 'button'));
    app.replaceChildren(panel); reportError(error);
  }
}
function projectsView(content: HTMLElement) {
  sectionHeading(content, '프로젝트', '프로젝트를 선택해 파이프라인과 배포 환경을 확인하세요.');
  const list = el('div', undefined, 'project-cards');
  for (const item of projects) {
    const card = link('', { page: 'pipelines', project: item.id }, 'card project-card');
    card.append(el('h2', item.name), el('span', ({ viewer: '조회', operator: '실행', editor: '편집', admin: '관리' } as Record<string, string>)[item.role] ?? (user.is_admin ? '서버 관리자' : item.role), 'badge'), el('p', '파이프라인 보기 →', 'caption')); list.append(card);
  }
  content.append(list);
  if (!projects.length) content.append(el('p', '접근 가능한 프로젝트가 없습니다. 관리자에게 프로젝트 권한을 요청하세요.', 'card'));
  if (user.is_admin) {
    const create = form('프로젝트 추가', async data => {
      const result = await api<Project>('/api/projects', 'POST', { name: data.get('name') });
      await navigate({ page: 'pipelines', project: result.id }); message('프로젝트를 생성했습니다.');
    });
    create.details.className = 'card'; field(create.node, '이름 (영문·숫자·_·-)', 'name'); submit(create.node, '프로젝트 생성'); content.append(create.details);
  }
}
function connectionsView(content: HTMLElement) {
  sectionHeading(content, 'Perforce 연결', '워크스페이스의 Perforce 서버 연결을 관리합니다.');
    const panel = el('div', undefined, 'card'); panel.append(el('h2', 'Perforce 연결'));
    for (const connection of connections) {
      const row = el('div', undefined, 'stack'); row.append(el('strong', connection.name), el('span', `${connection.definition.port} · ${connection.definition.user}`, 'caption'));
      row.append(button('연결 확인', async () => { const result = await api<unknown>(`/api/connections/${connection.id}/check`, 'POST'); message(JSON.stringify(result, null, 2)); }, 'small')); panel.append(row, el('hr'));
    }
    const create = form('연결 추가', async data => {
      await api('/api/connections', 'POST', { name: data.get('name'), definition: { port: data.get('port'), user: data.get('user') } }); await dashboard(); message('연결을 저장했습니다. ticket과 SSL trust는 서버에서 관리합니다.');
    });
    field(create.node, '연결 이름', 'name', 'local-p4'); field(create.node, 'P4PORT', 'port', 'perforce:1666'); field(create.node, 'P4USER', 'user', 'p4forge'); submit(create.node, '연결 저장'); panel.append(create.details); content.append(panel);
}
async function projectView(content: HTMLElement) {
  const id = project!.id; const projectName = project!.name; const route = currentRoute;
  if (route.page === 'pipeline-graph') {
    sectionHeading(content, '전체 파이프라인 뷰', `${projectName} · 연결되지 않은 파이프라인과 미발행 초안까지 함께 확인합니다.`);
    disposeProjectGraph = createProjectGraph(content, {route, api, link, onState: patch => {
      if (!content.isConnected) return;
      currentRoute = {...currentRoute,...patch}; history.replaceState(null,'',routeHash(currentRoute));
    }}); return;
  }
  if (route.page === 'ci-settings') { await ciSettingsView(content); return; }
  if (route.page === 'pipeline' || route.page === 'run') {
    if (route.page === 'run') await runPage(content);
    else {
      if (route.tab === 'settings') {
        const sourceRoots = await api<SourceRoot[]>(`/api/projects/${id}/roots`);
        if (!content.isConnected) return;
        roots = sourceRoots;
      }
      if (content.isConnected) await editor(content);
    }
    return;
  }
  if (route.page === 'settings') {
    sectionHeading(content, '프로젝트 설정', project!.name);
    const items: [string, string][] = [['sources', '소스 루트'], ['configuration', '구성 가져오기·내보내기'], ['retention', '산출물 보존 정책']];
    if (user.is_admin || project!.role === 'admin') items.push(['members', '멤버·접근 권한']);
    tabs(content, items, route, '프로젝트 설정');
    if (route.tab === 'retention') { await retentionView(content, id); return; }
    if (route.tab === 'members') {
      if (user.is_admin || project!.role === 'admin') await membersView(content, id);
      else content.append(el('p', '프로젝트 관리자만 접근 권한을 관리할 수 있습니다.', 'card'));
      return;
    }
    const panel = el('section', undefined, 'card');
    if (route.tab === 'configuration') {
      panel.append(el('h2', '구성 가져오기·내보내기'));
      if (writable()) {
        const importForm = form('프로젝트 TOML 가져오기', async data => {
          const result = await api<{ pipeline_ids: string[]; already_imported: boolean }>(`/api/projects/${id}/configuration`, 'POST', { source_name: data.get('source_name'), toml: data.get('toml') });
          if (result.pipeline_ids[0]) await selectPipeline(result.pipeline_ids[0], 'settings');
          message(result.already_imported ? '이미 가져온 설정입니다. 기존 DB 초안을 표시합니다.' : `${result.pipeline_ids.length}개 파이프라인을 DB 초안으로 가져왔습니다. 실행 준비 항목을 확인하세요.`);
        });
        field(importForm.node, '설정 파일 이름', 'source_name', 'root.p4forge.toml');
        const input = el('textarea'); input.name = 'toml'; input.required = true; input.setAttribute('aria-label', '프로젝트 설정 TOML'); importForm.node.append(input);
        submit(importForm.node, 'DB로 가져오기'); panel.append(importForm.details);
      }
      panel.append(button('프로젝트 TOML 내려받기', async () => {
        const result = await api<{ toml: string }>(`/api/projects/${id}/configuration`);
        downloadToml(`${projectName}.p4forge.toml`, result.toml);
      }, 'small'));

      content.append(panel); return;
    }
    const [sourceRoots, loadedConnections] = await Promise.all([api<SourceRoot[]>(`/api/projects/${id}/roots`), user.is_admin ? api<Connection[]>('/api/connections') : Promise.resolve([])]);
    if (!content.isConnected) return;
    roots = sourceRoots; connections = loadedConnections;
    panel.append(el('p', '빌드할 Stream·View 범위를 관리합니다. 서버 주소와 계정은 관리 → Perforce 연결에서 설정합니다.', 'caption'));
    const sources = el('details'); sources.open = true; sources.append(el('summary', `소스 루트 (${roots.length})`));
    for (const root of roots) {
      sources.append(el('p', `${root.name} · ${root.definition.stream ?? (root.definition.kind === 'view' ? 'Perforce View' : 'Classic')}`));
      if (root.definition.view) sources.append(el('pre', root.definition.view.join('\n')));
      sources.append(el('pre', root.id));
    }
    if (user.is_admin && connections.length) {
      const create = form('소스 루트 추가', async data => {
        const definition = data.get('kind') === 'view'
          ? { kind: 'view', view: String(data.get('view') ?? '').split(/\r?\n/).map(line => line.trim()).filter(Boolean) }
          : { kind: 'stream', stream: data.get('stream') };
        await api(`/api/projects/${id}/roots`, 'POST', { name: data.get('name'), connection_id: data.get('connection'), definition }); await dashboard(); message('소스 루트를 생성했습니다. 아래 UUID를 설정의 source_root_id로 사용합니다.');
      });
      field(create.node, '이름', 'name', 'main-source'); const label = el('label', '연결'); const select = el('select'); select.name = 'connection'; for (const connection of connections) option(select, connection.id, connection.name); label.append(select); create.node.append(label);
      const kindLabel = el('label', '소스 종류'); const kind = el('select'); kind.name = 'kind'; kind.setAttribute('aria-label', '소스 종류'); option(kind, 'stream', 'Stream'); option(kind, 'view', 'Perforce View'); kindLabel.append(kind); create.node.append(kindLabel);
      const stream = field(create.node, 'Stream', 'stream', '//streams/main');
      const viewFields = el('div'); const viewLabel = el('label', 'Perforce View'); const view = el('textarea'); view.name = 'view'; view.rows = 5; view.setAttribute('aria-label', 'Perforce View');
      view.placeholder = '//stream/root/Server/... ...\n-//stream/root/Client/...';
      viewLabel.append(view); viewFields.append(viewLabel, el('p', '한 줄에 포함 경로와 작업 폴더 경로를 입력합니다. 오른쪽 ...는 작업 폴더 최상위입니다. -로 시작하면 제외하며 오른쪽 경로는 생략할 수 있습니다. 위에서 아래로 적용하며 뒤의 규칙이 우선합니다.', 'caption'), el('p', '경로는 /...로 끝내고 공백이 있으면 큰따옴표로 감싸세요. 최신 Agent에서 실행할 수 있습니다.', 'caption')); create.node.append(viewFields);
      const selectKind = () => { const isView = kind.value === 'view'; stream.disabled = isView; stream.required = !isView; stream.parentElement!.style.display = isView ? 'none' : ''; view.disabled = !isView; view.required = isView; viewFields.style.display = isView ? '' : 'none'; };
      kind.addEventListener('change', selectKind); selectKind();
      submit(create.node, '소스 루트 저장'); sources.append(create.details);
    }
    if (writable() && roots.length) {
      const bind = form('프로젝트 설정의 Perforce 소스 연결', async data => {
        const snapshot = await api<{ generations: Record<string, number> }>(`/api/projects/${id}/configuration`);
        await api(`/api/projects/${id}/configuration/source`, 'PUT', { source_root_id: data.get('source_root_id'), generations: snapshot.generations });
        await dashboard(); message('프로젝트의 모든 설정 초안에 Perforce 소스를 연결했습니다.');
      });
      const label = el('label', '설정에 연결할 소스'); const select = el('select'); select.name = 'source_root_id';
      for (const root of roots) option(select, root.id, `${root.name} · ${root.definition.stream ?? (root.definition.kind === 'view' ? 'Perforce View' : 'classic')}`);
      label.append(select); bind.node.append(label, el('p', '이 프로젝트의 모든 구성 초안에 적용합니다. 발행 버전과 기존 실행은 유지됩니다.', 'caption'));
      submit(bind.node, '전체 설정에 소스 연결'); sources.append(bind.details);
    }
    panel.append(el('hr'), sources); content.append(panel);

    return;
  }
  const pipelines = await api<Pipeline[]>(`/api/projects/${id}/pipelines`);
  if (!content.isConnected) return;
  if (route.page === 'environments') {
    sectionHeading(content, '배포 환경', '승인과 배포 상태를 확인하고 환경별 설정을 관리하세요.');
    await environmentsView(content, id, pipelines); return;
  }
  pipelineList(content, pipelines);
}
async function membersView(content: HTMLElement, id: string) {
  if (user.is_admin || project!.role === 'admin') {
    const members = await api<{ user_id: string; email: string; role: string }[]>(`/api/projects/${id}/members`);
    const people = user.is_admin ? await api<User[]>('/api/users') : members.map(member => ({ id: member.user_id, email: member.email }));
    if (!content.isConnected) return;
    const access = el('details'); access.className = 'card'; access.open = true; access.append(el('summary', '프로젝트 접근 권한'));
    for (const member of members) access.append(el('p', `${member.email} · ${member.role}`, 'caption'));
    const edit = form('역할 지정', async data => { await api(`/api/projects/${id}/members`, 'PUT', { user_id: data.get('user'), role: data.get('role') }); await dashboard(); message('프로젝트 역할을 저장했습니다.'); });
    const personLabel = el('label', '사용자'); const personSelect = el('select'); personSelect.name = 'user'; for (const person of people) option(personSelect, person.id, person.email); personLabel.append(personSelect); edit.node.append(personLabel);
    const roleLabel = el('label', '역할'); const roleSelect = el('select'); roleSelect.name = 'role'; for (const [value, name] of [['viewer', 'Viewer · 조회'], ['operator', 'Operator · 실행 등록·취소'], ['editor', 'Editor · 설정 편집·발행'], ['admin', 'Admin · 프로젝트 권한 관리']]) option(roleSelect, value!, name!); roleLabel.append(roleSelect); edit.node.append(roleLabel); submit(edit.node, '역할 저장'); access.append(edit.details);
    access.append(el('p', '사용자는 먼저 Google Workspace로 로그인해야 합니다. 전역 관리자가 새 사용자를 프로젝트에 추가할 수 있습니다.', 'caption')); content.append(access);
  }

}
const runNames: Record<string, string> = { queued: '대기', running: '실행 중', succeeded: '성공', failed: '실패', lost: '연결 유실', canceled: '취소', skipped: '건너뜀' };
function pipelineList(content: HTMLElement, pipelines: Pipeline[]) {
  const projectId = project!.id;
  sectionHeading(content, '파이프라인', `${project!.name} · ${pipelines.length}개 파이프라인`);
  content.append(link('전체 파이프라인 뷰 →', {page:'pipeline-graph',project:projectId,q:currentRoute.q}, 'button graph-entry'));
  const panel = el('section', undefined, 'card'); const toolbar = el('div', undefined, 'agent-toolbar');
  const label = el('label', '파이프라인 검색'); const search = el('input'); search.type = 'search'; search.placeholder = '파이프라인 이름'; search.value = currentRoute.q ?? ''; label.append(search);
  const filterLabel = el('label', '표시 조건'); const filter = el('select');
  for (const [value, title] of [['', '전체'], ['published', '발행됨'], ['draft', '미발행'], ['running', '최근 실행 대기·실행 중'], ['failed', '최근 실행 실패'], ['automatic', '자동 빌드 켜짐']]) option(filter, value, title);
  filter.value = currentRoute.state ?? ''; filterLabel.append(filter); toolbar.append(label, filterLabel, button('새로고침', dashboard, 'small')); panel.append(toolbar);
  const scroll = el('div', undefined, 'scroll'); const table = el('table', undefined, 'pipeline-table'); table.setAttribute('aria-label', '파이프라인 목록');
  const head = el('thead'); const heading = el('tr'); for (const title of ['파이프라인', '발행 버전', '최근 실행', '실행 시각', '자동 빌드']) heading.append(el('th', title)); head.append(heading);
  const body = el('tbody'); table.append(head, body); scroll.append(table); panel.append(scroll);
  const empty = el('p', undefined, 'empty'); empty.setAttribute('role', 'status'); panel.append(empty);
  const render = () => {
    body.replaceChildren(); const filtered = pipelines.filter(item => matchesPipeline(item, search.value, filter.value));
    for (const item of filtered) {
      const row = el('tr'); const name = el('td'); name.append(link(item.name, { page: 'pipeline', project: projectId, pipeline: item.id }));
      if (item.current_version) {const connections = el('div', undefined, 'caption'); connections.append(link('연결 보기 →', {page:'pipeline',project:projectId,pipeline:item.id,tab:'connections'})); name.append(connections);}
      const latest = el('td');
      if (item.latest_run_id) latest.append(link(runNames[item.latest_run_status ?? ''] ?? item.latest_run_status ?? '확인 필요', { page: 'run', project: projectId, pipeline: item.id, run: item.latest_run_id }, `badge ${item.latest_run_status}`));
      else latest.textContent = '실행 없음';
      row.append(name, el('td', item.current_version ? `v${item.current_version}` : '미발행'), latest, el('td', item.latest_run_at ? date(item.latest_run_at) : '—'), el('td', item.trigger_enabled ? '켜짐' : '꺼짐')); body.append(row);
    }
    empty.hidden = filtered.length > 0;
    empty.textContent = pipelines.length ? '조건에 맞는 파이프라인이 없습니다.' : '파이프라인이 없습니다. 첫 파이프라인을 추가해 주세요.';
  };
  const update = () => {
    currentRoute = { ...currentRoute, q: search.value || undefined, state: filter.value || undefined };
    history.replaceState(null, '', routeHash(currentRoute)); render();
  };
  search.addEventListener('input', update); filter.addEventListener('change', update); render(); content.append(panel);
  if (writable()) {
    const create = form('파이프라인 추가', async data => {
      const result = await api<Pipeline>(`/api/projects/${projectId}/pipelines`, 'POST', { name: data.get('name') });
      await selectPipeline(result.id, 'settings');
    });
    create.details.className = 'card'; field(create.node, '이름', 'name'); submit(create.node, '파이프라인 생성'); content.append(create.details);
  }
}
async function selectPipeline(id: string, tab?: string) {
  pendingRun = undefined;
  if (selected?.pipeline.id === id && !tab) { await dashboard(); return; }
  await navigate({ page: 'pipeline', pipeline: id, tab: tab ?? 'runs' });
}
async function ciSettingsView(content: HTMLElement) {
  const projectId = project!.id; const route = {...currentRoute};
  sectionHeading(content, 'CI 설정', '저장된 초안과 발행 연결을 확인하고 파이프라인을 편집합니다.');
  content.append(link('프로젝트 전체 파이프라인 보기 →', {page:'pipeline-graph',project:projectId,pipeline:route.pipeline}, 'small graph-entry'));
  const [pipelines, sourceRoots] = await Promise.all([
    api<Pipeline[]>(`/api/projects/${projectId}/pipelines`), api<SourceRoot[]>(`/api/projects/${projectId}/roots`)
  ]);
  if (!content.isConnected) return;
  const toolbar = el('div', undefined, 'row ci-toolbar');
  const layout = el('div', undefined, 'ci-layout');
  const listPanel = el('aside', undefined, 'card ci-list'); listPanel.setAttribute('aria-label', 'CI 파이프라인 목록');
  const body = el('div', undefined, 'ci-body');
  const searchLabel = el('label', '파이프라인 검색'); const search = el('input'); search.type = 'search'; search.value = route.q ?? ''; search.placeholder = '파이프라인 이름'; searchLabel.append(search);
  const count = el('p', '', 'caption'); const list = el('div', undefined, 'list'); listPanel.append(el('h2', '파이프라인'), searchLabel, count, list);
  let listHidden = false; let focused = false;
  const toggleList = button('목록 접기', async () => {listHidden = !listHidden; listPanel.hidden = listHidden; layout.classList.toggle('list-hidden',listHidden); toggleList.textContent = listHidden ? '목록 펼치기' : '목록 접기'; toggleList.setAttribute('aria-expanded',String(!listHidden));}, 'small');
  toggleList.setAttribute('aria-expanded','true');
  const focus = button('집중 모드', async () => {focused = !focused; content.closest('.app-shell')?.classList.toggle('ci-focused',focused); focus.textContent = focused ? '집중 모드 나가기' : '집중 모드'; focus.setAttribute('aria-pressed',String(focused));}, 'small'); focus.setAttribute('aria-pressed','false');
  toolbar.append(toggleList, focus); layout.append(listPanel, body); content.append(toolbar, layout);
  const chosen = route.pipeline ?? pipelines[0]?.id;
  let views: HTMLElement | undefined;
  const remember = (patch: Partial<Route>) => {
    if (!content.isConnected) return;
    const viewChanged = patch.view !== undefined && patch.view !== currentRoute.view;
    currentRoute = {...currentRoute, ...patch};
    history.replaceState(null, '', routeHash(currentRoute)); editorHash = location.hash;
    for (const item of Array.from(views?.querySelectorAll<HTMLAnchorElement>('a[data-view]') ?? [])) item.href = routeHash({...currentRoute,view:item.dataset.view,version:undefined});
    if (viewChanged) drawList();
  };
  const drawList = () => {
    list.replaceChildren(); const visible = pipelines.filter(pipeline => matchesPipeline(pipeline, search.value, ''));
    count.textContent = `${pipelines.length}개 중 ${visible.length}개 표시`;
    const row = (pipeline: Pipeline) => {
      const item = link('', {page:'ci-settings',project:projectId,pipeline:pipeline.id,q:search.value || undefined,view:currentRoute.view ?? 'visual'}, 'ci-pipeline-link');
      item.append(el('strong',pipeline.name), el('span',pipeline.current_version ? `발행 v${pipeline.current_version}` : '미발행', 'caption'));
      if (pipeline.id === chosen) item.setAttribute('aria-current','page'); list.append(item);
    };
    for (const pipeline of visible) row(pipeline);
    if (!visible.length) list.append(el('p','검색 결과가 없습니다.', 'caption'));
    const outside = pipelines.find(p => p.id === chosen && !visible.includes(p));
    if (outside) {list.append(el('p','현재 선택 · 검색 결과 밖', 'caption')); row(outside);}
  };
  search.addEventListener('input', () => {remember({q:search.value || undefined}); drawList();}); drawList();
  if (!chosen) {body.append(el('p','등록된 파이프라인이 없습니다. 파이프라인 메뉴에서 새 파이프라인을 만들 수 있습니다.', 'card')); return;}
  if (!pipelines.some(p => p.id === chosen)) {body.append(el('p','선택한 파이프라인이 이 프로젝트에 없습니다.', 'card graph-error')); return;}
  const detail = selected?.pipeline.id === chosen ? selected : await api<Detail>(`/api/pipelines/${chosen}`);
  if (!content.isConnected) return;
  if (detail.pipeline.project_id !== projectId) throw new Error('파이프라인과 프로젝트가 일치하지 않습니다.');
  selected = detail; roots = sourceRoots;
  remember({pipeline:chosen,view:route.view ?? 'visual'});
  body.append(el('h2', detail.pipeline.name));
  views = el('nav', undefined, 'detail-tabs'); views.setAttribute('aria-label','CI 설정 보기');
  for (const [view,label] of [['visual','작업·초안'],['connections','발행 연결']] as const) {
    const item = link(label, {...currentRoute,view,version:undefined});
    item.dataset.view = view;
    if (view === 'connections' ? currentRoute.view === 'connections' : currentRoute.view !== 'connections') item.setAttribute('aria-current','page');
    views.append(item);
  }
  body.append(views);
  const shortcuts = el('div', undefined, 'row ci-shortcuts');
  for (const [tab,label] of [['settings','기존 설정'],['review','변경 비교'],['trigger','자동 빌드'],['versions','발행 이력']] as const) shortcuts.append(link(label,{page:'pipeline',project:projectId,pipeline:chosen,tab},'small'));
  body.append(shortcuts);
  if (currentRoute.view === 'connections') {
    const revisions = await api<Revision[]>(`/api/pipelines/${chosen}/revisions`); if (!content.isConnected) return;
    const connectionRoute = (next: Route): Route => ({page:'ci-settings',project:projectId,pipeline:next.pipeline,view:'connections',version:next.version,q:search.value || undefined,job:next.pipeline === chosen ? currentRoute.job : undefined});
    disposeConnectionsView = createConnectionsView(body, {
      route:{page:'pipeline',project:projectId,pipeline:chosen,tab:'connections',version:currentRoute.version},revisions,api,
      link:(label,next) => link(label,next.page === 'pipeline' && next.tab === 'connections' ? connectionRoute(next) : next),
      navigate: next => navigate(connectionRoute(next))
    });
    return;
  }
  if (detail.configuration_draft) {
    const imported = detail.configuration_draft;
    const notice = el('section',undefined,'card');
    notice.append(el('h3','가져온 프로젝트 구성 초안'),el('p',`${imported.source_name} · 구성 g${imported.generation} · ${imported.definition.category}`, 'caption'),el('p','이 구성은 실행 정의로 전환하기 전 상태입니다. 기존 설정에서 준비 항목과 TOML을 확인하세요.'));
    for (const requirement of imported.readiness) notice.append(el('p',requirement,'caption'));
    notice.append(link('구성 설정 열기',{page:'pipeline',project:projectId,pipeline:chosen,tab:'settings'},'button')); body.append(notice); return;
  }
  editorHash = location.hash;
  pipelineEditor = createPipelineEditor(body, {
    id:chosen,name:detail.pipeline.name,version:detail.pipeline.current_version,draft:detail.draft,
    toml:detail.draft_toml ?? '',savedToml:detail.draft_toml,definition:detail.draft.definition,template:example(),
    writable:writable(),roots:sourceRoots,api,message,inspect:true,initialView:currentRoute.view,initialJob:currentRoute.job,
    onView:(view,job) => remember({view,job}),
    review:() => navigate({page:'pipeline',project:projectId,pipeline:chosen,tab:'review',to:'draft',from:String(detail.pipeline.current_version)})
  });
}
async function openRun(pipeline: string, run: string, tab = 'graph') {
  await navigate({ page: 'run', pipeline, run, tab });
}
function example() {
  return `schema_version = 1\nsource_root_id = "${roots[0]?.id ?? '11111111-1111-1111-1111-111111111111'}"\nmax_parallel_jobs = 2\n\n[[jobs]]\nname = "build"\nstage = "build"\nneeds = []\nshell = "sh"\nscript = ["cat hello.txt"]\n\n[jobs.runner]\nos = "linux"\narch = "aarch64"\npool = "default"\n\n[[jobs]]\nname = "test"\nstage = "test"\nneeds = ["build"]\nshell = "sh"\nscript = ["test -f hello.txt"]\n\n[jobs.runner]\nos = "linux"\narch = "aarch64"\npool = "default"\n`;
}
async function editor(content: HTMLElement) {
  const detail = selected!; const id = detail.pipeline.id; const path = `/api/pipelines/${id}`;
  const tab = currentRoute.tab; const draftInput = { generation: detail.draft.generation, base_version: detail.draft.base_version };
  sectionHeading(content, detail.pipeline.name, detail.pipeline.current_version ? `발행 v${detail.pipeline.current_version}` : '아직 발행되지 않은 파이프라인입니다.');
  const revisions = await api<Revision[]>(`${path}/revisions`);
  if (!content.isConnected) return;
  if (runnable() && detail.pipeline.current_version > 0) {
    createRunPreflight(content, {route:currentRoute,revisions,api,link,navigate,message}, detail.pipeline.current_version, async input => {
      if (!pendingRun || pendingRun.changelist !== input.changelist || pendingRun.version !== input.version || pendingRun.source_run_id !== input.source_run_id) pendingRun = {request_id:crypto.randomUUID(),...input};
      const result = await api<Run>(`${path}/runs`, 'POST', pendingRun); pendingRun = undefined;
      if (content.isConnected) await openRun(id, result.id); message('실행을 등록했습니다.');
    });
  }

  tabs(content, [['runs', '실행 이력'], ['connections', '연결 관계'], ['settings', '설정'], ['review', '변경 비교'], ['trigger', '자동 빌드'], ['versions', '발행 이력']], currentRoute, '파이프라인 상세');
  if (tab === 'connections') {disposeConnectionsView = createConnectionsView(content, {route:currentRoute,revisions,api,link,navigate}); return;}
  if (tab === 'review') {disposeReviewView = createReviewView(content, {route:currentRoute,revisions,api,link,navigate,message}); return;}
  if (tab === 'settings') {
    if (detail.configuration_draft) { configurationEditor(content, id, detail.configuration_draft); return; }
    editorHash = location.hash;
    pipelineEditor = createPipelineEditor(content, {
      id, name: detail.pipeline.name, version: detail.pipeline.current_version, draft: detail.draft,
      toml: detail.draft_toml ?? example(), savedToml: detail.draft_toml, definition: detail.draft.definition, writable: writable(), roots, api, message,
      review: () => navigate({page:'pipeline',project:project?.id,pipeline:id,tab:'review',to:'draft',from:String(detail.pipeline.current_version)})
    });

    return;
  }
  if (tab === 'versions') {
    const history = el('div', undefined, 'card'); history.append(el('h2', '발행 이력'));
    for (const revision of revisions) {
      const row = el('div', undefined, 'row space'); row.append(el('span', `v${revision.version} · ${date(revision.created_at)} · ${revision.definition_hash.slice(0, 12)}`, 'caption'));
      row.append(link('연결 보기', {page:'pipeline',project:project?.id,pipeline:id,tab:'connections',version:String(revision.version)},'small'),link('이전 버전과 비교',{page:'pipeline',project:project?.id,pipeline:id,tab:'review',from:String(revision.version-1),to:String(revision.version)},'small'));
      if (writable()) row.append(button('초안으로 복원', async () => { await api(`${path}/restore`, 'POST', { version: revision.version, ...draftInput }); await selectPipeline(id, 'settings'); message(`v${revision.version}을 초안으로 복원했습니다. 다시 발행하면 새 버전이 생성됩니다.`); }, 'small'));
      history.append(row);
    }
    if (!revisions.length) history.append(el('p', '저장한 초안을 발행하면 이력이 생성됩니다.', 'muted')); content.append(history);

    return;
  }
  if (tab === 'trigger') { await triggerView(content, id, revisions); return; }
  const runs = await api<Run[]>(`${path}/runs`);
  if (!content.isConnected) return;
  const runCard = el('section', undefined, 'card'); runCard.append(el('h2', '실행 이력'), el('p', '최근 100건의 실행을 표시합니다.', 'caption'));
  const scroll = el('div', undefined, 'scroll'); const table = el('table'); const header = el('tr'); for (const title of ['실행', '소스 / 설정', '상태', '등록 시각', '']) header.append(el('th', title)); const thead = el('thead'); thead.append(header); table.append(thead); const tbody = el('tbody');
  for (const run of runs) {
    const row = el('tr'); const runName = el('td'); runName.append(link(run.id.slice(0, 8), { page: 'run', project: project?.id, pipeline: id, run: run.id })); row.append(runName, el('td', `CL ${run.source_snapshot.changelist} / v${run.version}`));
    const status = el('td'); status.append(el('span', run.cancel_requested && run.status === 'running' ? '취소 처리 중' : run.status === 'queued' && run.definition?.deployment ? '승인·배정 대기' : runNames[run.status] ?? run.status, `badge ${run.status}`)); row.append(status, el('td', date(run.created_at))); const action = el('td');
    if (runnable() && ['queued', 'running'].includes(run.status) && !run.cancel_requested) action.append(button('취소', async () => { await api(`/api/runs/${run.id}/cancel`, 'POST'); await selectPipeline(id); message('취소를 요청했습니다. 실행 중 작업은 Agent의 프로세스 정리가 끝난 뒤 종료됩니다.'); }, 'small danger')); row.append(action); tbody.append(row);
  }
  table.append(tbody); scroll.append(table); runCard.append(scroll); if (!runs.length) runCard.append(el('p', '등록된 실행이 없습니다.', 'empty')); runCard.append(button('새로고침', async () => { await selectPipeline(id); }, 'small')); content.append(runCard);

}
async function runPage(content: HTMLElement) {
  const run = currentRun!; const tab = currentRoute.tab;
  sectionHeading(content, `실행 ${run.id.slice(0, 8)}`, `${selected!.pipeline.name} · CL ${run.source_snapshot.changelist} · v${run.version}`);
  const bar = el('div', undefined, 'row run-actions');
  const stateBadge = el('span');
  const cancel = runnable() ? button('실행 취소', async () => { await api(`/api/runs/${run.id}/cancel`, 'POST'); await dashboard(); }, 'small danger') : undefined;
  const updateState = (status: string, cancelRequested: boolean) => {
    const ongoing = ['queued', 'running'].includes(status);
    stateBadge.textContent = ongoing && cancelRequested ? '취소 요청됨' : runNames[status] ?? status; stateBadge.className = `badge ${status}`;
    if (cancel) cancel.hidden = !ongoing || cancelRequested;
  };
  bar.append(stateBadge, button('상태 새로고침', dashboard, 'small')); if (cancel) bar.append(cancel); updateState(run.status, run.cancel_requested);
  content.append(bar);
  const items: [string, string][] = [['graph', '실행 그래프·로그'], ['chain', '실행 체인'], ['dispatch', '배정 점검'], ['artifacts', '산출물'], ['info', '실행 정보']];
  if (run.definition?.deployment) items.push(['deployment', '배포 승인·결과']);
  tabs(content, items, currentRoute, '실행 상세');
  if (tab === 'chain') { disposeRunChainView = createRunChainView(content, {runId:run.id, api, link, onState:updateState}); return; }
  if (tab === 'dispatch') { disposeDispatchView = createDispatchView(content, {runId: run.id, route: currentRoute, api, link, onState: updateState}); return; }
  if (tab === 'deployment') {
    if (run.definition?.deployment) await deploymentView(content, run.id);
    else content.append(el('p', '배포 실행이 아닙니다.', 'card'));
    return;
  }
  await runView(content, run.id, tab ?? 'graph');
}

type ChangeTrigger = { version: number; generation: number; enabled: boolean; includes: string[]; excludes: string[]; cursor: number; server_id: string; checked_at: string | null; last_error: string | null };
type TriggerResult = { trigger: ChangeTrigger | null; events: { changelist: number; version: number; outcome: string; run_id: string | null; status: string | null; created_at: string }[] };
async function triggerView(content: HTMLElement, id: string, revisions: Revision[]) {
  const result = await api<TriggerResult>(`/api/pipelines/${id}/trigger`);
  if (!content.isConnected) return;
  const current = result.trigger;
  const panel = el('section', undefined, 'card'); panel.id = 'change-trigger'; const heading = el('div', undefined, 'row space');
  const stale = current?.enabled && (!current.checked_at || Date.now() - Date.parse(current.checked_at) > 120000);
  heading.append(el('h2', 'Perforce 자동 빌드'), el('span', !current?.enabled ? '꺼짐' : current.last_error ? '감지 오류' : stale ? '감지 확인 대기' : '감지 중', `badge ${current?.last_error ? 'failed' : current?.enabled ? 'running' : ''}`)); panel.append(heading);
  panel.append(el('p', '새로 제출된 변경을 15초 간격으로 확인합니다. 포함 경로의 파일이 바뀌면 지정 버전으로 실행하며, 제외 경로는 우선 적용됩니다. 경로는 대소문자를 구분합니다.', 'caption'));
  if (current) {
    panel.append(el('p', `v${current.version} 고정 · 처리 위치 CL ${current.cursor} · ${current.server_id} · 마지막 확인 ${current.checked_at ? date(current.checked_at) : '대기'}`));
    panel.append(el('pre', [...current.includes, ...current.excludes.map(path => `-${path}`)].join('\n')));
    if (current.last_error) panel.append(el('p', current.last_error, 'error'));
  } else panel.append(el('p', '자동 빌드가 설정되지 않았습니다.', 'muted'));
  panel.append(el('p', '최초 저장은 현재 제출 번호부터 시작합니다. 껐다 다시 켜면 중단 중의 제출도 이어서 처리합니다. 서버 재시작에도 처리 위치가 유지됩니다.', 'caption'));
  if ((user.is_admin || project?.role === 'admin') && revisions.length) {
    const config = form('자동 빌드 설정', async data => {
      const lines = (key: string) => String(data.get(key) ?? '').split(/\r?\n/).map(line => line.trim()).filter(Boolean);
      await api(`/api/pipelines/${id}/trigger`, 'PUT', { generation: current?.generation ?? 0, version: Number(data.get('version')), enabled: data.get('enabled') === 'on', includes: lines('includes'), excludes: lines('excludes'), reset_to_head: data.get('reset') === 'on' });
      await selectPipeline(id); message('자동 빌드 설정을 저장했습니다.'); document.querySelector('#change-trigger')?.scrollIntoView({ block: 'start' });
    });
    const enabledLabel = el('label', '자동 빌드 켜기', 'retention-toggle'); const enabled = el('input'); enabled.type = 'checkbox'; enabled.name = 'enabled'; enabled.checked = current?.enabled ?? false; enabledLabel.append(enabled); config.node.append(enabledLabel);
    const versionLabel = el('label', '자동 빌드 발행 버전'); const version = el('select'); version.name = 'version';
    for (const revision of revisions) option(version, String(revision.version), `v${revision.version}`);
    version.value = String(current?.version ?? selected!.pipeline.current_version); versionLabel.append(version); config.node.append(versionLabel);
    for (const [name, text, value] of [['includes', '포함 경로 (한 줄에 하나)', current?.includes ?? []], ['excludes', '제외 경로 (한 줄에 하나)', current?.excludes ?? []]] as const) {
      const label = el('label', text); const input = el('textarea'); input.name = name; input.rows = 3; input.style.minHeight = '90px'; input.value = value.join('\n'); input.placeholder = '//streams/root/Server/...'; input.required = name === 'includes'; label.append(input); config.node.append(label);
    }
    const resetLabel = el('label', '현재 제출 번호로 새로 시작 (미처리 제출 건너뛰기)', 'retention-toggle'); const reset = el('input'); reset.type = 'checkbox'; reset.name = 'reset'; resetLabel.append(reset); config.node.append(resetLabel);
    config.node.append(el('p', '새 버전 발행만으로 자동 빌드 버전은 바뀌지 않습니다. 여기서 버전을 선택해 저장하세요.', 'caption'));
    submit(config.node, '자동 빌드 저장'); panel.append(config.details);
    if (current?.enabled) panel.append(button('자동 빌드 끄기', async () => {
      await api(`/api/pipelines/${id}/trigger`, 'PUT', { generation: current.generation, version: current.version, enabled: false, includes: current.includes, excludes: current.excludes });
      await selectPipeline(id); message('자동 감지를 중지했습니다. 이미 등록된 빌드는 계속 실행됩니다.'); document.querySelector('#change-trigger')?.scrollIntoView({ block: 'start' });
    }, 'small'));
  }
  panel.append(button('감지 상태 새로고침', async () => { await selectPipeline(id); document.querySelector('#change-trigger')?.scrollIntoView({ block: 'start' }); }, 'small'));
  if (result.events.length) {
    const table = el('table'); const header = el('tr'); for (const title of ['제출 / 설정', '처리', '시각']) header.append(el('th', title)); const head = el('thead'); head.append(header); table.append(head); const body = el('tbody');
    for (const event of result.events) {
      const row = el('tr'); row.append(el('td', `CL ${event.changelist} / v${event.version}`)); const outcome = el('td');
      if (event.run_id) outcome.append(button(`${event.run_id.slice(0, 8)} · ${event.status}`, async () => { await openRun(id, event.run_id!); }, 'small'));
      else outcome.textContent = '경로 필터로 제외'; row.append(outcome, el('td', date(event.created_at))); body.append(row);
    }
    table.append(body); const scroll = el('div', undefined, 'scroll'); scroll.append(table); panel.append(scroll);
  }
  content.append(panel);
}

function downloadToml(name: string, source: string) {
  const url = URL.createObjectURL(new Blob([source], { type: 'text/plain;charset=utf-8' }));
  const link = el('a'); link.href = url; link.download = name; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function configurationEditor(content: HTMLElement, pipelineId: string, draft: ConfigurationDraft) {
  const config = draft.definition;
  const panel = el('section', undefined, 'card'); const title = el('div', undefined, 'row space');
  title.append(el('h2', config.name), el('span', `DB 초안 g${draft.generation} · 실행 준비 필요`, 'badge queued')); panel.append(title);
  panel.append(el('p', `${config.category} · ${config.runner_os}/${config.runner_arch} · 작업 위치 ${config.working_directory} · 동시 작업 ${config.max_parallel_jobs}`, 'caption'));
  panel.append(el('p', `단계: ${config.stages.join(' → ')} · 선행 파이프라인: ${config.needs.join(', ') || '없음'}`));
  if (config.source_root_id) { const root = roots.find(root => root.id === config.source_root_id); panel.append(el('p', `Perforce 소스: ${root?.name ?? config.source_root_id} · ${root?.definition.stream ?? (root?.definition.kind === 'view' ? 'Perforce View' : 'classic')}`, 'caption')); }
  panel.append(el('p', '파이프라인 공용 workspace · 단계별 순차 실행', 'caption'));
  if (config.checkout_view) panel.append(el('p', `Perforce checkout view: ${config.checkout_view}`, 'caption'));
  const filters = el('details'); filters.append(el('summary', `변경 경로 필터 (${config.changes.length})`), el('pre', config.changes.join('\n'))); panel.append(filters);
  for (const stage of config.stages) {
    panel.append(el('h3', stage));
    for (const job of config.jobs.filter(job => job.stage === stage)) {
      const commands = el('details'); commands.append(el('summary', `${job.name} · ${job.timeout_seconds}초`), el('pre', job.script.join('\n'))); panel.append(commands);
    }
  }
  panel.append(el('h3', '실행 준비')); const readiness = el('ul');
  for (const requirement of draft.readiness) readiness.append(el('li', requirement)); panel.append(readiness);
  panel.append(el('p', '설정은 DB에 저장됐습니다. 필요한 실행 기능과 소스를 연결하기 전까지 발행·실행되지 않습니다.', 'caption'));
  const editor = el('details'); editor.append(el('summary', 'TOML 편집'));
  const input = el('textarea'); input.setAttribute('aria-label', '프로젝트 파이프라인 TOML'); input.spellcheck = false; input.value = draft.toml; input.readOnly = !writable(); editor.append(input);
  const actions = el('div', undefined, 'row');
  if (writable()) actions.append(button('설정 초안 저장', async () => {
    await api(`/api/pipelines/${pipelineId}/configuration`, 'PUT', { generation: draft.generation, toml: input.value });
    await selectPipeline(pipelineId); message('설정 초안을 DB에 저장했습니다.');
  }, 'primary'));
  actions.append(button('설정 TOML 내려받기', async () => { downloadToml(`${config.name}.toml`, input.value); })); editor.append(actions); panel.append(editor);
  panel.append(el('p', `가져온 파일: ${draft.source_name} · SHA-256 ${draft.source_hash.slice(0, 12)}`, 'caption')); content.append(panel);
}

async function agentsView(content: HTMLElement) {
  const route = currentRoute;
  const agentFilters: AgentFilters = {search: route.q ?? '', state: route.state ?? (route.agent ? 'all' : 'active'), os: route.os ?? '', arch: route.arch ?? '', engine: route.engine ?? '', availability: route.availability ?? ''};
  let selectedAgent = route.agent;
  const [allAgents, releases, updates, credentials] = await Promise.all([api<BuildAgent[]>('/api/agents'), api<AgentRelease[]>('/api/agent-releases'), api<AgentUpdate[]>('/api/agent-updates'), api<PerforceHealth[]>('/api/perforce-credentials')]);
  if (!content.isConnected) return;
  let liveAgents = allAgents;
  content.append(el('h1', '에이전트'), el('p', '모든 프로젝트가 공유하는 실행 자원입니다. 운영체제·아키텍처와 Docker 엔진·가용성으로 찾습니다.', 'muted'));
  const summary = el('div', undefined, 'agent-summary'); summary.setAttribute('aria-label', '에이전트 요약'); content.append(summary);
  const filterRoute = (agent = selectedAgent): Route => ({page: 'agents', agent, q: agentFilters.search || undefined, state: agentFilters.state, os: agentFilters.os || undefined, arch: agentFilters.arch || undefined, engine: agentFilters.engine || undefined, availability: agentFilters.availability || undefined});
  const summaryCards = new Map<string, {link: HTMLAnchorElement; count: HTMLElement}>();
  for (const [state, label] of [['active', '활성'], ['online', '온라인'], ['offline', '오프라인'], ['revoked', '폐기 이력']]) {
    const card = link('', {page: 'agents', state}, 'agent-stat'); const count = el('strong', '0'); card.append(el('span', label, 'caption'), count); summary.append(card); summaryCards.set(state, {link: card, count});
  }
  content.append(el('p', '기본 목록에는 활성 에이전트를 표시합니다. 요약 건수는 검색·환경 조건 기준이며, 폐기 이력에서 이전 등록 정보를 확인할 수 있습니다.', 'caption'));
  const panel = el('section', undefined, 'card'); content.append(panel);
  const refreshAgents = async () => { await dashboard(); };
  const toolbar = el('div', undefined, 'agent-toolbar');
  const searchLabel = el('label', '에이전트 검색'); const search = el('input'); search.type = 'search'; search.value = agentFilters.search; search.placeholder = '이름 · OS · Pool'; searchLabel.append(search); toolbar.append(searchLabel);
  const stateLabel = el('label', '등록·연결 상태'); const stateSelect = el('select');
  for (const [value, title] of [['active', '활성 에이전트'], ['online', '온라인'], ['offline', '오프라인'], ['draining', '새 작업 중지'], ['revoked', '폐기 이력'], ['all', '전체 · 폐기 포함']]) option(stateSelect, value, title);
  stateSelect.value = agentFilters.state!; stateLabel.append(stateSelect); toolbar.append(stateLabel);
  const filterSelects: [keyof AgentFilters, HTMLSelectElement][] = [];
  for (const [key, title, choices] of [
    ['os', '운영체제', [['linux', 'Linux'], ['macos', 'macOS'], ['windows', 'Windows']]],
    ['arch', '아키텍처', [['x86_64', 'x86_64'], ['aarch64', 'aarch64']]],
    ['engine', 'Docker 엔진', [['linux', 'Linux 엔진'], ['windows', 'Windows 엔진']]],
    ['availability', 'Docker 가용성', [['available', '사용 가능'], ['unavailable', '사용 불가·확인 실패'], ['unknown', '확인 전'], ['stale', '확인 지연']]],
  ] as [keyof AgentFilters, string, [string, string][]][]) {
    const label = el('label', title); const select = el('select'); option(select, '', '전체');
    for (const [value, text] of choices) option(select, value, text);
    select.value = agentFilters[key] ?? ''; label.append(select); toolbar.append(label); filterSelects.push([key, select]);
  }
  toolbar.append(button('상태 새로고침', refreshAgents, 'small'), link('필터 초기화', {page: 'agents'}, 'button small')); panel.append(toolbar);
  const selectionNotice = el('div', undefined, 'agent-selection-notice'); panel.append(selectionNotice);
  const tableWrap = el('div', undefined, 'scroll'); const table = el('table', undefined, 'agent-table'); table.setAttribute('aria-label', '등록된 에이전트');
  const header = el('tr'); for (const name of ['상태', '에이전트', '운영체제', 'Docker', '버전', '마지막 연결', '관리']) { const cell = el('th', name); cell.scope = 'col'; header.append(cell); }
  const head = el('thead'); head.append(header); const body = el('tbody'); table.append(head, body); tableWrap.append(table); panel.append(tableWrap);
  const empty = el('p', undefined, 'empty'); panel.append(empty);
  const checked = el('p', undefined, 'caption'); checked.setAttribute('role', 'status'); panel.append(checked);
  panel.append(el('p', 'Docker는 실제 실행 계정에서 대기 중 60초 간격으로 점검합니다. 실행 중에는 마지막 결과가 표시됩니다. Docker가 필요한 작업은 지정한 엔진이 최근 150초 안에 사용 가능으로 확인된 경우 배정합니다.', 'caption'));
  const resultCount = el('p', undefined, 'caption'); panel.insertBefore(resultCount, tableWrap);
  const cells = new Map<string, { row: HTMLTableRowElement; detail: HTMLTableRowElement; toggle: HTMLButtonElement; status: HTMLElement; docker: HTMLElement; version: HTMLElement; seen: HTMLElement }>();
  const repaint = () => {
    const now = Date.now(); let visible = 0;
    const scoped = liveAgents.filter(agent => matchesAgent(agent, agentFilters, now));
    for (const [state, card] of summaryCards) {
      card.count.textContent = String(liveAgents.filter(agent => matchesAgent(agent, {...agentFilters, state}, now)).length);
      card.link.href = routeHash({...filterRoute(), agent: undefined, state});
      if (agentFilters.state === state) card.link.setAttribute('aria-current', 'true'); else card.link.removeAttribute('aria-current');
    }
    selectionNotice.replaceChildren();
    if (selectedAgent && !scoped.some(agent => agent.id === selectedAgent)) {
      const exists = liveAgents.some(agent => agent.id === selectedAgent);
      selectionNotice.append(el('p', exists ? '선택한 에이전트가 현재 필터에서 제외되어 있습니다.' : '선택한 에이전트를 찾을 수 없습니다. 목록에서 다른 에이전트를 선택하세요.'));
      if (exists) selectionNotice.append(link('선택한 에이전트 보기', {page: 'agents', agent: selectedAgent, state: 'all'}, 'button small'));
      selectionNotice.append(link('선택 해제', {...filterRoute(), agent: undefined}, 'button small'));
    }
    selectionNotice.hidden = !selectionNotice.childElementCount;
    for (const agent of liveAgents) {
      const view = cells.get(agent.id); if (!view) continue;
      const show = scoped.some(item => item.id === agent.id); const expanded = show && agent.id === selectedAgent; view.row.hidden = !show; view.detail.hidden = !expanded; view.toggle.setAttribute('aria-expanded', String(expanded)); if (show) visible++;
      const state = connectionStatus(agent, now); view.status.replaceChildren(el('span', state.label, `badge ${state.online ? 'succeeded' : 'canceled'}`));
      if (agent.active && agent.draining) view.status.append(el('div', '새 작업 중지', 'caption'));
      const docker = dockerStatus(agent, now); view.docker.replaceChildren(el('span', docker.label, `badge ${docker.tone}`), el('div', docker.detail, 'caption'));
      view.docker.title = agent.docker_checked_at ? `마지막 확인: ${date(agent.docker_checked_at)}` : 'Docker 상태 보고를 기다리고 있습니다.';
      view.version.textContent = agent.agent_version ?? '확인 전'; view.seen.textContent = agent.last_seen_at ? date(agent.last_seen_at) : '연결 전';
    }
    resultCount.textContent = `${visible}대 표시 · 전체 등록 ${liveAgents.length}대`;
    empty.hidden = visible > 0; empty.textContent = liveAgents.length ? '조건에 맞는 에이전트가 없습니다.' : '등록된 에이전트가 없습니다. 아래에서 첫 에이전트를 등록하세요.';
  };
  const saveFilters = (replace = false) => {
    const next = filterRoute(); const hash = routeHash(next);
    if (location.hash !== hash) { if (replace) history.replaceState(null, '', hash); else history.pushState(null, '', hash); }
    currentRoute = next; repaint();
  };
  search.addEventListener('input', () => { agentFilters.search = search.value; selectedAgent = undefined; saveFilters(true); });
  stateSelect.addEventListener('change', () => { agentFilters.state = stateSelect.value; selectedAgent = undefined; saveFilters(); });
  for (const [key, select] of filterSelects) select.addEventListener('change', () => { agentFilters[key] = select.value; selectedAgent = undefined; saveFilters(); });
  const statuses: Record<string, string> = { requested: '빌드 종료 대기', staged: '패치 적용·연결 확인 중', succeeded: '패치 완료', rolled_back: '이전 버전으로 복구됨', failed: '패치 실패', cancelled: '요청 취소됨' };
  for (const agent of allAgents) {
    const online = connectionStatus(agent).online;
    const tr = el('tr'); const statusCell = el('td'); const name = el('td'); name.append(el('strong', agent.name), el('div', `Pool · ${agent.pool}`, 'caption'));
    const os = el('td'); os.append(el('span', ({macos: 'macOS', windows: 'Windows', linux: 'Linux'} as Record<string, string>)[agent.os] ?? agent.os), el('div', agent.arch, 'caption'));
    const docker = el('td'); const version = el('td'); const seen = el('td', undefined, 'caption'); const manage = el('td');
    const detail = el('tr'); detail.hidden = true; const detailCell = el('td'); detailCell.colSpan = 7; const row = el('div', undefined, 'stack agent-detail'); row.append(el('h3', `${agent.name} 관리`), el('span', `ID ${agent.id} · 1 slot`, 'caption')); detailCell.append(row); detail.append(detailCell);
    const toggle = button('관리', async () => { selectedAgent = selectedAgent === agent.id ? undefined : agent.id; saveFilters(); if (!detail.hidden) detail.scrollIntoView({block: 'nearest'}); }, 'small'); toggle.setAttribute('aria-label', `${agent.name} 관리`); toggle.setAttribute('aria-expanded', 'false'); detail.id = `agent-detail-${agent.id}`; toggle.setAttribute('aria-controls', detail.id); manage.append(toggle);
    tr.append(statusCell, name, os, docker, version, seen, manage); body.append(tr, detail); cells.set(agent.id, {row: tr, detail, toggle, status: statusCell, docker, version, seen});
    const history = updates.filter(update => update.agent_id === agent.id);
    const pending = history.find(update => ['requested', 'staged'].includes(update.status));
    row.append(el('span', `설치 버전 ${agent.agent_version ?? '확인 전'}${agent.binary_sha256 ? ` · ${agent.binary_sha256.slice(0, 12)}` : ''}`, 'caption'));
    const tickets = credentials.filter(item => item.agent_id === agent.id);
    if (!agent.active) row.append(el('p', '폐기된 에이전트입니다. 서버 접속과 새 작업 배정이 차단되었으며 기존 실행·설치·패치 이력은 보존합니다. 호스트에 설치된 서비스의 종료·삭제는 별도로 수행해야 합니다.', 'caption'));
    else if (agent.perforce_managed) {
      if (!tickets.length) row.append(el('p', 'Perforce 자동 갱신 설정 없음 · 새 작업 배정 중지', 'caption'));
      for (const ticket of tickets) {
        const recent = (value: string | null) => value !== null && Date.now() - Date.parse(value) < 150000;
        const live = (value: string | null) => value !== null && Date.parse(value) - Date.now() > 300000;
        const ready = ticket.server_state === 'ready' && ticket.agent_state === 'ready' && recent(ticket.server_checked_at) && recent(ticket.agent_checked_at) && live(ticket.server_expires_at) && live(ticket.agent_expires_at);
        const reason = ticket.server_state === 'error' ? '서버 갱신 실패' : ticket.agent_state === 'error' ? 'Agent 인증 확인 실패' : !recent(ticket.server_checked_at) || !recent(ticket.agent_checked_at) ? '상태 확인 대기 또는 지연' : 'ticket 만료 임박 또는 확인 중';
        row.append(el('strong', `Perforce ${ticket.role === 'read' ? '읽기' : '게시'} · ${ready ? '정상 · 자동 갱신' : reason}`));
        row.append(el('span', `${ticket.connection.user} · 만료 ${ticket.agent_expires_at ? date(ticket.agent_expires_at) : '확인 전'} · 마지막 갱신 ${ticket.renewed_at ? date(ticket.renewed_at) : '확인 전'}`, 'caption'));
        if (!ready) row.append(el('span', ticket.role === 'read' ? '새 작업 배정 중지 · 인증 복구 후 자동 재개' : '게시 작업 배정 중지 · 인증 복구 후 자동 재개', 'caption'));
      }
    } else row.append(el('span', 'Perforce 자동 갱신 미설정 · 수동 ticket 사용', 'caption'));
    if (agent.active) {
      const actions = el('div', undefined, 'row');
      if (!pending) actions.append(button(agent.draining ? '새 작업 재개' : '새 작업 중지', async () => { await api(`/api/agents/${agent.id}`, 'PUT', { active: true, draining: !agent.draining }); await refreshAgents(); }, 'small'));
      actions.append(button('자격 증명 폐기', async () => { await api(`/api/agents/${agent.id}`, 'PUT', { active: false, draining: true }); await navigate({page: 'agents', agent: agent.id, state: 'revoked'}); message('Agent 자격 증명을 폐기했습니다. 폐기 이력에 보존되며 호스트 서비스는 별도로 중지해야 합니다.'); }, 'small danger')); row.append(actions);
      if (['macos', 'windows'].includes(agent.os)) {
        if (!agent.self_patch_ready && !pending) row.append(el('p', '셀프 패치 모듈 연결 대기 · 최초 한 번 최신 설치 패키지의 업데이트를 실행해 주세요.', 'caption'));
        const available = releases.filter(release => release.os === agent.os && release.arch === agent.arch && release.sha256 !== agent.binary_sha256);
        if (!pending && agent.self_patch_ready && online && available.length) {
          const label = el('label', '패치 버전'); const select = el('select'); select.setAttribute('aria-label', `${agent.name} 패치 버전`);
          for (const release of available) option(select, release.id, `${release.version} · ${release.sha256.slice(0, 12)} · ${(release.size / 1048576).toFixed(1)} MiB`);
          label.append(select); row.append(label);
          row.append(button('셀프 패치 적용', async () => { await api(`/api/agents/${agent.id}/update`, 'POST', { release_id: select.value }); await refreshAgents(); message('패치를 요청했습니다. 실행 중인 빌드를 마친 후 적용하며, 연결 확인에 실패하면 이전 바이너리로 복구합니다.'); }, 'small primary'));
        } else if (!pending && agent.self_patch_ready && available.length === 0) row.append(el('span', '적용할 다른 릴리스가 없습니다.', 'caption'));
      }
    }
    for (const update of history.slice(0, 3)) {
      const release = releases.find(item => item.id === update.release_id);
      row.append(el('span', `${statuses[update.status] ?? update.status} · ${release?.version ?? update.release_id.slice(0, 8)} · ${date(update.created_at)}`, 'caption'));
      if (update.reason) row.append(el('span', update.reason, 'caption'));
      if (agent.active && update.status === 'requested') row.append(button('패치 요청 취소', async () => { await api(`/api/agent-updates/${update.id}/cancel`, 'POST'); await refreshAgents(); }, 'small'));
    }
  }
  repaint();
  const selectedDetail = selectedAgent ? cells.get(selectedAgent)?.detail : undefined;
  if (selectedDetail && !selectedDetail.hidden) selectedDetail.scrollIntoView({block: 'nearest'});
  const create = form('Agent 등록', async data => {
    const result = await api<{ agent: BuildAgent; token: string }>('/api/agents', 'POST', { name: data.get('name'), pool: data.get('pool'), os: data.get('os'), arch: data.get('arch') });
    await navigate({page: 'agents', agent: result.agent.id, state: 'active'}); message('Agent를 등록했습니다. 자격 증명은 이번 응답에서만 제공됩니다. 파일을 저장하고 권한을 600으로 설정하세요.');
    const url = URL.createObjectURL(new Blob([result.token], { type: 'text/plain' })); const link = el('a', ' Agent 인증 파일 내려받기'); link.href = url; link.download = 'agent-token'; link.addEventListener('click', () => { setTimeout(() => URL.revokeObjectURL(url), 1000); }, { once: true }); notice.append(link);
  });
  field(create.node, 'Agent 이름', 'name', 'linux-agent'); field(create.node, 'Pool', 'pool', 'default');
  const osLabel = el('label', '운영체제'); const os = el('select'); os.name = 'os'; option(os, 'linux', 'Linux'); option(os, 'macos', 'macOS'); option(os, 'windows', 'Windows x86_64 preview'); osLabel.append(os); create.node.append(osLabel);
  const label = el('label', 'Architecture'); const architecture = el('select'); architecture.name = 'arch'; option(architecture, 'aarch64', 'aarch64'); option(architecture, 'x86_64', 'x86_64'); label.append(architecture); create.node.append(label); submit(create.node, '등록 및 인증 파일 발급'); panel.append(create.details);
  os.addEventListener('change', () => { if (os.value === 'windows') architecture.value = 'x86_64'; });
  panel.append(el('p', '등록한 에이전트는 OS·아키텍처·Pool과 실행 조건이 일치하는 프로젝트의 작업을 받습니다. Docker 엔진과 가용성은 연결 후 자동으로 확인합니다. Pool은 전용 실행기를 구분하는 선택 조건입니다.', 'caption')); content.append(panel);
  panel.append(el('p', '셀프 패치는 Mac·Windows Agent를 지원합니다. 적용 후 정상 연결을 확인하고 이전 작업 수신 상태로 돌아갑니다. 릴리스 등록은 서버 운영자가 수행합니다.', 'caption'));
  let disposed = false; let loading = false;
  const refreshStatus = async () => {
    if (disposed || loading || document.hidden) return;
    loading = true;
    try {
      const latest = await api<BuildAgent[]>('/api/agents');
      if (disposed || !content.isConnected) return;
      // Rebuild management actions when registration or control state changes elsewhere.
      const changed = latest.length !== liveAgents.length || latest.some(agent => {
        const previous = liveAgents.find(item => item.id === agent.id);
        return !previous || (['active', 'draining', 'name', 'os', 'arch', 'pool', 'self_patch_ready', 'binary_sha256', 'agent_version', 'perforce_managed'] as const).some(key => agent[key] !== previous[key]);
      });
      if (changed) { await dashboard(); return; }
      liveAgents = latest; repaint(); checked.textContent = `${new Date().toLocaleTimeString('ko-KR')} 갱신 · 5초 간격 자동 갱신`;
    } catch { if (!disposed) checked.textContent = '상태 갱신 실패 · 마지막 수신 정보입니다. 새로고침으로 다시 확인하세요.'; }
    finally { loading = false; }
  };
  checked.textContent = `${new Date().toLocaleTimeString('ko-KR')} 갱신 · 5초 간격 자동 갱신`;
  const timer = window.setInterval(() => { repaint(); void refreshStatus(); }, 5000);
  document.addEventListener('visibilitychange', refreshStatus);
  disposeAgentsView = () => { disposed = true; window.clearInterval(timer); document.removeEventListener('visibilitychange', refreshStatus); };

}

async function runView(content: HTMLElement, id: string, tab: string) {
  if (tab === 'graph') {
    const jobs = await api<GraphJob[]>(`/api/runs/${id}/jobs`);
    if (!content.isConnected) return;
    const graph = createRunGraph(id, jobs, {
      initialJobId: currentRoute.job,
      onSelection: job => {
        if (!content.isConnected || currentRoute.page !== 'run' || currentRoute.run !== id || currentRoute.tab !== 'graph') return;
        currentRoute = {...currentRoute, job};
        history.replaceState(null, '', routeHash(currentRoute));
      },
      dispatchHref: routeHash({...currentRoute, tab: 'dispatch'}),
      jobs: () => api<GraphJob[]>(`/api/runs/${id}/jobs`),
      logs: (attempt, after) => api<GraphLog[]>(`/api/attempts/${attempt}/logs?after=${after}`),
    });
    disposeRunGraph?.(); disposeRunGraph = graph.dispose;
    content.append(graph.element);
    return;
  }
  if (tab === 'info') {
    const run = currentRun!;
    const info = el('section', undefined, 'card'); info.append(el('h2', '실행 정보'), el('p', `실행 ID: ${id}`), el('p', `등록 시각: ${date(run.created_at)}`), el('p', `CL ${run.source_snapshot.changelist} · 발행 v${run.version}`)); content.append(info);
    const outputs = await api<{ publication: {status: string; changelist: number} | null; children: {pipeline_id: string; pipeline_name: string; version: number; run_id: string | null; status: string; reason: string}[]; parent_run_id: string | null }>(`/api/runs/${id}/outputs`);
    if (outputs.publication || outputs.children.length || outputs.parent_run_id) {
      const result = el('section', undefined, 'card'); result.append(el('h2', 'Perforce 제출 · 후속 실행'));
      result.append(link('전체 실행 체인 보기 →', {...currentRoute,tab:'chain'}));
      if (outputs.parent_run_id) result.append(link(`선행 실행 ${outputs.parent_run_id.slice(0, 8)} →`, {page:'run',run:outputs.parent_run_id,tab:'chain'}));
      if (outputs.publication) result.append(el('p', outputs.publication.status === 'prepared' ? `CL ${outputs.publication.changelist} 제출 예약 · 결과 확인 필요` : `CL ${outputs.publication.changelist} · ${outputs.publication.status === 'submitted' ? '생성 파일 제출 확인' : '변경 없음 · 입력 CL 유지'}`));
      for (const child of outputs.children) {
        const row = el('div', undefined, 'row'); row.append(el('span', `${child.pipeline_name} v${child.version} · ${child.status}${child.reason ? ` · ${child.reason}` : ''}`));
        if (child.run_id) row.append(button('후속 실행 보기', async () => { await openRun(child.pipeline_id, child.run_id!); }, 'small'));
        result.append(row);
      }
      content.append(result);
    }
    return;
  }
  const inputs = await api<{artifact_id: string; source_run_id: string; name: string; size: number; sha256: string}[]>(`/api/runs/${id}/inputs`);
  if (inputs.length) {
    const incoming = el('section', undefined, 'card'); incoming.append(el('h2', `전달받은 산출물 (${inputs.length})`));
    incoming.append(el('p', '실행 등록 시 제공 실행과 SHA-256을 고정했습니다. Agent가 모든 파일을 검증한 뒤 P4FORGE_INPUTS 경로에서 읽기 전용으로 제공합니다.', 'caption'));
    for (const input of inputs) {
      const row = el('div', undefined, 'stack'); row.append(el('strong', input.name), el('span', `${input.size.toLocaleString()} bytes · 제공 실행 ${input.source_run_id}`, 'caption'), el('code', `SHA-256 ${input.sha256}`, 'artifact-hash')); row.append(link('제공 실행 보기 →', {page:'run',run:input.source_run_id,tab:'artifacts'})); incoming.append(row, el('hr'));
    }
    content.append(incoming);
  }
  const artifacts = await api<{id: string; name: string; size: number; sha256: string; purge_requested_at: string | null; deleted_at: string | null}[]>(`/api/runs/${id}/artifacts`);
  const files = el('section', undefined, 'card'); files.append(el('h2', `빌드 산출물 (${artifacts.length})`));
  if (!artifacts.length) files.append(el('p', '산출물을 설정한 빌드가 성공하면 이곳에서 다운로드할 수 있습니다.', 'caption'));
  for (const artifact of artifacts) {
    const row = el('div', undefined, 'stack');
    const file = artifact.purge_requested_at ? el('span', `${artifact.name} · ${artifact.deleted_at ? '보존 기간 만료로 삭제됨' : '정리 중'}`) : el('a', artifact.name);
    if (file instanceof HTMLAnchorElement) { file.href = `/api/runs/${id}/artifacts/${artifact.id}`; file.download = artifact.name; }
    row.append(file, el('span', `${artifact.size.toLocaleString()} bytes`, 'caption'), el('code', `SHA-256 ${artifact.sha256}`, 'artifact-hash')); files.append(row, el('hr'));
  }
  content.append(files);
}

async function start() {
  const bootstrap = await api<{ google_configured: boolean; login_url: string; workspace_domain: string }>('/api/bootstrap');
  const response = await fetch('/api/me', { credentials: 'same-origin' });
  if (response.status === 401) {
    app.replaceChildren(); const hero = el('section', undefined, 'card hero'); hero.append(el('span', 'PERFORCE CI/CD', 'badge'), el('h1', '빌드의 시작을 한곳에서.'), el('p', '프로젝트, Perforce 소스, 파이프라인 설정과 발행 이력을 자체 서버에서 관리합니다. Google Workspace 계정으로 로그인하세요.'));
    if (bootstrap.google_configured) { const link = el('a', 'Google Workspace로 로그인', 'button primary'); link.href = bootstrap.login_url; hero.append(link, el('p', `${bootstrap.workspace_domain} 계정만 사용할 수 있습니다.`, 'caption')); }
    else hero.append(el('p', '관리자가 Google OAuth client 설정을 완료해야 로그인할 수 있습니다.', 'muted'));
    app.append(hero); return;
  }
  if (!response.ok) throw new Error(`사용자 조회 실패 (${response.status})`);
  user = await response.json() as User;
  identity.append(el('span', user.email), button('로그아웃', async () => { if (pipelineEditor && !pipelineEditor.canLeave()) return; await api('/auth/logout', 'POST'); pipelineEditor?.dispose(); pipelineEditor = undefined; location.reload(); }, 'small'));
  window.addEventListener('hashchange', () => { void dashboard().catch(error => message(String(error instanceof Error ? error.message : error), true)); });
  await dashboard();
}
start().catch(error => { app.replaceChildren(el('p', '서버 연결을 확인해 주세요.')); message(String(error instanceof Error ? error.message : error), true); });


type RetentionPolicy = { generation: number; enabled: boolean; retention_days: number; keep_successful_runs: number; last_sweep_at: string | null };
async function retentionView(content: HTMLElement, projectId: string) {
  const path = `/api/projects/${projectId}/artifact-retention`;
  const data = await api<{ policy: RetentionPolicy; summary: { stored_files: number; stored_bytes: number; pending_files: number; failed_deletions: number; deleted_files: number } }>(path);
  const panel = el('details', undefined, 'card'); panel.open = true; panel.append(el('summary', '산출물 보존 정책'));
  panel.append(el('p', `자동 정리 ${data.policy.enabled ? '켜짐' : '꺼짐'} · ${data.policy.retention_days}일 보관 · 파이프라인별 최근 성공 ${data.policy.keep_successful_runs}건 유지`));
  panel.append(el('p', `보관 중 ${data.summary.stored_files}개 · 업로드 기록 ${(data.summary.stored_bytes / 1048576).toFixed(2)} MiB · 정리 중 ${data.summary.pending_files}개 · 삭제 완료 ${data.summary.deleted_files}개`, 'caption'));
  if (data.summary.failed_deletions) panel.append(el('p', `파일 삭제 실패 ${data.summary.failed_deletions}개 · 자동 재시도 중입니다. 저장소 상태를 확인하세요.`, 'error'));
  panel.append(el('p', data.policy.last_sweep_at ? `마지막 정리 대상 확인: ${date(data.policy.last_sweep_at)}` : '아직 자동 정리 대상 확인 이력이 없습니다.', 'caption'));
  panel.append(el('p', '1분마다 확인합니다. 보관 기간이 지났어도 최근 성공 빌드는 유지하고, 실행 중인 업로드는 보호합니다. 실패·취소 등으로 남은 파일은 실행 종료 후 24시간이 지나면 정리합니다.', 'caption'));
  if (user.is_admin || project?.role === 'admin') {
    const fields = el('div', undefined, 'stack');
    const toggleLabel = el('label', '자동 정리', 'retention-toggle'); const toggle = el('input'); toggle.type = 'checkbox'; toggle.checked = data.policy.enabled; toggle.setAttribute('aria-label', '산출물 자동 정리'); toggleLabel.append(toggle); fields.append(toggleLabel);
    const daysLabel = el('label', '보관 기간 (일)'); const days = el('input'); days.type = 'number'; days.min = '1'; days.max = '3650'; days.value = String(data.policy.retention_days); daysLabel.append(days); fields.append(daysLabel);
    const keepLabel = el('label', '파이프라인별 최근 성공 유지 건수'); const keep = el('input'); keep.type = 'number'; keep.min = '1'; keep.max = '1000'; keep.value = String(data.policy.keep_successful_runs); keepLabel.append(keep); fields.append(keepLabel);
    const input = () => ({ generation: data.policy.generation, enabled: toggle.checked, retention_days: Number(days.value), keep_successful_runs: Number(keep.value) });
    const preview = el('div'); let previewed: string | undefined;
    const save = button('보존 정책 저장', async () => {
      if (previewed !== JSON.stringify(input())) throw new Error('변경한 정책의 정리 대상을 먼저 확인하세요.');
      await api(path, 'PUT', input()); await dashboard();
      const summary = Array.from(document.querySelectorAll('summary')).find(node => node.textContent === '산출물 보존 정책');
      if (summary?.parentElement instanceof HTMLDetailsElement) summary.parentElement.open = true;
      message('산출물 보존 정책을 DB에 저장했습니다.');
    }, 'primary'); save.disabled = true;
    const invalidate = () => { previewed = undefined; save.disabled = true; preview.replaceChildren(); };
    for (const field of [toggle, days, keep]) field.addEventListener('input', invalidate);
    fields.append(button('정리 대상 미리보기', async () => {
      const snapshot = input();
      const result = await api<{ count: number; bytes: number; files: { pipeline_name: string; run_id: string; name: string; reason: string }[] }>(`${path}/preview`, 'POST', snapshot);
      if (JSON.stringify(snapshot) !== JSON.stringify(input())) return;
      previewed = JSON.stringify(snapshot); save.disabled = false;
      preview.replaceChildren(el('p', `현재 정리 대상 ${result.count}개 · 업로드 기록 ${(result.bytes / 1048576).toFixed(2)} MiB`));
      for (const file of result.files) preview.append(el('p', `${file.pipeline_name} / ${file.run_id.slice(0, 8)} · ${file.name} · ${file.reason === 'expired' ? '보관 기간 초과' : '실패·미완료 업로드'}`, 'caption'));
      if (result.count > result.files.length) preview.append(el('p', `전체 ${result.count}개 중 ${result.files.length}개를 표시합니다.`, 'caption'));
    }), preview, save);
    fields.append(el('p', '삭제한 파일은 백업에서만 복원할 수 있습니다. 실행 이력·파일명·해시는 남습니다. 정책을 끄면 새 정리 작업을 중지하며 이미 정리 중인 파일의 삭제는 완료합니다.', 'caption'));
    panel.append(fields);
  }
  content.append(panel);
}

type DeploymentEnvironment = { id: string; project_id: string; name: string; pipeline_id: string; generation: number; active_run_id: string | null; current_run_id: string | null; previous_run_id: string | null; health: string; checked_at: string | null; executor_agent_id: string | null };
type DeploymentExecutor = AgentStatus & { id: string; name: string; pool: string; os: string; arch: string; draining: boolean };
type RunDeployment = { status: string; environment: DeploymentEnvironment; rollback_of: string | null; approver_email: string | null; approved_at: string | null; note: string; executor_agent_id: string | null };
function executorName(agents: DeploymentExecutor[], id: string | null): string {
  if(!id)return '미지정 · 배포 승인 불가';
  const agent=agents.find(a=>a.id===id);
  if (!agent) return id;
  const docker = dockerStatus(agent);
  return `${agent.name} · ${agent.os}/${agent.arch} · Docker ${docker.label} (${docker.detail}) · Pool ${agent.pool}${!agent.active?' · 폐기됨':agent.draining?' · 새 작업 중지':''}`;
}
const deploymentNames: Record<string,string> = { awaiting_approval:'승인 대기', approved:'승인 완료 · 배정 대기', running:'배포 중', succeeded:'상태 확인 성공', rejected:'승인 거절', canceled:'실행 전 취소', recovery_required:'복구 확인 필요 · 환경 잠금 유지', resolved:'외부 작업 종료 확인됨' };
async function openDeployment(pipeline: string, run: string) { await openRun(pipeline, run, 'deployment'); }
async function environmentsView(content: HTMLElement,projectId: string,pipelines: Pipeline[]) {
  const environments = await api<(DeploymentEnvironment & { active_deployment_status: string | null; pending_approval_count: number; pending_approval_run_id: string | null })[]>(`/api/projects/${projectId}/environments`);
  const executors = await api<DeploymentExecutor[]>(`/api/projects/${projectId}/deployment-agents`);
  if (!content.isConnected) return;
  const panel=el('section'); panel.className='card'; panel.append(el('h2',`배포 환경 (${environments.length})`));
  if (!environments.length) panel.append(el('p', '등록된 배포 환경이 없습니다.', 'empty'));
  panel.append(el('p','환경마다 승인된 실행 하나만 배포합니다. 상태는 마지막 배포의 확인 결과이며 지속적인 서비스 모니터링은 아닙니다. 현재·직전 성공 배포의 입력 산출물은 자동 정리에서 보호합니다.','caption'));
  const admin=user.is_admin || project?.role==='admin';
  for(const env of environments) {
    const row=el('section',undefined,'stack'); row.append(el('h3',env.name),el('code',env.id),el('p',`${env.active_run_id ? '잠김' : '실행 가능'} · 마지막 확인 ${env.health==='healthy'?'정상':'미확인'} · ${env.checked_at?date(env.checked_at):'확인 이력 없음'} · g${env.generation}`));
    const state = env.active_deployment_status;
    if (state) row.append(el('span', deploymentNames[state] ?? state, `badge ${state === 'recovery_required' ? 'failed' : 'running'}`));
    if (env.pending_approval_count) {
      row.append(el('span', `승인 대기 ${env.pending_approval_count}건`, 'badge queued'));
      if (env.pending_approval_run_id) row.append(link('최근 승인 요청 보기', { page: 'run', project: projectId, pipeline: env.pipeline_id, run: env.pending_approval_run_id, tab: 'deployment' }));
    }
    row.append(el('p',`허용 Agent: ${executorName(executors,env.executor_agent_id)}`));
    if(admin && !env.active_run_id) {
      const binding=form(`${env.name} 허용 Agent 설정`,async data=>{
        await api(`/api/environments/${env.id}/executor`,'POST',{generation:env.generation,agent_id:data.get('agent_id'),note:data.get('note')});
        await dashboard();message('환경의 허용 Agent를 저장했습니다. 새 승인은 이 Agent에 고정됩니다.');
      });
      binding.node.append(el('p','공용 에이전트 중 이 환경에 지정된 에이전트만 배포를 받습니다. 파이프라인의 OS·아키텍처·Pool과 Docker 조건도 일치해야 합니다. 승인 또는 실행 중인 배포가 있으면 변경할 수 없습니다.','caption'));
      const label=el('label','허용 Agent');const select=el('select');select.name='agent_id';select.required=true;
      option(select,'','Agent 선택');
      for(const agent of executors.filter(a=>a.active))option(select,agent.id,executorName(executors,agent.id));
      if(env.executor_agent_id)select.value=env.executor_agent_id;
      label.append(select);binding.node.append(label);field(binding.node,'Agent 지정 사유','note');submit(binding.node,'허용 Agent 저장');row.append(binding.details);
    }
    if(env.active_run_id) row.append(button(`환경 점유 실행 ${env.active_run_id.slice(0,8)}`,async()=>openDeployment(env.pipeline_id,env.active_run_id!),'small'));
    for(const [label,run] of [['현재 성공',env.current_run_id],['직전 성공',env.previous_run_id]]) if(run) row.append(button(`${label} ${run.slice(0,8)}`,async()=>openDeployment(env.pipeline_id,run),'small'));
    if(admin && !env.active_run_id && env.current_run_id) {
      let pending: { target_run_id: string; note: string; request_id: string } | undefined;
      const restore=form(`${env.name} 성공 배포 복구 요청`,async data=>{
        const target=String(data.get('target_run_id')); const note=String(data.get('note')).trim();
        if(!pending || pending.target_run_id!==target || pending.note!==note) pending={target_run_id:target,note,request_id:crypto.randomUUID()};
        const run=await api<Run>(`/api/environments/${env.id}/rollback`,'POST',{generation:env.generation,...pending});
        await openDeployment(env.pipeline_id,run.id); message('이전 성공 배포의 고정 설정과 산출물로 복구 실행을 만들었습니다. 검토 후 별도 승인하세요.');
      });
      const label=el('label','복구할 성공 실행');const targets=el('select');targets.name='target_run_id';
      option(targets,env.current_run_id,`현재 성공 ${env.current_run_id}`);if(env.previous_run_id) option(targets,env.previous_run_id,`직전 성공 ${env.previous_run_id}`);
      label.append(targets);restore.node.append(label);field(restore.node,'복구 요청 사유','note');submit(restore.node,'복구 실행 요청');row.append(restore.details);
    }
    panel.append(row,el('hr'));
  }
  if(admin) {
    const add=form('배포 환경 추가',async data=>{await api(`/api/projects/${projectId}/environments`,'POST',{name:data.get('name'),pipeline_id:data.get('pipeline_id')});await dashboard();message('배포 환경을 만들었습니다. 표시된 UUID를 해당 파이프라인의 deployment.environment_id에 설정하고 허용 Agent를 지정하세요.');});
    field(add.node,'환경 이름','name');const label=el('label','배포 파이프라인');const select=el('select');select.name='pipeline_id';
    for(const pipeline of pipelines) option(select,pipeline.id,pipeline.name);label.append(select);add.node.append(label);submit(add.node,'환경 생성');panel.append(add.details);
  }
  panel.append(button('환경 상태 새로고침',async()=>dashboard(),'small'));content.append(panel);
}
async function deploymentView(content: HTMLElement,run: string) {
  const data=await api<{deployment:RunDeployment|null;input_hash:string;version:number;changelist:number}>(`/api/runs/${run}/deployment`);
  const deployment=data.deployment;if(!deployment)return;
  const env=deployment.environment;const panel=el('section',undefined,'card');panel.append(el('h2',`배포 · ${env.name}`),el('strong',deploymentNames[deployment.status]??deployment.status));
  const executors=await api<DeploymentExecutor[]>(`/api/projects/${env.project_id}/deployment-agents`);
  const executor=deployment.status==='awaiting_approval'?env.executor_agent_id:deployment.executor_agent_id;
  panel.append(el('p',`${deployment.status==='awaiting_approval'?'승인할 Agent':'승인 시 Agent'}: ${executor ? executorName(executors,executor) : deployment.status==='awaiting_approval'?'미지정 · 환경에서 Agent를 지정하세요':'지정 이력 없음 (기존 실행)'}`));
  panel.append(el('p',`CL ${data.changelist} · 발행 v${data.version} · 환경 g${env.generation}`),el('code',`입력 SHA-256 ${data.input_hash}`,'artifact-hash'));
  if(deployment.rollback_of)panel.append(el('p',`복구 원본 실행 ${deployment.rollback_of}`));
  if(deployment.approver_email)panel.append(el('p',`승인 ${deployment.approver_email} · ${deployment.approved_at?date(deployment.approved_at):''}`,'caption'));
  if(deployment.note)panel.append(el('p',deployment.note));
  const admin=user.is_admin || project?.role==='admin';
  if(admin && deployment.status==='awaiting_approval') {
    panel.append(el('p','발행 설정, CL, 제공 실행과 파일 해시를 확인한 뒤 승인하세요. 승인하면 환경을 점유하며, 마지막 상태 확인 작업까지 성공해야 정상 배포로 기록합니다.','caption'));
    const formNode=el('form');const reason=field(formNode,'배포 승인·거절 사유','note');
    for(const [label,approve] of [['배포 승인',true],['승인 거절',false]] as const) formNode.append(button(label,async()=>{
      if(!reason.value.trim())throw new Error('승인·거절 사유를 입력하세요.');
      await api(`/api/runs/${run}/deployment`,'POST',{generation:env.generation,input_hash:data.input_hash,approve,note:reason.value.trim()});await dashboard();message(approve?'배포를 승인하고 환경을 잠갔습니다.':'배포 승인을 거절했습니다.');
    },approve?'primary':'danger'));
    panel.append(formNode);
  }
  if(admin && deployment.status==='recovery_required') {
    const recovery=form('외부 작업 종료 확인 및 잠금 해제',async data=>{
      await api(`/api/environments/${env.id}/recover`,'POST',{generation:env.generation,run_id:run,stopped_confirmed:data.get('stopped')==='on',note:data.get('note')});await dashboard();message('환경 잠금을 해제했습니다. 정상 상태는 아직 확인되지 않았습니다. 성공 배포 복구 요청을 만들 수 있습니다.');
    });
    recovery.node.append(el('p','Agent 연결 유실이나 취소만으로 외부 작업 종료를 보장할 수 없습니다. 대상 호스트의 배포 프로세스가 종료됐는지 직접 확인한 뒤 해제하세요.','caption'));
    const stopped=field(recovery.node,'대상 호스트의 외부 배포 작업 종료를 확인했습니다','stopped','','checkbox');stopped.required=true;field(recovery.node,'종료 확인 근거','note');submit(recovery.node,'종료 확인 후 잠금 해제');panel.append(recovery.details);
  }
  content.append(panel);
}
