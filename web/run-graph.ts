export type GraphJob = {
  id: string; name: string; stage: string | null; needs: string[]; status: string;
  shared_workspace: boolean; attempt_id: string | null; agent_name: string | null;
  reason: string | null; exit_code: number | null; cancel_requested: boolean | null;
};
export type GraphLog = { sequence: number; stream: string; content: string };
type GraphNode = { job: GraphJob; x: number; y: number };
type GraphEdge = { from: GraphNode; to: GraphNode };
type GraphLane = { stage: string; x: number; y: number; height: number };

export const NODE_WIDTH = 244;
export const NODE_HEIGHT = 138;
const COLUMN_GAP = 76;
const ROW_GAP = 18;
const PADDING = 20;

export function graphScale(value: number): number {
  return Number.isFinite(value) ? Math.min(2, Math.max(0.01, value)) : 1;
}
export function fitGraphScale(width: number, height: number, availableWidth: number, availableHeight: number): number {
  if (Math.min(width, height, availableWidth, availableHeight) <= 0) return 1;
  return graphScale(Math.min(1, Math.max(1, availableWidth - 24) / width, Math.max(1, availableHeight - 24) / height));
}
export function adjacentFailure(jobs: GraphJob[], selected: string | undefined, direction: -1 | 1): string | undefined {
  const failures = jobs.filter(job => job.status === 'failed' || job.status === 'lost');
  if (!failures.length) return undefined;
  const index = failures.findIndex(job => job.id === selected);
  return failures[index < 0 ? (direction === 1 ? 0 : failures.length - 1) : (index + direction + failures.length) % failures.length].id;
}

// Only recorded dependencies determine columns. Stage labels never add edges.
export function layoutJobs(jobs: GraphJob[]): { nodes: GraphNode[]; edges: GraphEdge[]; lanes: GraphLane[]; width: number; height: number } {
  if (!jobs.length) return { nodes: [], edges: [], lanes: [], width: 0, height: 0 };
  const byName = new Map(jobs.map(job => [job.name, job]));
  if (byName.size !== jobs.length || new Set(jobs.map(job => job.id)).size !== jobs.length) throw new Error('중복 작업이 있어 그래프를 표시할 수 없습니다.');
  const pending = new Map<string, number>();
  const children = new Map<string, string[]>();
  const ranks = new Map<string, number>();
  for (const job of jobs) {
    const needs = [...new Set(job.needs)];
    pending.set(job.name, needs.length);
    ranks.set(job.name, 0);
    for (const name of needs) {
      if (!byName.has(name)) throw new Error(`선행 작업 “${name}”을 찾을 수 없습니다.`);
      const list = children.get(name) ?? [];
      list.push(job.name); children.set(name, list);
    }
  }
  const ready = jobs.filter(job => !pending.get(job.name)).map(job => job.name);
  for (let i = 0; i < ready.length; i++) {
    const name = ready[i];
    for (const child of children.get(name) ?? []) {
      ranks.set(child, Math.max(ranks.get(child)!, ranks.get(name)! + 1));
      pending.set(child, pending.get(child)! - 1);
      if (!pending.get(child)) ready.push(child);
    }
  }
  if (ready.length !== jobs.length) throw new Error('순환 의존 관계가 있어 그래프를 표시할 수 없습니다.');
  const nodes: GraphNode[] = [];
  const lanes: GraphLane[] = [];
  const stages = [...new Set(jobs.map(job => job.stage || '단계 미지정'))];
  const columns = Array.from({ length: Math.max(...ranks.values()) + 1 }, (_, rank) => jobs.filter(job => ranks.get(job.name) === rank));
  const maxRows = Math.max(...columns.map(column => column.length));
  const laneHeight = 36 + maxRows * (NODE_HEIGHT + ROW_GAP);
  for (const [rank, column] of columns.entries()) {
    column.sort((a, b) => stages.indexOf(a.stage || '단계 미지정') - stages.indexOf(b.stage || '단계 미지정'));
    const x = PADDING + rank * (NODE_WIDTH + COLUMN_GAP);
    const offset = (maxRows - column.length) * (NODE_HEIGHT + ROW_GAP) / 2;
    column.forEach((job, row) => nodes.push({ job, x, y: PADDING + 36 + offset + row * (NODE_HEIGHT + ROW_GAP) }));
    lanes.push({ stage: [...new Set(column.map(job => job.stage || '단계 미지정'))].join(' · '), x: x - 8, y: PADDING, height: laneHeight });
  }
  const placed = new Map(nodes.map(node => [node.job.name, node]));
  const edges = nodes.flatMap(to => [...new Set(to.job.needs)].map(name => ({ from: placed.get(name)!, to })));
  const width = PADDING * 2 + NODE_WIDTH + Math.max(...ranks.values()) * (NODE_WIDTH + COLUMN_GAP);
  return { nodes, edges, lanes, width, height: laneHeight + PADDING * 2 };
}

