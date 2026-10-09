export type AgentStatus = {
  active: boolean;
  draining?: boolean;
  last_seen_at: string | null;
  docker_state: string;
  docker_engine_os: string | null;
  docker_checked_at: string | null;
};

export function connectionStatus(agent: Pick<AgentStatus, 'active' | 'last_seen_at'>, now = Date.now()) {
  const age = agent.last_seen_at ? now - Date.parse(agent.last_seen_at) : NaN;
  const online = agent.active && Number.isFinite(age) && age >= -30000 && age < 30000;
  return { online, label: !agent.active ? '폐기됨' : online ? '온라인' : '오프라인' };
}

export function dockerStatus(agent: AgentStatus, now = Date.now()) {
  const engine = agent.docker_engine_os === 'linux' ? 'Linux 엔진' : agent.docker_engine_os === 'windows' ? 'Windows 엔진' : '';
  const known: Record<string, [string, string]> = {
    available: ['사용 가능', engine],
    cli_missing: ['사용 불가', 'CLI 없음'],
    engine_unavailable: ['사용 불가', '엔진 연결 또는 권한 확인 필요'],
    check_failed: ['확인 실패', '점검 시간 초과 또는 실행 오류'],
  };
  const state = known[agent.docker_state];
  if (!state || !agent.docker_checked_at || (agent.docker_state === 'available' && !engine)) return { availability: 'unknown', label: '확인 전', detail: '상태 보고 대기', tone: 'canceled' };
  const age = now - Date.parse(agent.docker_checked_at);
  const stale = !Number.isFinite(age) || age < -30000 || age > 150000 || !connectionStatus(agent, now).online;
  if (stale) return { availability: 'stale', label: '확인 지연', detail: `마지막: ${state[0]}${state[1] ? ` · ${state[1]}` : ''}`, tone: 'queued' };
  return { availability: agent.docker_state === 'available' ? 'available' : 'unavailable', label: state[0], detail: state[1], tone: agent.docker_state === 'available' ? 'succeeded' : 'queued' };
}

export type AgentFilters = { search: string; os: string; arch: string; engine: string; availability: string; state?: string };
export function matchesAgent(agent: AgentStatus & { name: string; os: string; arch: string; pool: string }, filters: AgentFilters, now = Date.now()) {
  const online = connectionStatus(agent, now).online;
  const stateMatches = filters.state === 'all' ? true
    : filters.state === 'revoked' ? !agent.active
    : filters.state === 'online' ? online
    : filters.state === 'offline' ? agent.active && !online
    : filters.state === 'draining' ? agent.active && agent.draining === true
    : agent.active;
  return stateMatches && (!filters.os || agent.os === filters.os)
    && (!filters.arch || agent.arch === filters.arch)
    && (!filters.engine || agent.docker_engine_os === filters.engine)
    && (!filters.availability || dockerStatus(agent, now).availability === filters.availability)
    && `${agent.name} ${agent.os} ${agent.arch} ${agent.pool}`.toLowerCase().includes(filters.search.trim().toLowerCase());
}
