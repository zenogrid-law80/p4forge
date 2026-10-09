import type {Route} from './navigation.js';

type RecentRun = {id: string; status: string; cancel_requested: boolean; changelist: number; created_at: string; deployment_status: string | null};
type VersionNode = {key: string; pipeline_id: string; name: string; version: number; current_version: number; published_at: string; job_count: number; artifact_inputs: string[]; artifacts: string[]; publishes_generated: boolean; deployment: boolean; latest_run: RecentRun | null};
type Edge = {from: string; to: string; artifacts: string[]; changelist_mode: 'input' | 'published_or_input'};
type Report = {checked_at: string; project_id: string; root: string | null; nodes: VersionNode[]; edges: Edge[]};
type Context = {route: Route; revisions: {version: number}[]; api: <T>(path: string) => Promise<T>; link: (title: string, route: Route, className?: string) => HTMLAnchorElement; navigate: (route: Route) => Promise<void>};
export const CONNECTION_WIDTH = 272;
export const CONNECTION_HEIGHT = 164;

// Version keys distinguish old pinned targets from current versions of the same pipeline.
export function layoutConnections(nodes: {key: string}[], edges: {from: string; to: string}[]) {
  const keys = new Set(nodes.map(node => node.key));
  if (keys.size !== nodes.length) throw new Error('중복된 발행 버전이 있습니다.');
  const pending = new Map(nodes.map(node => [node.key, 0]));
  const ranks = new Map(nodes.map(node => [node.key, 0]));
  const children = new Map(nodes.map(node => [node.key, new Set<string>()]));
  for (const edge of edges) {
    if (!keys.has(edge.from) || !keys.has(edge.to)) throw new Error('연결된 발행 버전을 찾을 수 없습니다.');
    if (!children.get(edge.from)!.has(edge.to)) {
      children.get(edge.from)!.add(edge.to); pending.set(edge.to, pending.get(edge.to)! + 1);
    }
  }
  const ready = nodes.filter(node => !pending.get(node.key)).map(node => node.key);
  for (let i = 0; i < ready.length; i++) {
    for (const child of children.get(ready[i])!) {
      ranks.set(child, Math.max(ranks.get(child)!, ranks.get(ready[i])! + 1));
      pending.set(child, pending.get(child)! - 1);
      if (!pending.get(child)) ready.push(child);
    }
  }
  if (ready.length !== nodes.length) throw new Error('순환 연결이 있어 목록으로 표시합니다.');
  const columns = Array.from({length: nodes.length ? Math.max(...ranks.values()) + 1 : 0}, (_, rank) => nodes.filter(node => ranks.get(node.key) === rank));
  const rows = Math.max(0, ...columns.map(column => column.length));
  const placed = columns.flatMap((column, rank) => column.map((node, row) => ({key: node.key, x: 20 + rank * (CONNECTION_WIDTH + 88), y: 20 + (row + (rows - column.length) / 2) * (CONNECTION_HEIGHT + 24)})));
  return {nodes: placed, width: columns.length ? 40 + columns.length * (CONNECTION_WIDTH + 88) - 88 : 0, height: rows ? 40 + rows * (CONNECTION_HEIGHT + 24) - 24 : 0};
}

const names: Record<string, string> = {queued: '배정 대기', running: '실행 중', succeeded: '성공', failed: '실패', lost: '연결 유실', canceled: '취소', awaiting_approval: '배포 승인 대기', recovery_required: '배포 복구 확인 필요'};
function runLabel(run: RecentRun) { return run.deployment_status && ['awaiting_approval','recovery_required'].includes(run.deployment_status) ? names[run.deployment_status] : run.cancel_requested && ['queued','running'].includes(run.status) ? '취소 처리 중' : names[run.status] ?? run.status; }
function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string) { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; }

