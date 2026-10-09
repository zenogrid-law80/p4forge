import { layoutJobs, NODE_WIDTH, NODE_HEIGHT, graphScale, fitGraphScale, type GraphJob } from './run-graph.js';
import {createEditorDiagnostics, diagnosticFields, type EditorIssue, type DiagnosticReport} from './editor-diagnostics.js';

type Runner = {os: string; arch: string; pool: string; docker?: string};
export type EditorJob = {name: string; stage: string; needs: string[]; runner: Runner; shell: string; script: string[]; working_directory: string; timeout_seconds: number; environment: Record<string, string>};
export type EditorDefinition = {
  schema_version: number; source_root_id: string; max_parallel_jobs: number; workspace?: string; stages?: string[];
  jobs: EditorJob[]; deployment?: {environment_id: string; health_check: string};
  [key: string]: unknown;
};
type Validation = {graph: {order: string[]; edges: string[][]}; definition_hash: string};
type Api = <T>(path: string, method?: string, body?: unknown) => Promise<T>;
type Context = {
  id: string; name: string; version: number; draft: {generation: number; base_version: number};
  toml: string; savedToml: string | null; writable: boolean; api: Api;
  roots: {id: string; name: string}[]; review: () => Promise<void>; message: (text: string, error?: boolean) => void;
  definition?: EditorDefinition | null; inspect?: boolean; initialView?: string; initialJob?: string; template?: string;
  onView?: (view: 'visual' | 'toml', job?: string) => void;
};
export type EditorHandle = {canLeave: () => boolean; dispose: () => void};
const validName = (name: string) => /^[a-zA-Z0-9_-]{1,80}$/.test(name);