const statuses: Record<string, { label: string; symbol: string }> = {
  queued: { label: '대기', symbol: '◷' }, running: { label: '실행 중', symbol: '▶' },
  blocked: { label: '차단', symbol: 'Ⅱ' },
  succeeded: { label: '성공', symbol: '✓' }, failed: { label: '실패', symbol: '×' },
  lost: { label: '연결 유실', symbol: '!' }, canceled: { label: '취소', symbol: '−' },
  skipped: { label: '건너뜀', symbol: '↷' },
};
function status(job: GraphJob) {
  return job.cancel_requested && job.status === 'running' ? { label: '취소 처리 중', symbol: '◷' } : statuses[job.status] ?? { label: job.status, symbol: '?' };
}
function element<K extends keyof HTMLElementTagNameMap>(tag: K, text = '', className = '') {
  const node = document.createElement(tag); node.textContent = text; node.className = className; return node;
}
function action(text: string, callback: () => void, className = 'small') {
  const node = element('button', text, className); node.type = 'button'; node.addEventListener('click', callback); return node;
}

export function createRunGraph(runId: string, initialJobs: GraphJob[], api: {
  initialJobId?: string;
  onSelection?: (id: string) => void;
  dispatchHref?: string;
  jobs: () => Promise<GraphJob[]>;
  logs: (attempt: string, after: number) => Promise<GraphLog[]>;
}) {
  const panel = element('section', '', 'card run-graph');
  panel.setAttribute('aria-label', '실행 그래프');
  const heading = element('div', '', 'row space');
  heading.append(element('h2', '실행 그래프'), element('code', runId.slice(0, 8), 'caption'));
  const summary = element('p', '', 'graph-summary');
  const controls = element('div', '', 'row space graph-controls');
  const autoLabel = element('label', '', 'retention-toggle');
  const auto = element('input'); auto.type = 'checkbox'; auto.checked = true;
  autoLabel.append(auto, document.createTextNode('상태 자동 갱신 · 5초'));
  const refreshed = element('span', '', 'caption');
  const refreshButton = action('상태 새로고침', () => { void refresh(); });
  controls.append(autoLabel, refreshed, refreshButton);
  const error = element('p', '', 'graph-error'); error.hidden = true; error.setAttribute('role', 'alert');
  const viewport = element('div', '', 'graph-viewport'); viewport.tabIndex = 0;
  viewport.setAttribute('role', 'region'); viewport.setAttribute('aria-label', '작업 의존 관계 · 가로와 세로로 스크롤할 수 있습니다');
  const surface = element('div', '', 'graph-surface');
  const canvas = element('div', '', 'graph-canvas'); surface.append(canvas); viewport.append(surface);
  const navigation = element('div', '', 'graph-navigation'); navigation.setAttribute('aria-label', '그래프 탐색');
  const zoomControls = element('div', '', 'row');
  const zoomOut = action('축소', () => changeScale(scale / 1.25));
  const zoomReset = action('100%', () => changeScale(1)); zoomReset.setAttribute('aria-label', '원래 크기 · 100%');
  const zoomIn = action('확대', () => changeScale(scale * 1.25));
  const fit = action('화면 맞춤', () => { stopFollowing(); fitView(); });
  const locate = action('선택 작업으로 이동', () => { stopFollowing(); revealSelected(true); });
  const zoomLabel = element('span', '', 'caption'); zoomLabel.setAttribute('aria-live', 'polite');
  zoomControls.append(zoomOut, zoomReset, zoomIn, fit, zoomLabel, locate);
  const failureControls = element('div', '', 'row');
  const previousFailure = action('이전 실패', () => jumpFailure(-1));
  const nextFailure = action('다음 실패', () => jumpFailure(1));
  const failuresLabel = element('span', '', 'caption'); failuresLabel.setAttribute('aria-live', 'polite');
  failureControls.append(previousFailure, nextFailure, failuresLabel);
  const followLabel = element('label', '', 'retention-toggle');
  const follow = element('input'); follow.type = 'checkbox';
  followLabel.append(follow, document.createTextNode('실행 중 작업 따라가기'));
  const followNotice = element('span', '직접 작업을 선택하거나 그래프·로그를 탐색하면 따라가기가 꺼집니다.', 'caption'); followNotice.setAttribute('role', 'status');
  navigation.append(zoomControls, failureControls, followLabel, followNotice);
  const detail = element('section', '', 'graph-detail'); detail.setAttribute('aria-label', '선택한 작업 상세');
  const info = element('div');
  const logTitle = element('h3', '로그');
  const logNotice = element('p', '', 'caption'); logNotice.setAttribute('role', 'status');
  const output = element('pre', '', 'build-log'); output.tabIndex = 0; output.setAttribute('aria-label', '선택한 작업 로그');
  const more = action('다음 로그 / 새 로그 읽기', () => { void loadLogs(); });
  detail.append(info, logTitle, logNotice, output, more);
  const workspace = element('div', '', 'run-graph-workspace'); workspace.append(viewport, detail);
  panel.append(heading, summary, controls, error, element('p', '화살표는 선행 작업 → 후속 작업입니다. 작업을 선택하면 상세 정보와 로그를 확인할 수 있습니다.', 'caption'), navigation, workspace);

  let jobs = initialJobs;
  let selectedId: string | undefined;
  let initialJobId = api.initialJobId;
  if (initialJobId && !initialJobs.some(job => job.id === initialJobId)) panel.append(element('p', '링크에서 지정한 작업이 이 실행에 없습니다. 현재 실행의 작업을 표시합니다.', 'graph-error'));
  let selectedAttempt: string | null = null;
  let cursor = -1;
  let selectionVersion = 0;
  let disposed = false;
  let refreshing = false;
  let loadingLogs = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let topology = '';
  let scale = 1;
  let layout: ReturnType<typeof layoutJobs> | undefined;
  let infoSignature = '';
  let firstResize = true;
  const buttons = new Map<string, { button: HTMLButtonElement; badge: HTMLElement; agent: HTMLElement }>();
  let paths: { node: SVGPathElement; from: string; to: string }[] = [];
  const active = () => jobs.some(job => ['queued', 'running'].includes(job.status));
  const selectedJob = () => jobs.find(job => job.id === selectedId);

  function stopFollowing() {
    if (!follow.checked) return;
    follow.checked = false;
    followNotice.textContent = '수동 탐색으로 따라가기를 멈췄습니다. 다시 켜면 실행 중 작업으로 이동합니다.';
  }
  function paintScale() {
    const scalable = !!layout?.width;
    surface.classList.toggle('scaled', scalable);
    surface.style.width = scalable ? `${layout!.width * scale}px` : '';
    surface.style.height = scalable ? `${layout!.height * scale}px` : '';
    canvas.style.transform = scalable ? `scale(${scale})` : '';
    zoomLabel.textContent = scalable ? `배율 ${Math.round(scale * 100)}%` : '배율 조절 없음';
    zoomOut.disabled = !scalable || scale <= 0.01;
    zoomIn.disabled = !scalable || scale >= 2;
    zoomReset.disabled = fit.disabled = !scalable;
  }
  function changeScale(value: number) {
    if (!layout?.width) return;
    stopFollowing();
    const centerX = (viewport.scrollLeft + viewport.clientWidth / 2) / scale;
    const centerY = (viewport.scrollTop + viewport.clientHeight / 2) / scale;
    scale = graphScale(value); paintScale();
    viewport.scrollLeft = centerX * scale - viewport.clientWidth / 2;
    viewport.scrollTop = centerY * scale - viewport.clientHeight / 2;
  }
  function fitView() {
    if (!layout?.width || !viewport.clientWidth) return;
    scale = fitGraphScale(layout.width, layout.height, viewport.clientWidth, viewport.clientHeight);
    paintScale(); viewport.scrollLeft = viewport.scrollTop = 0;
  }
  function revealSelected(focus = false) {
    const button = selectedId ? buttons.get(selectedId)?.button : undefined;
    if (!button || !viewport.clientWidth) return;
    const bounds = button.getBoundingClientRect(); const view = viewport.getBoundingClientRect();
    viewport.scrollLeft += bounds.left - view.left + bounds.width / 2 - viewport.clientWidth / 2;
    viewport.scrollTop += bounds.top - view.top + bounds.height / 2 - viewport.clientHeight / 2;
    if (focus) button.focus({preventScroll: true});
  }
  function navigationState() {
    const failures = jobs.filter(job => job.status === 'failed' || job.status === 'lost');
    const index = failures.findIndex(job => job.id === selectedId);
    previousFailure.disabled = nextFailure.disabled = !failures.length;
    failuresLabel.textContent = `실패·연결 유실 ${failures.length}개${index < 0 ? '' : ` · ${index + 1}/${failures.length} 선택`}`;
    locate.disabled = !selectedJob(); follow.disabled = !active();
  }
  function jumpFailure(direction: -1 | 1) {
    stopFollowing();
    const id = adjacentFailure(jobs, selectedId, direction);
    if (id) { select(id); revealSelected(true); }
  }
  function followRunning() {
    if (!follow.checked) return;
    const running = selectedJob()?.status === 'running' ? selectedJob() : jobs.find(job => job.status === 'running');
    if (running) {
      const changed = running.id !== selectedId;
      select(running.id); if (changed) revealSelected();
      followNotice.textContent = `따라가는 작업: ${running.name}${auto.checked ? '' : ' · 자동 갱신 꺼짐, 수동 갱신 때 반영'}`;
    } else if (active()) followNotice.textContent = '실행 중인 작업을 기다립니다.';
    else { follow.checked = false; followNotice.textContent = '실행 중인 작업이 없어 따라가기를 마쳤습니다.'; }
  }
  function accessLost(cause: unknown): boolean {
    if (!cause || typeof cause !== 'object' || !('status' in cause) || ![401, 403].includes(Number(cause.status))) return false;
    dispose(); update([]); info.replaceChildren();
    error.textContent = '로그인이 만료되었거나 접근 권한이 없습니다. 화면을 새로고침해 다시 확인하세요.'; error.hidden = false;
    auto.checked = false; auto.disabled = refreshButton.disabled = true;
    return true;
  }

  function draw() {
    canvas.replaceChildren(); buttons.clear(); paths = [];
    layout = undefined;
    try { layout = layoutJobs(jobs); } catch (cause) {
      canvas.append(element('p', `${cause instanceof Error ? cause.message : cause} 작업 목록으로 표시합니다.`, 'graph-error'));
    }
    canvas.classList.toggle('graph-fallback', !layout);
    canvas.style.width = layout?.width ? `${layout.width}px` : '';
    canvas.style.height = layout?.height ? `${layout.height}px` : '';
    if (!jobs.length) { canvas.append(element('p', '표시할 작업이 없습니다.', 'empty')); paintScale(); return; }
    if (layout) {
      for (const lane of layout.lanes) {
        const band = element('div', '', 'graph-lane'); band.style.left = `${lane.x}px`; band.style.top = `${lane.y}px`; band.style.height = `${lane.height}px`;
        const label = element('span', lane.stage, 'graph-stage'); label.title = lane.stage; band.append(label); canvas.append(band);
      }
      const ns = 'http://www.w3.org/2000/svg';
      const svg = document.createElementNS(ns, 'svg'); svg.setAttribute('class', 'graph-edges');
      svg.setAttribute('width', String(layout.width)); svg.setAttribute('height', String(layout.height)); svg.setAttribute('aria-hidden', 'true');
      const defs = document.createElementNS(ns, 'defs');
      const marker = document.createElementNS(ns, 'marker');
      const markerId = `graph-arrow-${crypto.randomUUID()}`;
      for (const [key, value] of Object.entries({ id: markerId, viewBox: '0 0 10 10', refX: '9', refY: '5', markerWidth: '7', markerHeight: '7', orient: 'auto-start-reverse' })) marker.setAttribute(key, value);
      const arrow = document.createElementNS(ns, 'path'); arrow.setAttribute('d', 'M 0 0 L 10 5 L 0 10 z'); arrow.setAttribute('fill', '#8c9eb5');
      marker.append(arrow); defs.append(marker); svg.append(defs);
      for (const edge of layout.edges) {
        const x1 = edge.from.x + NODE_WIDTH; const y1 = edge.from.y + NODE_HEIGHT / 2;
        const x2 = edge.to.x - 4; const y2 = edge.to.y + NODE_HEIGHT / 2;
        const bend = COLUMN_GAP / 2;
        const path = document.createElementNS(ns, 'path');
        // A dependency that skips columns passes below the nodes, not through them.
        const skipsColumn = edge.to.x - edge.from.x > NODE_WIDTH + COLUMN_GAP;
        const bottom = layout.height - 8;
        path.setAttribute('d', skipsColumn
          ? `M ${x1} ${y1} L ${x1 + bend} ${y1} L ${x1 + bend} ${bottom} L ${x2 - bend} ${bottom} L ${x2 - bend} ${y2} L ${x2} ${y2}`
          : `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`);
        path.setAttribute('class', 'graph-edge'); path.setAttribute('marker-end', `url(#${markerId})`);
        svg.append(path); paths.push({ node: path, from: edge.from.job.id, to: edge.to.job.id });
      }
      canvas.append(svg);
    }
    const positions = new Map(layout?.nodes.map(node => [node.job.id, node]) ?? []);
    for (const job of jobs) {
      const node = action('', () => { stopFollowing(); select(job.id); }, 'graph-node');
      const position = positions.get(job.id);
      if (position) { node.style.left = `${position.x}px`; node.style.top = `${position.y}px`; }
      const badge = element('span', '', 'graph-status');
      const name = element('strong', job.name, 'graph-job-name'); name.title = job.name;
      const agent = element('span', '', 'graph-agent');
      node.append(badge, name, agent, element('span', `${job.stage || '단계 미지정'}${job.shared_workspace ? ' · 공용 workspace' : ''}`, 'graph-workspace'));
      canvas.append(node); buttons.set(job.id, { button: node, badge, agent });
    }
    paintScale();
  }

  function selectionInfo() {
    const job = selectedJob(); detail.hidden = !job;
    navigationState();
    if (!job) { infoSignature = ''; return; }
    const current = status(job);
    const signature = JSON.stringify(job);
    if (signature !== infoSignature) {
      infoSignature = signature;
      info.replaceChildren(element('h3', job.name), element('p', `${current.symbol} ${current.label} · 단계: ${job.stage || '미지정'} · Agent: ${job.agent_name ?? '배정 대기'}${job.exit_code === null ? '' : ` · 종료 코드 ${job.exit_code}`}`, 'caption'), element('p', `선행 작업: ${job.needs.join(', ') || '없음'}`, 'caption'));
      if (job.reason) info.append(element('pre', job.reason, 'graph-reason'));
      if (['queued', 'blocked'].includes(job.status)) {
        info.append(element('p', '선행 작업, 배포 승인, Agent 배정 상태를 확인하세요.', 'caption'));
        if (api.dispatchHref) { const link = element('a', '배정 점검 보기 →'); link.href = api.dispatchHref; info.append(link); }
      }
    }
    for (const [id, entry] of buttons) entry.button.setAttribute('aria-pressed', String(id === selectedId));
    for (const path of paths) path.node.classList.toggle('selected', path.from === selectedId || path.to === selectedId);
  }

  function select(id: string) {
    const job = jobs.find(job => job.id === id); if (!job) return;
    if (selectedId === id && selectedAttempt === job.attempt_id) { selectionInfo(); return; }
    const changed = selectedId !== id;
    selectedId = id; selectionVersion++; loadingLogs = false;
    selectedAttempt = selectedJob()?.attempt_id ?? null; cursor = -1; output.textContent = '';
    logTitle.textContent = selectedJob()?.shared_workspace ? '로그 · 공용 workspace 전체' : '작업 로그';
    output.hidden = more.hidden = !selectedAttempt; more.disabled = false;
    logNotice.textContent = selectedAttempt ? '' : '이 작업은 아직 실행 이력이 없어 로그가 없습니다.';
    selectionInfo();
    if (changed) api.onSelection?.(id);
    if (selectedAttempt) void loadLogs();
  }

  async function loadLogs() {
    if (!selectedAttempt || loadingLogs || disposed) return;
    const version = selectionVersion;
    loadingLogs = true; more.disabled = true; logNotice.textContent = '로그를 불러오는 중…';
    try {
      const entries = await api.logs(selectedAttempt, cursor);
      if (disposed || version !== selectionVersion) return;
      for (const entry of entries) { output.append(document.createTextNode(`[${entry.stream}] ${entry.content}`)); cursor = entry.sequence; }
      logNotice.textContent = entries.length ? '추가 로그는 아래 버튼으로 읽을 수 있습니다.' : '현재 저장된 로그를 모두 표시했습니다.';
    } catch (cause) {
      if (!disposed && version === selectionVersion && accessLost(cause)) return;
      if (!disposed && version === selectionVersion) logNotice.textContent = `로그 조회 실패: ${cause instanceof Error ? cause.message : cause}`;
    } finally {
      if (!disposed && version === selectionVersion) { loadingLogs = false; more.disabled = false; }
    }
  }

  function update(next: GraphJob[]) {
    jobs = next;
    const nextTopology = JSON.stringify(jobs.map(({ id, name, stage, needs }) => ({ id, name, stage, needs })));
    if (topology !== nextTopology) { topology = nextTopology; draw(); }
    const counts = new Map<string, number>();
    for (const job of jobs) {
      const current = status(job); counts.set(current.label, (counts.get(current.label) ?? 0) + 1);
      const entry = buttons.get(job.id)!;
      entry.button.dataset.status = Object.hasOwn(statuses, job.status) ? job.status : 'unknown';
      entry.badge.textContent = `${current.symbol} ${current.label}`;
      entry.agent.textContent = job.agent_name ?? 'Agent 배정 대기'; entry.agent.title = entry.agent.textContent;
      entry.button.setAttribute('aria-label', `${job.name} · ${job.stage || '단계 미지정'} · ${current.label} · 상세 및 로그 보기`);
    }
    summary.textContent = `${jobs.length}개 작업${counts.size ? ' · ' : ''}${[...counts].map(([name, count]) => `${name} ${count}`).join(' · ')}`;
    refreshed.textContent = `${new Date().toLocaleTimeString('ko-KR')} 갱신`;
    if (!jobs.length) {
      selectedId = undefined; selectedAttempt = null; selectionVersion++; loadingLogs = false;
      output.textContent = ''; cursor = -1; selectionInfo();
    } else if (!selectedJob()) {
      select((jobs.find(job => job.id === initialJobId) ?? jobs.find(job => job.status === 'running') ?? jobs.find(job => ['failed', 'lost'].includes(job.status)) ?? jobs[0]).id);
      initialJobId = undefined;
    }
    else if ((selectedJob()?.attempt_id ?? null) !== selectedAttempt) select(selectedId!);
    else selectionInfo();
    followRunning();
  }

  function schedule() {
    clearTimeout(timer);
    if (!disposed && auto.checked && active()) timer = setTimeout(() => {
      if (!panel.isConnected) { dispose(); return; }
      if (document.hidden) schedule(); else void refresh();
    }, 5000);
  }
  async function refresh() {
    if (refreshing || disposed) return;
    clearTimeout(timer); refreshing = true; refreshButton.disabled = true;
    try {
      const next = await api.jobs();
      if (disposed) return;
      update(next); error.hidden = true;
    } catch (cause) {
      if (!disposed && accessLost(cause)) return;
      if (!disposed) { error.textContent = `상태 갱신 실패 · 마지막으로 확인한 상태입니다. ${cause instanceof Error ? cause.message : cause}`; error.hidden = false; }
    } finally {
      refreshing = false; if (!disposed) { refreshButton.disabled = false; schedule(); }
    }
  }
  const resize = new ResizeObserver(() => {
    if (disposed || !panel.isConnected || !firstResize || !viewport.clientWidth) return;
    firstResize = false;
    revealSelected();
  });
  resize.observe(viewport);
  function dispose() { disposed = true; selectionVersion++; clearTimeout(timer); resize.disconnect(); }
  auto.addEventListener('change', () => { schedule(); followRunning(); });
  follow.addEventListener('change', () => {
    if (follow.checked) { followRunning(); revealSelected(); }
    else followNotice.textContent = '따라가기를 껐습니다. 선택한 작업을 유지합니다.';
  });
  for (const region of [viewport, detail]) {
    region.addEventListener('wheel', stopFollowing, {passive: true});
    region.addEventListener('pointerdown', stopFollowing);
    region.addEventListener('keydown', stopFollowing);
    region.addEventListener('focusin', stopFollowing);
  }
  update(initialJobs); schedule();
  return { element: panel, dispose };
}