export function createConnectionsView(content: HTMLElement, context: Context): () => void {
  const panel = el('section', undefined, 'card pipeline-connections');
  panel.append(el('h2', '연결 관계'), el('p', '선택한 발행 버전이 성공한 뒤 이어지는 파이프라인과 전달할 산출물을 확인합니다.', 'caption'));
  const controls = el('div', undefined, 'row connection-controls');
  const label = el('label', '연결 기준 발행 버전'); const select = el('select');
  const addVersion = (version: string) => {if (!Array.from(select.options).some(option => option.value === version)) {const option = el('option', `v${version}`); option.value = version; select.append(option);}};
  for (const revision of context.revisions) addVersion(String(revision.version));
  if (context.route.version) addVersion(context.route.version);
  select.value = context.route.version ?? select.options[0]?.value ?? ''; select.disabled = !select.options.length;
  label.append(select); const refresh = el('button', '연결 새로고침', 'small'); refresh.type = 'button';
  const status = el('span', '불러오는 중…', 'caption'); status.setAttribute('role', 'status'); controls.append(label, refresh, status); panel.append(controls);
  const body = el('div'); panel.append(body); content.append(panel);
  let disposed = false; let loading = false; let checked: string | undefined; let selected: string | undefined;
  const render = (data: Report) => {
    const oldViewport = body.querySelector('.connections-viewport'); const scroll = {left: oldViewport?.scrollLeft ?? 0, top: oldViewport?.scrollTop ?? 0};
    body.replaceChildren();
    if (!data.root || !data.nodes.length) {body.append(el('p', '발행된 버전이 없습니다. 초안을 발행하면 연결 관계를 확인할 수 있습니다.', 'empty')); return;}
    const root = data.nodes.find(node => node.key === data.root)!;
    addVersion(String(root.version)); select.value = String(root.version); select.disabled = false;
    selected = data.nodes.some(node => node.key === selected) ? selected : data.root;
    body.append(el('p', `시작 ${root.name} v${root.version} · 연결된 발행 버전 ${data.nodes.length}개 · 성공 후 연결 ${data.edges.length}개`, 'caption'));
    body.append(el('p', '화살표는 성공 후 실행 등록을 뜻합니다. 후속 버전은 발행 설정에 고정되며, 배포 단계는 별도 승인을 거칩니다. 각 노드의 최근 실행은 해당 버전의 개별 이력입니다.', 'caption'));
    if (!data.edges.length) body.append(el('p', '이 버전에 설정된 후속 파이프라인이 없습니다.', 'caption'));
    body.append(el('p', '노드를 선택하면 연결 조건과 실행 이력을 볼 수 있습니다. 넓은 그래프는 좌우로 스크롤하세요.', 'caption'));
    const viewport = el('div', undefined, 'connections-viewport'); viewport.tabIndex = 0; viewport.setAttribute('role','region'); viewport.setAttribute('aria-label','발행 버전 연결 그래프 · 넘치는 영역은 스크롤하여 확인');
    const canvas = el('div', undefined, 'connections-canvas'); viewport.append(canvas);
    let layout: ReturnType<typeof layoutConnections> | undefined;
    try {layout = layoutConnections(data.nodes, data.edges);} catch (error) {body.append(el('p', error instanceof Error ? error.message : '그래프를 목록으로 표시합니다.', 'graph-error')); canvas.classList.add('connections-fallback');}
    if (layout) {
      canvas.style.width = `${layout.width}px`; canvas.style.height = `${layout.height}px`;
      const svg = document.createElementNS('http://www.w3.org/2000/svg','svg'); svg.setAttribute('width',String(layout.width)); svg.setAttribute('height',String(layout.height)); svg.setAttribute('aria-hidden','true'); svg.classList.add('graph-edges');
      const positions = new Map(layout.nodes.map(node => [node.key,node]));
      for (const edge of data.edges) {
        const from = positions.get(edge.from)!; const to = positions.get(edge.to)!;
        const x = from.x + CONNECTION_WIDTH, y = from.y + CONNECTION_HEIGHT / 2, end = to.x - 5, bottom = to.y + CONNECTION_HEIGHT / 2;
        const path = document.createElementNS(svg.namespaceURI,'path'); path.setAttribute('d',`M ${x} ${y} C ${x+44} ${y}, ${end-44} ${bottom}, ${end} ${bottom} M ${end-7} ${bottom-5} L ${end} ${bottom} L ${end-7} ${bottom+5}`); path.setAttribute('class','graph-edge'); svg.append(path);
      }
      canvas.append(svg);
    }
    const details = el('section', undefined, 'connection-detail'); details.id = 'pipeline-connection-details'; details.setAttribute('aria-label','선택한 발행 버전 상세');
    const buttons = new Map<string, HTMLButtonElement>();
    const versionRoute = (node: VersionNode): Route => ({page:'pipeline',project:data.project_id,pipeline:node.pipeline_id,tab:'connections',version:String(node.version)});
    const choose = (key: string) => {
      selected = key; const node = data.nodes.find(node => node.key === key)!;
      for (const [id, button] of buttons) button.setAttribute('aria-pressed', String(id === selected));
      details.replaceChildren(el('h3',`${node.name} · v${node.version}`), el('p', `발행 ${new Date(node.published_at).toLocaleString('ko-KR')} · 작업 ${node.job_count}개 · 현재 발행 v${node.current_version}`, 'caption'));
      if (node.version !== node.current_version) details.append(el('p', `현재 발행 버전(v${node.current_version})과 별도로 고정된 v${node.version}을 사용합니다.`, 'caption'));
      details.append(el('p', `입력 산출물: ${node.artifact_inputs.join(', ') || '없음'}`, 'caption'),el('p', `보관할 산출물: ${node.artifacts.join(', ') || '없음'}`, 'caption'));
      if (node.publishes_generated) details.append(el('p', '생성 파일을 Perforce에 게시합니다. 후속 실행은 게시 결과 CL 또는 변경이 없을 때 입력 CL을 사용합니다.', 'caption'));
      if (node.deployment) details.append(el('p', '배포 환경의 승인과 지정 에이전트 조건을 충족한 뒤 실행됩니다.', 'caption'));
      const actions = el('div', undefined, 'row');
      if (node.key !== data.root) actions.append(context.link('이 버전에서 연결 보기 →', versionRoute(node)));
      actions.append(context.link('발행 이력 →', {...versionRoute(node),tab:'versions',version:undefined}));
      if (node.latest_run) {
        const run = node.latest_run;
        details.append(el('p', `이 버전의 최근 실행: ${runLabel(run)} · CL ${run.changelist} · ${new Date(run.created_at).toLocaleString('ko-KR')}`, 'caption'));
        actions.append(context.link(`실행 ${run.id.slice(0,8)} 보기 →`,{page:'run',project:data.project_id,pipeline:node.pipeline_id,run:run.id,tab:run.deployment_status === 'awaiting_approval' || run.deployment_status === 'recovery_required' ? 'deployment' : 'graph'}));
      } else details.append(el('p', '이 버전의 실행 이력이 없습니다.', 'caption'));
      details.append(actions);
      const related = data.edges.filter(edge => edge.from === node.key || edge.to === node.key);
      if (related.length) {
        details.append(el('h4','연결과 전달 조건')); const list = el('ul', undefined, 'connection-transfers');
        for (const edge of related) {
          const from = data.nodes.find(item => item.key === edge.from)!; const to = data.nodes.find(item => item.key === edge.to)!;
          const item = el('li'); item.append(context.link(`${from.name} v${from.version}`,versionRoute(from)), document.createTextNode(' → '),context.link(`${to.name} v${to.version}`,versionRoute(to)));
          item.append(el('p', `성공 후 등록 · ${edge.changelist_mode === 'published_or_input' ? '게시 결과 CL (변경 없으면 입력 CL)' : '입력 CL 유지'} · ${edge.artifacts.length ? `산출물: ${edge.artifacts.join(', ')}` : '전달할 산출물 없음'}`, 'caption')); list.append(item);
        }
        details.append(list);
      }
    };
    for (const node of data.nodes) {
      const button = el('button', undefined, 'connection-node'); button.type = 'button'; button.setAttribute('aria-label', `${node.name} v${node.version} 연결 상세`); button.setAttribute('aria-controls',details.id);
      const point = layout?.nodes.find(point => point.key === node.key); if (point) {button.style.left = `${point.x}px`; button.style.top = `${point.y}px`;}
      button.append(el('strong',node.name),el('span',`v${node.version}${node.version !== node.current_version ? ` · 현재 v${node.current_version}` : ' · 현재 발행'}`, 'caption'),el('span',`${node.job_count}개 작업 · ${node.deployment ? '승인 배포' : '파이프라인'} · 입력 ${node.artifact_inputs.length} / 산출물 ${node.artifacts.length}`, 'caption'),el('span',node.latest_run ? `최근 실행 · ${runLabel(node.latest_run)}` : '이 버전 실행 없음','caption'));
      button.addEventListener('click',()=>choose(node.key)); buttons.set(node.key,button); canvas.append(button);
    }
    body.append(viewport,details); choose(selected ?? root.key); viewport.scrollLeft = scroll.left; viewport.scrollTop = scroll.top;
  };
  const fetchGraph = async () => {
    if (disposed || loading) return; loading = true; refresh.disabled = true;
    try {
      const data = await context.api<Report>(`/api/pipelines/${encodeURIComponent(context.route.pipeline!)}/connections${context.route.version ? `?version=${encodeURIComponent(context.route.version)}` : ''}`);
      if (disposed || !content.isConnected) return;
      render(data); checked = data.checked_at; status.className = 'caption'; status.textContent = `${new Date(checked).toLocaleString('ko-KR')} 기준`;
    } catch (error) {if (!disposed && content.isConnected) {status.className = 'graph-error'; status.textContent = `연결 조회 실패${checked ? ` · ${new Date(checked).toLocaleString('ko-KR')}의 마지막 결과입니다` : ''}: ${error instanceof Error ? error.message : String(error)}`;}}
    finally {loading = false; refresh.disabled = false;}
  };
  select.addEventListener('change',()=>{void context.navigate({...context.route,version:select.value}).catch(error=>{status.textContent=String(error);});});
  refresh.addEventListener('click',()=>{void fetchGraph();}); void fetchGraph();
  return () => {disposed = true;};
}