export function renameJob(definition: EditorDefinition, index: number, name: string): void {
  if (!validName(name)) throw new Error('작업 이름은 영문·숫자·_·-로 1~80자 입력하세요.');
  if (definition.jobs.some((job, i) => i !== index && job.name === name)) throw new Error('이미 있는 작업 이름입니다.');
  const old = definition.jobs[index].name;
  definition.jobs[index].name = name;
  for (const job of definition.jobs) job.needs = job.needs.map(need => need === old ? name : need);
  if (definition.deployment?.health_check === old) definition.deployment.health_check = name;
}
export function removeJob(definition: EditorDefinition, index: number): void {
  const name = definition.jobs[index].name;
  if (definition.jobs.length === 1) throw new Error('작업이 한 개 이상 필요합니다.');
  if (definition.jobs.some((job, i) => i !== index && job.needs.includes(name)) || definition.deployment?.health_check === name) {
    throw new Error('다른 작업 또는 배포 상태 확인에서 참조 중입니다. 참조를 먼저 변경하세요.');
  }
  definition.jobs.splice(index, 1);
}
export function editorGraph(definition: EditorDefinition): GraphJob[] {
  const stages = definition.stages ?? [];
  const shared = definition.workspace === 'pipeline';
  if (!definition.jobs.length || definition.jobs.length > 64) throw new Error('작업은 1~64개가 필요합니다.');
  if (shared) {
    if (definition.max_parallel_jobs !== 1) throw new Error('공유 workspace의 동시 작업 수는 1이어야 합니다.');
    if (!stages.length || stages.length > 64 || stages.some(stage => !validName(stage)) || new Set(stages).size !== stages.length) throw new Error('공유 workspace에는 중복 없는 단계 이름이 1~64개 필요합니다.');
    const first = definition.jobs[0].runner;
    if (definition.jobs.some(job => job.runner.os !== first.os || job.runner.arch !== first.arch || job.runner.pool !== first.pool || job.runner.docker !== first.docker)) throw new Error('공유 workspace의 실행 조건은 모든 작업에서 같아야 합니다.');
  } else if (stages.length) throw new Error('작업별 workspace에서는 단계 순서 목록을 비워 주세요. 작업의 단계 이름은 표시용으로 유지됩니다.');
  return definition.jobs.map((job, i) => {
    if (!validName(job.name)) throw new Error('작업 이름은 영문·숫자·_·-로 1~80자 입력하세요.');
    if (new Set(job.needs).size !== job.needs.length) throw new Error(`${job.name}: 중복 의존 작업이 있습니다.`);
    if (job.needs.includes(job.name)) throw new Error(`${job.name}: 자기 자신을 의존 작업으로 지정할 수 없습니다.`);
    if (shared && !stages.includes(job.stage)) throw new Error(`${job.name}: 단계 순서 목록에 있는 단계를 선택하세요.`);
    const needs = [...job.needs];
    if (shared) for (const earlier of definition.jobs) {
      if (stages.indexOf(earlier.stage) < stages.indexOf(job.stage) && !needs.includes(earlier.name)) needs.push(earlier.name);
    }
    return {id: String(i), name: job.name, stage: job.stage, needs, status: 'draft', shared_workspace: shared, attempt_id: null, agent_name: null, reason: null, exit_code: null, cancel_requested: null};
  });
}
function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className = '') {
  const node = document.createElement(tag); if (text !== undefined) node.textContent = text; node.className = className; return node;
}
function download(text: string, name: string) {
  const url = URL.createObjectURL(new Blob([text], {type: 'text/plain;charset=utf-8'}));
  const a = el('a'); a.href = url; a.download = name; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function createPipelineEditor(parent: HTMLElement, context: Context): EditorHandle {
  const path = `/api/pipelines/${context.id}`;
  let model: EditorDefinition | undefined; let baseline = ''; let selected = 0;
  let savedDefinition = context.definition ? structuredClone(context.definition) : undefined;
  let editing = context.writable && !context.inspect;
  let rememberedJob = context.initialJob;
  let scale = 1; let graphSize = {width: 0, height: 0};
  if (context.initialView !== 'toml' && context.inspect && savedDefinition) {
    model = structuredClone(savedDefinition); baseline = JSON.stringify(model);
    selected = Math.max(0, model.jobs.findIndex(job => job.name === rememberedJob));
  }
  let savedText = context.savedToml; let originalText = context.toml;
  let draft = {generation: context.draft.generation, base_version: context.draft.base_version};
  let busy = false; let disposed = false; let importSequence = 0; let pendingName = false;
  const card = el('section', undefined, 'card pipeline-editor');
  const title = el('h2', '파이프라인 초안'); const badge = el('span', '', 'badge');
  const heading = el('div', undefined, 'row space'); heading.append(title, badge);
  const state = el('p', '', 'caption'); state.setAttribute('aria-live', 'polite');
  const status = el('p', '', 'editor-status'); status.setAttribute('role', 'status');
  const fields = el('fieldset', undefined, 'editor-fields'); const mode = el('div', undefined, 'row');
  const text = el('textarea'); text.setAttribute('aria-label', '파이프라인 TOML'); text.spellcheck = false; text.value = context.toml; text.readOnly = !editing;
  const visual = el('div', undefined, 'editor-visual'); visual.hidden = true;
  const graph = el('div', undefined, 'editor-graph'); const graphError = el('p', '', 'graph-error'); graphError.setAttribute('role', 'status');
  const form = el('div', undefined, 'editor-properties'); form.setAttribute('aria-label', '선택 작업 속성');
  const validation = el('pre'); validation.hidden = true;
  const actions = el('div', undefined, 'row editor-actions');
  fields.append(mode, text, visual, actions); card.append(heading, state, fields, status, validation); parent.append(card);
  const dirty = () => editing && (pendingName || text.value !== (savedText ?? '') || (model !== undefined && JSON.stringify(model) !== baseline));
  const active = () => !disposed && card.isConnected;
  const updateState = () => {
    badge.textContent = `발행 v${context.version} · 초안 g${draft.generation}`;
    state.textContent = `기준 v${draft.base_version} · ${dirty() ? '저장하지 않은 변경 있음' : savedText === null ? '저장된 초안 없음' : '저장된 초안'} · ${editing ? '편집 중' : '조회 중'} · 검사: ${diagnostics.label()} · 실행에는 발행 버전이 고정됩니다.`;
  };
  const diagnostics = createEditorDiagnostics(fields,{
    id:context.id,api:context.api,
    snapshot:()=>({key:JSON.stringify({editing,draft,model,text:text.value,pendingName}),generation:draft.generation,base_version:draft.base_version,body:editing?{...draft,...(model?{definition:model}:{toml:text.value})}:undefined}),
    canCheck:()=>{if(pendingName){context.message('입력한 작업 이름을 먼저 적용하거나 원래 이름으로 되돌려 주세요.',true);return false;}return active();},
    jump:jumpToIssue,changed:()=>{updateState();if(model)drawGraph();}
  });
  const changed = () => {validation.hidden = true; status.textContent = ''; diagnostics.invalidate();};
  text.addEventListener('input', changed);
  const action = (label: string, run: () => void | Promise<void>, className = '') => {
    const button = el('button', label, className); button.type = 'button';
    button.addEventListener('click', async () => {
      if (busy) return;
      if (pendingName && !['이름 변경', '편집 취소', '변환 전 TOML로 되돌리기', '변환 전 TOML 내려받기'].includes(label)) {context.message('입력한 작업 이름을 먼저 적용하거나 원래 이름으로 되돌려 주세요.', true); return;}
      busy = true; fields.disabled = true; status.textContent = '처리 중…'; status.className = 'editor-status';
      try {await run(); if (active() && status.textContent === '처리 중…') status.textContent = '';}
      catch (error) {if (active()) {status.textContent = error instanceof Error ? error.message : String(error); status.className = 'editor-status graph-error';if((error as {status?:number}).status===400)await diagnostics.check();}}
      finally {
        busy = false; fields.disabled = false;
        if (active()) {
          updateState();
          if (label === '편집' && editing) (model ? form.querySelector<HTMLInputElement>('[aria-label="새 작업 이름"]') : text)?.focus({preventScroll:true});
          if (label === '편집 취소' && !editing) edit.focus({preventScroll:true});
        }
      }
    }); return button;
  };
  const showValidation = (result: Validation) => {
    validation.textContent = `검증 통과 · 순서: ${result.graph.order.join(' → ')}\nSHA-256: ${result.definition_hash}`;
    validation.hidden = false;
  };
  const preview = async () => {
    const result = await context.api<Validation & {toml: string}>(`${path}/definition-preview`, 'POST', {definition: model});
    if (active()) showValidation(result); return result;
  };
  const enter = action('폼으로 편집', async () => {
    const result = !editing && savedDefinition ? {definition: savedDefinition} : await diagnostics.check();
    if (!active()) return;
    if(!result?.definition || 'graph' in result && !result.graph) {status.textContent='정의 오류를 TOML에서 먼저 수정해 주세요. 입력은 유지됩니다.';return;}
    originalText = text.value; model = structuredClone(result.definition); baseline = JSON.stringify(model);
    selected = Math.max(0, model.jobs.findIndex(job => job.name === rememberedJob));
    renderMode(); renderForm(); diagnostics.invalidate();
  });
  const convert = action('TOML로 변환', async () => {
    rememberedJob = model?.jobs[selected]?.name;
    if (editing) {const result = await diagnostics.check(); if (!active()||!result?.toml) return; text.value = result.toml;}
    model = undefined; renderMode(); diagnostics.invalidate();
    if (editing) status.textContent = '폼 내용을 TOML로 변환했습니다. 오류가 있는 설정도 수정할 수 있으며 저장 시 다시 검증합니다.';
  });
  const original = action('변환 전 TOML 내려받기', () => download(originalText, `${context.name}-original.toml`));
  const discard = action('변환 전 TOML로 되돌리기', () => {
    if (!window.confirm('폼 편집 내용을 버리고 변환 전 TOML로 돌아갈까요?')) return;
    text.value = originalText; model = undefined; pendingName = false; validation.hidden = true; renderMode(); diagnostics.invalidate();
  });
  mode.append(enter, convert);
  if (context.writable) mode.append(original, discard);
  const edit = action('편집', () => {editing = true; if (!savedDefinition && !text.value) text.value = context.template ?? ''; renderMode(); if (model) renderForm();diagnostics.invalidate();});
  const cancel = action('편집 취소', () => {
    if (dirty() && !window.confirm('저장하지 않은 변경을 버리고 저장된 초안으로 돌아갈까요?')) return;
    rememberedJob = model?.jobs[selected]?.name ?? rememberedJob;
    const wasVisual = !!model;
    text.value = savedText ?? ''; originalText = text.value; editing = false; pendingName = false;
    model = wasVisual && savedDefinition ? structuredClone(savedDefinition) : undefined;
    if (model) {const index = model.jobs.findIndex(job => job.name === rememberedJob); selected = index < 0 ? Math.min(selected, model.jobs.length - 1) : index; baseline = JSON.stringify(model);}
    validation.hidden = true; renderMode(); if (model) renderForm();diagnostics.clear();
  });
  if (context.inspect && context.writable) mode.append(edit, cancel);
  const note = el('p', '폼 변환·저장은 주석과 TOML 서식을 다시 작성합니다. 변환 전 원문을 내려받을 수 있습니다. 산출물·후속 고정 버전·배포 설정·환경 변수는 보존되며 TOML에서 편집합니다.', 'caption');
  const tools = el('div', undefined, 'row editor-graph-tools');
  const tool = (label: string, run: () => void) => {const b = el('button', label, 'small'); b.type = 'button'; b.addEventListener('click', run); return b;};
  const zoomLabel = el('span', '배율 100%', 'caption');
  const locate = () => {
    form.hidden = false; properties.textContent = '속성 접기'; properties.setAttribute('aria-expanded', 'true');
    const viewport = graph.querySelector<HTMLElement>('.graph-viewport'); const node = graph.querySelector<HTMLElement>('[aria-pressed="true"]');
    if (!viewport || !node) return;
    const b = node.getBoundingClientRect(), v = viewport.getBoundingClientRect();
    viewport.scrollLeft += b.left - v.left + b.width / 2 - viewport.clientWidth / 2;
    viewport.scrollTop += b.top - v.top + b.height / 2 - viewport.clientHeight / 2;
    node.focus({preventScroll:true});
  };
  function setScale(value: number) {
    const viewport = graph.querySelector<HTMLElement>('.graph-viewport'); if (!viewport) return;
    const x = (viewport.scrollLeft + viewport.clientWidth / 2) / scale, y = (viewport.scrollTop + viewport.clientHeight / 2) / scale;
    scale = graphScale(value); applyScale();
    viewport.scrollLeft = x * scale - viewport.clientWidth / 2; viewport.scrollTop = y * scale - viewport.clientHeight / 2;
  }
  function applyScale() {
    const surface = graph.querySelector<HTMLElement>('.graph-surface'), canvas = graph.querySelector<HTMLElement>('.graph-canvas');
    if (surface && canvas) {surface.style.width = `${graphSize.width * scale}px`; surface.style.height = `${graphSize.height * scale}px`; canvas.style.transform = `scale(${scale})`;}
    zoomLabel.textContent = `배율 ${Math.round(scale * 100)}%`;
  }
  const properties = tool('속성 접기', () => {form.hidden = !form.hidden; properties.textContent = form.hidden ? '속성 펼치기' : '속성 접기'; properties.setAttribute('aria-expanded', String(!form.hidden));});
  properties.setAttribute('aria-expanded', 'true');
  tools.append(tool('축소', () => setScale(scale / 1.25)), tool('100%', () => setScale(1)), tool('확대', () => setScale(scale * 1.25)), tool('화면 맞춤', () => {
    const viewport = graph.querySelector<HTMLElement>('.graph-viewport'); if (!viewport) return;
    setScale(fitGraphScale(graphSize.width, graphSize.height, viewport.clientWidth, viewport.clientHeight)); viewport.scrollLeft = viewport.scrollTop = 0;
  }), zoomLabel, tool('선택 작업으로 이동', locate), properties);
  const workspace = el('div', undefined, 'editor-workspace'); workspace.append(graph, form);
  visual.append(note, graphError, tools, workspace);
  function notifyView() {
    if (model) rememberedJob = model.jobs[selected]?.name;
    context.onView?.(model ? 'visual' : 'toml', rememberedJob);
  }
  function renderMode() {
    text.hidden = !!model; visual.hidden = !model; enter.hidden = !!model; convert.hidden = !model; discard.hidden = !model;
    text.readOnly = !editing; file.hidden = !!model || !editing;
    enter.textContent = editing ? '폼으로 편집' : '시각화 보기'; enter.hidden ||= !editing && !savedDefinition;
    convert.textContent = editing ? 'TOML로 변환' : 'TOML 보기';
    original.hidden = !editing || originalText === text.value && !model; discard.hidden ||= !editing;
    edit.hidden = editing; cancel.hidden = !editing;
    note.hidden = !editing;
    for (const button of Array.from(actions.querySelectorAll<HTMLElement>('.editor-write-action'))) button.hidden = !editing;
    notifyView();
  }
  function jumpToIssue(issue:EditorIssue,report:DiagnosticReport) {
    if(busy||pendingName){context.message('처리 중이거나 미적용 작업 이름이 있습니다. 먼저 완료해 주세요.',true);return;}
    if(!model && report.definition && report.definition.jobs.length>0 && report.definition.jobs.length<=64 && issue.field && diagnosticFields[issue.field] && issue.field!=='toml') {
      originalText=text.value;model=structuredClone(report.definition);baseline=JSON.stringify(model);selected=Math.min(selected,model.jobs.length-1);renderMode();
      // Only the editing representation changed; another inspection will use this form.
      diagnostics.invalidate();
    }
    if(model) {
      if(issue.job_index!==undefined&&issue.job_index!==null&&model.jobs[issue.job_index])selected=issue.job_index;
      form.hidden=false;properties.textContent='속성 접기';properties.setAttribute('aria-expanded','true');renderForm();
      const label=diagnosticFields[issue.field??''];
      const target=issue.field==='needs'?form.querySelector<HTMLElement>('.editor-needs'):issue.field==='jobs'?form.querySelector<HTMLElement>('.editor-toolbar select'):label?Array.from(form.querySelectorAll<HTMLElement>('[aria-label]')).find(n=>n.getAttribute('aria-label')===label):undefined;
      if(target){const details=target.closest('details');if(details)details.open=true;target.tabIndex=target.tabIndex<0?-1:target.tabIndex;target.focus();target.scrollIntoView({block:'center'});return;}
      // Advanced fields remain editable in TOML even when current validation fails.
      text.value=editing?(report.toml??text.value):(savedText??text.value);model=undefined;renderMode();diagnostics.invalidate();
    }
    text.focus();
    const offset=issue.offset??0;text.setSelectionRange(offset,Math.min(text.value.length,offset+1));text.scrollIntoView({block:'center'});
  }
  function drawGraph() {
    if (!model) return;
    const oldViewport = graph.querySelector('.graph-viewport'); const scroll = {x: oldViewport?.scrollLeft ?? 0, y: oldViewport?.scrollTop ?? 0};
    graph.replaceChildren(); graphError.textContent = '';
    try {
      const data = layoutJobs(editorGraph(model));
      graphSize = {width:data.width, height:data.height};
      const viewport = el('div', undefined, 'graph-viewport'); viewport.tabIndex = 0; viewport.setAttribute('aria-label', '초안 의존 관계 그래프');
      const canvas = el('div', undefined, 'graph-canvas'); canvas.style.width = `${data.width}px`; canvas.style.height = `${data.height}px`;
      const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); svg.classList.add('graph-edges'); svg.setAttribute('width', String(data.width)); svg.setAttribute('height', String(data.height)); svg.setAttribute('aria-hidden', 'true');
      for (const edge of data.edges) {
        const line = document.createElementNS(svg.namespaceURI, 'path'); const x = edge.from.x + NODE_WIDTH; const y = edge.from.y + NODE_HEIGHT / 2;
        const targetX = edge.to.x; const targetY = edge.to.y + NODE_HEIGHT/2;
        // Route long edges above intervening nodes so stage barriers stay visible.
        const top = Math.min(edge.from.y, edge.to.y) - 18;
        line.setAttribute('d', targetX - x > 100
          ? `M${x},${y} C${x+30},${y} ${x+30},${top} ${x+45},${top} L${targetX-45},${top} C${targetX-30},${top} ${targetX-30},${targetY} ${targetX},${targetY}`
          : `M${x},${y} C${x+38},${y} ${targetX-38},${targetY} ${targetX},${targetY}`);
        line.classList.add('graph-edge'); if (!model.jobs[Number(edge.to.job.id)].needs.includes(edge.from.job.name)) line.classList.add('editor-stage-edge'); svg.append(line);
      }
      canvas.append(svg);
      for (const node of data.nodes) {
        const index = Number(node.job.id); const job = model.jobs[index];
        const button = el('button', undefined, 'graph-node'); button.type = 'button'; button.style.left = `${node.x}px`; button.style.top = `${node.y}px`;
        button.setAttribute('aria-pressed', String(index === selected)); button.setAttribute('aria-label', `${job.name} 작업 ${editing ? '편집' : '보기'}`);
        const errors=diagnostics.currentIssues().filter(issue=>issue.level==='error'&&issue.job_index===index);
        button.classList.toggle('editor-node-error',!!errors.length);
        if(errors.length)button.append(el('span',`오류 ${errors.length}개`,'graph-reason'));
        button.append(el('strong', job.name, 'graph-job-name'), el('span', job.stage || '단계 없음', 'caption'), el('span', `${job.runner.os} · ${job.runner.arch}`, 'caption'), el('span', `Docker ${job.runner.docker ?? '조건 없음'} · 선행 ${node.job.needs.length}개`, 'caption'));
        button.addEventListener('click', () => {if (busy || pendingName) {context.message('입력한 작업 이름을 적용하거나 처리가 끝난 뒤 선택하세요.', true); return;} selected = index; form.hidden = false; properties.textContent = '속성 접기'; properties.setAttribute('aria-expanded','true'); renderForm(); form.querySelector<HTMLInputElement>('[aria-label="새 작업 이름"]')?.focus({preventScroll:true});}); canvas.append(button);
      }
      const surface = el('div', undefined, 'graph-surface scaled'); surface.append(canvas); viewport.append(surface);
      graph.append(viewport, el('p', model.workspace === 'pipeline' ? '왼쪽 → 오른쪽 실행 흐름 · 점선은 공유 workspace의 앞 단계 완료 대기입니다.' : '왼쪽 → 오른쪽 실행 흐름 · 작업별 workspace에서 단계 이름은 표시용입니다.', 'caption'));
      applyScale(); viewport.scrollLeft = scroll.x; viewport.scrollTop = scroll.y;
    } catch (error) {graphError.textContent = error instanceof Error ? error.message : String(error);}
  }
  function input(parent: HTMLElement, label: string, value: string, apply: (value: string) => void, multiline = false) {
    const wrap = el('label', label); const field = multiline ? el('textarea') : el('input'); field.value = value; field.setAttribute('aria-label', label);
    field.addEventListener('input', () => {apply(field.value); changed();}); wrap.append(field); parent.append(wrap); return field;
  }
  function select(parent: HTMLElement, label: string, value: string, values: [string, string][], apply: (value: string) => void) {
    const wrap = el('label', label); const field = el('select'); field.setAttribute('aria-label', label);
    for (const [id, name] of values) {const option = el('option', name); option.value = id; field.append(option);}
    field.value = value; field.addEventListener('change', () => {apply(field.value); changed();}); wrap.append(field); parent.append(wrap); return field;
  }
  function renderForm() {
    if (!model) return;
    form.replaceChildren(); drawGraph();
    const definition = model; const general = el('details', undefined, 'editor-general'); general.append(el('summary', '소스·workspace·단계 순서'));
    const settings = el('div', undefined, 'two');
    select(settings, '소스 루트', definition.source_root_id, context.roots.map(root => [root.id, root.name]), value => definition.source_root_id = value);
    select(settings, 'Workspace', definition.workspace ?? 'job', [['job', '작업별 workspace'], ['pipeline', '공유 workspace']], value => definition.workspace = value);
    const parallel = input(settings, '동시 작업 수', String(definition.max_parallel_jobs), value => definition.max_parallel_jobs = Number(value)); parallel.setAttribute('type', 'number'); parallel.setAttribute('min', '1'); parallel.setAttribute('max', '32');
    input(settings, '단계 순서 (한 줄에 하나 · 공유 workspace만)', (definition.stages ?? []).join('\n'), value => definition.stages = value ? value.split('\n') : [], true);
    general.append(settings, el('p', '공유 workspace는 동시 작업 수 1, 동일한 실행 조건과 단계 순서가 필요합니다. 모드를 바꿔도 기존 값을 자동 삭제하지 않습니다.', 'caption')); form.append(general);
    const toolbar = el('div', undefined, 'row editor-toolbar');
    const jobSelect = select(toolbar, editing ? '편집할 작업' : '선택 작업', String(selected), definition.jobs.map((job, i) => [String(i), job.name]), value => {if (pendingName) {jobSelect.value = String(selected); context.message('입력한 작업 이름을 먼저 적용하거나 원래 이름으로 되돌려 주세요.', true); return;} selected = Number(value); renderForm();});
    const uniqueName = () => {let i = 1; while (definition.jobs.some(job => job.name === `job-${i}`)) i++; return `job-${i}`;};
    toolbar.append(action('작업 추가', () => {
      if (definition.jobs.length >= 64) throw new Error('작업은 64개까지 추가할 수 있습니다.');
      const current = definition.jobs[selected];
      definition.jobs.push({name: uniqueName(), stage: current.stage, needs: [], runner: structuredClone(current.runner), shell: current.shell, script: ['echo hello'], working_directory: '.', timeout_seconds: 3600, environment: {}});
      selected = definition.jobs.length - 1; changed(); renderForm();
    }));
    form.append(toolbar);
    const job = definition.jobs[selected]; const editor = el('div', undefined, 'editor-job'); editor.append(el('h3', `${job.name} 작업`));
    const nameRow = el('div', undefined, 'row'); const name = el('input'); name.value = job.name; name.setAttribute('aria-label', '새 작업 이름');
    name.addEventListener('input', () => {pendingName = name.value !== job.name; changed();});
    const nameLabel = el('label', '작업 이름'); nameLabel.append(name); nameRow.append(nameLabel, action('이름 변경', () => {renameJob(definition, selected, name.value); pendingName = false; changed(); renderForm();})); editor.append(nameRow);
    const fields = el('div', undefined, 'two');
    input(fields, '단계 이름', job.stage, value => job.stage = value);
    input(fields, '작업 디렉터리', job.working_directory, value => job.working_directory = value);
    select(fields, '운영체제', job.runner.os, [['linux', 'Linux'], ['windows', 'Windows'], ['macos', 'macOS']], value => job.runner.os = value);
    select(fields, '아키텍처', job.runner.arch, [['x86_64', 'x86_64'], ['aarch64', 'aarch64']], value => job.runner.arch = value);
    select(fields, 'Docker 엔진 조건', job.runner.docker ?? '', [['', '조건 없음'], ['linux', 'Linux 엔진 사용 가능'], ['windows', 'Windows 엔진 사용 가능']], value => {if (value) job.runner.docker = value; else delete job.runner.docker;});
    input(fields, '실행 풀', job.runner.pool, value => job.runner.pool = value);
    select(fields, '셸', job.shell, [['sh', 'sh'], ['power_shell', 'PowerShell']], value => job.shell = value);
    const timeout = input(fields, '제한 시간 (초)', String(job.timeout_seconds), value => job.timeout_seconds = Number(value)); timeout.setAttribute('type', 'number'); timeout.setAttribute('min', '1'); timeout.setAttribute('max', '86400'); editor.append(fields);
    editor.append(el('p', '에이전트는 프로젝트와 독립된 공용 자원입니다. OS·아키텍처와 Docker 엔진 종류·가용성으로 배정하며 Windows에는 PowerShell을 사용합니다.', 'caption'));
    if (definition.workspace === 'pipeline') editor.append(action('이 실행 조건을 모든 작업에 적용', () => {for (const other of definition.jobs) other.runner = structuredClone(job.runner); changed(); renderForm();}));
    const needs = el('fieldset', undefined, 'editor-needs');needs.tabIndex=-1; needs.append(el('legend', '선행 작업 (완료 후 실행)'));
    for (const other of definition.jobs) if (other !== job) {
      const label = el('label', undefined, 'retention-toggle'); const check = el('input'); check.type = 'checkbox'; check.checked = job.needs.includes(other.name);
      check.addEventListener('change', () => {job.needs = check.checked ? [...job.needs, other.name] : job.needs.filter(name => name !== other.name); changed();}); label.append(check, el('span', other.name)); needs.append(label);
    }
    // Invalid TOML can enter the form through a diagnostic. Keep broken references visible and removable.
    for (const dependency of new Set(job.needs.filter(name => name===job.name || !definition.jobs.some(other=>other.name===name)))) {
      const label=el('label',undefined,'retention-toggle');const check=el('input');check.type='checkbox';check.checked=true;
      check.addEventListener('change',()=>{job.needs=job.needs.filter(name=>name!==dependency);changed();renderForm();});
      label.append(check,el('span',`${dependency} · ${dependency===job.name?'자기 참조':'없는 작업'} (해제하여 제거)`));needs.append(label);
    }
    if (definition.jobs.length === 1) needs.append(el('p', '다른 작업이 없습니다.', 'caption')); editor.append(needs);
    editor.append(el('h3', '스크립트'), el('p', '각 명령은 여러 줄을 유지합니다. 명령 순서대로 실행됩니다.', 'caption'));
    job.script.forEach((script, i) => {
      const row = el('div', undefined, 'editor-command'); input(row, `명령 ${i+1}`, script, value => job.script[i] = value, true);
      const controls = el('div', undefined, 'row');
      if (i > 0) controls.append(action(`명령 ${i+1} 위로`, () => {[job.script[i-1], job.script[i]] = [job.script[i], job.script[i-1]]; changed(); renderForm();}, 'small'));
      if (i < job.script.length-1) controls.append(action(`명령 ${i+1} 아래로`, () => {[job.script[i+1], job.script[i]] = [job.script[i], job.script[i+1]]; changed(); renderForm();}, 'small'));
      controls.append(action(`명령 ${i+1} 삭제`, () => {if (job.script.length === 1) throw new Error('명령이 한 개 이상 필요합니다.'); job.script.splice(i, 1); changed(); renderForm();}, 'small danger')); row.append(controls); editor.append(row);
    });
    editor.append(action('명령 추가', () => {if (job.script.length >= 100) throw new Error('명령은 100개까지 추가할 수 있습니다.'); job.script.push(''); changed(); renderForm();}, 'small'));
    const preserved = el('details'); preserved.append(el('summary', '보존된 추가 설정 (TOML에서 편집)'), el('pre', JSON.stringify({environment: job.environment, publish_generated: definition.publish_generated, on_success: definition.on_success, artifacts: definition.artifacts, artifact_inputs: definition.artifact_inputs, deployment: definition.deployment}, null, 2))); editor.append(preserved);
    const ordering = el('div', undefined, 'row');
    for (const delta of [-1, 1]) if (definition.jobs[selected+delta]) ordering.append(action(delta === -1 ? '작업 순서 위로' : '작업 순서 아래로', () => {const to = selected+delta; [definition.jobs[selected], definition.jobs[to]] = [definition.jobs[to], definition.jobs[selected]]; selected = to; changed(); renderForm();}, 'small'));
    ordering.append(action('작업 삭제', () => {removeJob(definition, selected); selected = Math.min(selected, definition.jobs.length-1); changed(); renderForm();}, 'small danger')); editor.append(ordering); form.append(editor);
    if (!editing) {
      for (const button of Array.from(form.querySelectorAll('button'))) button.hidden = true;
      for (const field of Array.from(form.querySelectorAll('input,textarea,select'))) {
        if (field === jobSelect) continue;
        if (field instanceof HTMLSelectElement || field instanceof HTMLInputElement && field.type === 'checkbox') field.disabled = true;
        else (field as HTMLInputElement | HTMLTextAreaElement).readOnly = true;
      }
    }
    notifyView();
  }
  const file = el('input'); file.type = 'file'; file.accept = '.toml,text/plain'; file.setAttribute('aria-label', 'TOML 파일 가져오기');
  file.addEventListener('change', async () => {
    const source = file.files?.[0]; if (!source || busy) return; const sequence = ++importSequence;
    if (source.size > 256*1024) {status.textContent = '설정은 256 KiB 이하여야 합니다.'; return;}
    busy = true; fields.disabled = true;
    try {const value = await source.text(); if (active() && sequence === importSequence) {text.value = value; originalText = value; changed();}}
    catch {if (active()) status.textContent = '파일을 읽지 못했습니다.';}
    finally {busy = false; fields.disabled = false; file.value = '';}
  });
  async function save(review: boolean) {
    const toml = model ? (await preview()).toml : text.value;
    if (!active()) return;
    const result = await context.api<{generation: number; base_version: number; definition: EditorDefinition}>(`${path}/draft`, 'PUT', {...draft, toml});
    if (!active()) return;
    draft = {generation: result.generation, base_version: result.base_version}; text.value = toml; savedText = toml; if (model) baseline = JSON.stringify(model);
    savedDefinition = structuredClone(result.definition);
    diagnostics.clear();
    updateState(); status.textContent = '초안을 저장했습니다. 변경 비교에서 검토한 뒤 새 버전을 발행하세요.';
    if (review) {busy = false; await context.review();}
  }
  if (context.writable) actions.append(
    action('초안 저장', () => save(false), 'primary'), action('초안 저장·변경 비교', () => save(true), 'primary'),
    action('변경 비교·발행', async () => {if (dirty()) throw new Error('변경한 내용을 먼저 초안으로 저장해 주세요.'); busy = false; await context.review();}), file);
  for (const item of Array.from(actions.children)) if (item !== file) item.classList.add('editor-write-action');
  actions.append(action('TOML 내려받기', async () => {const value = model && editing ? (await preview()).toml : text.value; if (active()) download(value, `${context.name}.toml`);}));
  const unload = (event: BeforeUnloadEvent) => {if (dirty() || busy) {event.preventDefault(); event.returnValue = '';}};
  window.addEventListener('beforeunload', unload); renderMode(); if (model) renderForm(); updateState();
  return {
    canLeave: () => {
      if (busy) {context.message('처리가 끝난 뒤 이동해 주세요.'); return false;}
      return !dirty() || window.confirm('저장하지 않은 초안 변경이 있습니다. 변경을 버리고 이동할까요?');
    },
    dispose: () => {disposed = true; importSequence++;diagnostics.dispose(); window.removeEventListener('beforeunload', unload);}
  };
}
