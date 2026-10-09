import type {Route} from './navigation.js';

type Candidate = {id: string; name: string; reasons: string[]; protocol_version: number | null; protocol_required: number; protocol_reported_at: string | null};
type Job = {id: string; name: string; status: string; runner: {os: string; arch: string; pool: string; docker?: string}; required_protocol: number; reason: string; pending_dependencies: {name: string; status: string}[]; candidates: {registered: number; active: number; precheck_passed: number; blockers: Record<string, number>; items: Candidate[]} | null};
type Report = {checked_at: string; run_status: string; cancel_requested: boolean; run_issues: string[]; max_parallel_jobs: number; running_jobs: number; shared_workspace: boolean; admin_details: boolean; jobs: Job[]};
const labels: Record<string, string> = {
  cancel_requested: '취소 처리 중', recovery_required: '배포 복구 확인 필요', awaiting_approval: '배포 승인 대기', executor_missing: '배포 환경의 허용 에이전트 미지정',
  environment_changed: '승인된 배포의 환경 잠금·에이전트가 변경됨', approval_invalid: '승인자의 현재 권한 확인 필요', deployment_not_ready: '배포 승인·환경 상태 확인 필요',
  dependencies: '선행 작업 완료 대기', scheduler_transition: '선행 작업 완료 후 상태 전환 대기', parallel_limit: '동시 실행 한도 대기', no_agents: '조건에 맞는 활성 에이전트 없음', agents_unavailable: '후보 에이전트 점검 필요', awaiting_claim: '에이전트 배정 요청 대기',
  revoked: '폐기됨', offline: '연결 확인 필요', draining: '새 작업 중지', busy: '다른 작업 수행 중', protocol_unknown: '지원 버전 보고 없음·지연', protocol_old: '에이전트 업데이트 필요',
  docker_stale: 'Docker 확인 없음·지연', docker_unavailable: 'Docker 사용 불가', docker_engine: 'Docker 엔진 불일치', read_credentials: 'Perforce 읽기 인증·연결 확인 필요', publish_credentials: 'Perforce 게시 인증·연결 확인 필요',
  queued: '배정 대기', blocked: '선행 작업 대기', assigned: '공용 workspace에 배정됨', running: '실행 중', finalizing: '마무리 중', succeeded: '성공', failed: '실패', canceled: '취소', lost: '연결 유실', skipped: '건너뜀',
};
const label = (code: string) => labels[code] ?? code;
function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; }
type Context = {runId: string; route: Route; api: <T>(path: string) => Promise<T>; link: (text: string, route: Route, className?: string) => HTMLAnchorElement; onState: (status: string, cancelRequested: boolean) => void};
export function createDispatchView(content: HTMLElement, context: Context): () => void {
  const panel = el('section', undefined, 'card dispatch-panel'); panel.append(el('h2', '배정 점검'), el('p', '이 실행에 고정된 조건과 최근 보고된 상태로 대기 사유를 확인합니다.', 'caption'));
  const controls = el('div', undefined, 'row workspace-refresh'); const refresh = el('button', '점검 새로고침', 'small'); refresh.type = 'button'; const autoLabel = el('label', '5초 자동 갱신', 'retention-toggle'); const auto = el('input'); auto.type = 'checkbox'; auto.checked = true; autoLabel.prepend(auto); const status = el('span', '불러오는 중…', 'caption'); status.setAttribute('role', 'status'); controls.append(refresh, autoLabel, status); panel.append(controls);
  const body = el('div'); panel.append(body); content.append(panel);
  let disposed = false; let loading = false; let checked: string | undefined; let ongoing = true;
  const render = (data: Report) => {
    body.replaceChildren(); ongoing = ['queued', 'running'].includes(data.run_status); context.onState(data.run_status, data.cancel_requested);
    body.append(el('p', `실행 ${label(data.run_status)} · 실행 중 작업 ${data.running_jobs} / 동시 실행 한도 ${data.max_parallel_jobs}${data.shared_workspace ? ' · 공용 workspace' : ''}`, 'caption'));
    if (!ongoing) body.append(el('p', '종료된 실행입니다. 현재 에이전트 상태로 과거의 대기 원인을 추정하지 않습니다.', 'caption'));
    if (data.run_issues.length) {
      const issues = el('div', undefined, 'dispatch-issues'); for (const issue of data.run_issues) issues.append(el('p', label(issue)));
      if (data.run_issues.some(code => ['awaiting_approval', 'recovery_required', 'executor_missing', 'environment_changed', 'approval_invalid', 'deployment_not_ready'].includes(code))) issues.append(context.link('배포 승인·결과 확인 →', {...context.route, tab: 'deployment'})); body.append(issues);
    }
    if (!data.jobs.length) body.append(el('p', '표시할 작업이 없습니다.', 'empty'));
    for (const job of data.jobs) {
      const card = el('article', undefined, 'dispatch-job'); const heading = el('div', undefined, 'row space'); heading.append(el('h3', job.name), el('span', label(job.reason === 'not_waiting' ? job.status : job.reason), `badge ${job.reason === 'awaiting_claim' || job.status === 'succeeded' ? 'succeeded' : 'queued'}`)); card.append(heading);
      card.append(el('p', `${job.runner.os} / ${job.runner.arch} · Pool ${job.runner.pool} · Docker ${job.runner.docker ? `${job.runner.docker} 엔진 필요` : '조건 없음'}`, 'caption'));
      if (job.pending_dependencies.length && ['queued','blocked','assigned'].includes(job.status)) card.append(el('p', `기다리는 선행 작업: ${job.pending_dependencies.map(item => `${item.name} (${label(item.status)})`).join(', ')}`, 'caption'));
      const c = job.candidates;
      if (c) {
        card.append(el('p', `조건 일치 등록 ${c.registered}대 · 활성 ${c.active}대 · 에이전트 사전 점검 통과 ${c.precheck_passed}대`, 'caption'));
        if (Object.keys(c.blockers).length) card.append(el('p', Object.entries(c.blockers).map(([code, count]) => `${label(code)} ${count}대`).join(' · '), 'caption'));
        if (c.precheck_passed) card.append(el('p', '에이전트 사전 점검을 통과해도 실행의 승인·선행 작업·동시 실행 조건을 충족해야 배정됩니다.', 'caption'));
        if (data.admin_details) {
          if (!c.items.length) card.append(context.link('에이전트 목록 확인 →', {page: 'agents', state: 'all', os: job.runner.os, arch: job.runner.arch, q: job.runner.pool}));
          for (const candidate of c.items) {
            const row = el('div', undefined, 'dispatch-candidate'); row.append(context.link(candidate.name, {page: 'agents', state: 'all', agent: candidate.id}), el('span', candidate.reasons.length ? candidate.reasons.map(label).join(' · ') : '사전 점검 통과', 'caption'));
            if (candidate.reasons.some(code => code === 'protocol_old' || code === 'protocol_unknown')) row.append(el('span', `보고된 프로토콜 ${candidate.protocol_version ?? '없음'} / 필요 ${candidate.protocol_required}${candidate.protocol_reported_at ? ` · ${new Date(candidate.protocol_reported_at).toLocaleString('ko-KR')}` : ''}`, 'caption'));
            card.append(row);
          }
          if (c.registered > c.items.length) card.append(el('p', `후보 ${c.registered}대 중 ${c.items.length}대를 표시합니다. 요약은 전체 후보 기준입니다.`, 'caption'));
        } else card.append(el('p', '에이전트별 상세와 변경은 서버 관리자에게 문의하세요.', 'caption'));
      }
      body.append(card);
    }
    body.append(el('p', '현재 시점의 조회 결과이며 작업을 예약하거나 실행하지 않습니다. 실제 배정은 에이전트 요청 시 권한·잠금·버전·인증·동시 작업 상태를 다시 검사합니다.', 'caption'));
  };
  const fetchReport = async () => {
    if (disposed || loading) return;
    loading = true; refresh.disabled = true;
    try {
      const data = await context.api<Report>(`/api/runs/${encodeURIComponent(context.runId)}/dispatch`);
      if (disposed || !content.isConnected) return;
      render(data); checked = data.checked_at; status.className = 'caption'; status.textContent = `${new Date(checked).toLocaleString('ko-KR')} 기준${ongoing ? '' : ' · 자동 갱신 종료'}`;
    } catch (error) {
      if (!disposed && content.isConnected) { status.className = 'graph-error'; status.textContent = `점검 실패${checked ? ` · ${new Date(checked).toLocaleString('ko-KR')}의 마지막 결과입니다` : ''}: ${error instanceof Error ? error.message : String(error)}`; }
    } finally {loading = false; refresh.disabled = false;}
  };
  const visible = () => {if (!document.hidden && auto.checked && ongoing) void fetchReport();};
  refresh.addEventListener('click', () => {void fetchReport();}); auto.addEventListener('change', visible); document.addEventListener('visibilitychange', visible);
  const timer = window.setInterval(visible, 5000); void fetchReport();
  return () => {disposed = true; clearInterval(timer); document.removeEventListener('visibilitychange', visible);};
}
