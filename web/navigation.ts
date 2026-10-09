export type Page = 'overview' | 'runs' | 'operations' | 'projects' | 'pipelines' | 'pipeline-graph' | 'pipeline' | 'run' | 'ci-settings' | 'environments' | 'settings' | 'agents' | 'connections';
export type Route = { page: Page; project?: string; pipeline?: string; run?: string; tab?: string; q?: string; state?: string; cursor?: string; agent?: string; os?: string; arch?: string; engine?: string; availability?: string; version?: string; job?: string; from?: string; to?: string; view?: string };
const pages: Page[] = ['overview', 'runs', 'operations', 'projects', 'pipelines', 'pipeline-graph', 'pipeline', 'run', 'ci-settings', 'environments', 'settings', 'agents', 'connections'];
const tabs: Partial<Record<Page, string[]>> = {
  pipeline: ['runs', 'connections', 'settings', 'review', 'trigger', 'versions'],
  run: ['graph', 'chain', 'dispatch', 'artifacts', 'info', 'deployment'],
  settings: ['sources', 'configuration', 'retention', 'members'],
};
export function parseRoute(hash: string): Route {
  const [path, query = ''] = hash.replace(/^#/, '').split('?');
  const page: Page = path === 'runners' ? 'agents' : pages.includes(path as Page) ? path as Page : 'overview';
  const params = new URLSearchParams(query);
  const route: Route = { page };
  if (['runs', 'pipelines', 'pipeline-graph', 'pipeline', 'run', 'ci-settings', 'environments', 'settings'].includes(page)) {
    route.project = params.get('project') || undefined;
  }
  if (page === 'pipeline' || page === 'run' || page === 'ci-settings' || page === 'pipeline-graph') route.pipeline = params.get('pipeline') || undefined;
  if (page === 'run') route.run = params.get('run') || undefined;
  if (tabs[page]) route.tab = tabs[page]!.includes(params.get('tab') ?? '') ? params.get('tab')! : tabs[page]![0];
  if (page === 'run' && route.tab === 'graph') {
    const job = params.get('job');
    if (job && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(job)) route.job = job.toLowerCase();
  }
  if (page === 'ci-settings') {
    route.q = params.get('q') || undefined;
    route.view = ['visual', 'toml', 'connections'].includes(params.get('view') ?? '') ? params.get('view')! : 'visual';
    const job = params.get('job');
    if (job && /^[a-zA-Z0-9_-]{1,80}$/.test(job)) route.job = job;
  }
  if (page === 'pipeline-graph') {
    route.q = params.get('q') || undefined;
    route.view = params.get('view') === 'configuration' ? 'configuration' : 'published';
    const version = params.get('version');
    if (route.view === 'published' && version && /^[1-9]\d*$/.test(version) && Number.isSafeInteger(Number(version))) route.version = version;
  }
  if (page === 'pipeline' && route.tab === 'connections' || page === 'ci-settings' && route.view === 'connections') {
    const version = params.get('version');
    if (version && /^[1-9]\d*$/.test(version) && Number.isSafeInteger(Number(version))) route.version = version;
  }
  if (page === 'pipeline' && route.tab === 'review') {
    const from=params.get('from'), to=params.get('to');
    if (from && /^(0|[1-9]\d*)$/.test(from) && Number.isSafeInteger(Number(from))) route.from=from;
    if (to && (to === 'draft' || /^[1-9]\d*$/.test(to) && Number.isSafeInteger(Number(to)))) route.to=to;
  }
  if (page === 'pipelines') {
    route.q = params.get('q') || undefined;
    const state = params.get('state');
    if (state && ['draft', 'published', 'running', 'failed', 'automatic'].includes(state)) route.state = state;
  }
  if (page === 'runs') {
    route.q = params.get('q') || undefined;
    const state = params.get('state');
    if (state && ['queued', 'running', 'succeeded', 'failed', 'canceled', 'awaiting_approval', 'recovery_required'].includes(state)) route.state = state;
    route.cursor = params.get('cursor') || undefined;
  }
  if (page === 'agents') {
    for (const key of ['q', 'agent'] as const) if (params.get(key)) route[key] = params.get(key)!;
    for (const [key, values] of [
      ['state', ['active', 'online', 'offline', 'draining', 'revoked', 'all']],
      ['os', ['linux', 'macos', 'windows']], ['arch', ['x86_64', 'aarch64']],
      ['engine', ['linux', 'windows']], ['availability', ['available', 'unavailable', 'unknown', 'stale']],
    ] as const) {
      const value = params.get(key); if (value && (values as readonly string[]).includes(value)) route[key] = value;
    }
  }
  return route;
}
export function routeHash(route: Route): string {
  const params = new URLSearchParams();
  for (const key of ['project', 'pipeline', 'run', 'tab', 'q', 'state', 'cursor', 'agent', 'os', 'arch', 'engine', 'availability', 'version', 'job', 'from', 'to', 'view'] as const) {
    if (route[key]) params.set(key, route[key]);
  }
  return `#${route.page}${params.size ? `?${params}` : ''}`;
}
export type PipelineItem = {
  name: string; current_version: number; latest_run_status?: string | null; trigger_enabled?: boolean;
};
export function matchesPipeline(item: PipelineItem, query: string, state: string): boolean {
  if (!item.name.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())) return false;
  if (state === 'draft') return item.current_version === 0;
  if (state === 'published') return item.current_version > 0;
  if (state === 'running') return ['queued', 'running'].includes(item.latest_run_status ?? '');
  if (state === 'failed') return ['failed', 'lost'].includes(item.latest_run_status ?? '');
  if (state === 'automatic') return item.trigger_enabled === true;
  return true;
}
