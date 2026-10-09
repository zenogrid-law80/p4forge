import type {Route} from './navigation.js';
import {layoutConnections, CONNECTION_WIDTH as WIDTH, CONNECTION_HEIGHT as HEIGHT} from './pipeline-connections.js';

export type ProjectNode = {key:string; pipeline_id:string; name:string; version:number|null; current_version:number; job_count:number|null; configuration:{generation:number;category:string;needs:string[];job_count:number}|null; latest_run:{id:string;version:number;status:string;changelist:number;created_at:string;deployment_status:string|null}|null; artifacts?:string[]; artifact_inputs?:string[]; deployment?:boolean};
type Edge = {from:string;to:string;kind:'published'|'configuration';artifacts?:string[];changelist_mode?:string};
type Report = {project_id:string;view:'published'|'configuration';checked_at:string;pipeline_count:number;nodes:ProjectNode[];edges:Edge[];warnings:{key:string;message:string}[]};
type Context = {route:Route;api:<T>(path:string)=>Promise<T>;link:(title:string,route:Route,className?:string)=>HTMLAnchorElement;onState:(patch:Partial<Route>)=>void};

// Pack disconnected components, so independent pipelines form a useful grid.
// Version keys preserve the meaning of links pinned to older publications.
export function layoutProjectGraph(nodes:{key:string}[],edges:{from:string;to:string}[]) {
  const byKey = new Map(nodes.map(node=>[node.key,node]));
  if (byKey.size !== nodes.length) throw new Error('중복 노드가 있어 목록으로 표시합니다.');
  const neighbors = new Map(nodes.map(node=>[node.key,new Set<string>()]));
  for (const edge of edges) {
    if (!byKey.has(edge.from) || !byKey.has(edge.to)) throw new Error('연결 대상을 찾을 수 없어 목록으로 표시합니다.');
    neighbors.get(edge.from)!.add(edge.to); neighbors.get(edge.to)!.add(edge.from);
  }
  const visited = new Set<string>(); const placed:{key:string;x:number;y:number}[]=[];
  let x=0,y=0,rowHeight=0,width=0;
  for (const node of nodes) {
    if (visited.has(node.key)) continue;
    const group=[node.key]; visited.add(node.key);
    for (let i=0;i<group.length;i++) for (const child of neighbors.get(group[i])!) if (!visited.has(child)) {visited.add(child);group.push(child);}
    const keys = new Set(group);
    const layout = layoutConnections(nodes.filter(n=>keys.has(n.key)),edges.filter(e=>keys.has(e.from)));
    if (x && x+layout.width>1120) {x=0;y+=rowHeight+24;rowHeight=0;}
    placed.push(...layout.nodes.map(n=>({...n,x:n.x+x,y:n.y+y})));
    width=Math.max(width,x+layout.width);rowHeight=Math.max(rowHeight,layout.height);x+=layout.width+24;
  }
  return {nodes:placed,width,height:y+rowHeight};
}
export function selectProjectNode(nodes:ProjectNode[], pipeline?:string, version?:string) {
  return nodes.find(n=>n.pipeline_id===pipeline && (version ? n.version===Number(version) : n.version===null || n.version===n.current_version)) ?? nodes[0];
}
export function nodeLabel(node:ProjectNode, view:string) {
  if (view==='configuration') return node.configuration ? `가져온 구성 g${node.configuration.generation}` : '가져온 구성 없음';
  if (!node.version) return '미발행';
  return `v${node.version}${node.version===node.current_version ? ' · 현재 발행' : ` · 고정 과거 버전 (현재 v${node.current_version})`}`;
}
const statusNames:Record<string,string>={queued:'대기',running:'실행 중',succeeded:'성공',failed:'실패',lost:'연결 유실',canceled:'취소',awaiting_approval:'승인 대기',recovery_required:'복구 확인 필요'};
function el<K extends keyof HTMLElementTagNameMap>(tag:K,text?:string,className?:string) {const n=document.createElement(tag);if(text!==undefined)n.textContent=text;if(className)n.className=className;return n;}
function action(text:string,click:()=>void) {const b=el('button',text,'small');b.type='button';b.addEventListener('click',click);return b;}

export function createProjectGraph(content:HTMLElement, context:Context) {
  let route={...context.route}; let data:Report|undefined; let selected:string|undefined; let disposed=false; let request=0; let scale=1;
  let layout:ReturnType<typeof layoutProjectGraph>|undefined; let buttons=new Map<string,HTMLButtonElement>();
  const panel=el('section',undefined,'card project-graph');const controls=el('div',undefined,'row project-graph-controls');
  const viewLabel=el('label','연결 기준');const view=el('select');
  for (const [value,title] of [['published','발행된 실행 연결'],['configuration','가져온 구성 의존']]) {const option=el('option',title);option.value=value;view.append(option);}
  view.value=route.view??'published';viewLabel.append(view);
  const searchLabel=el('label','파이프라인 검색');const search=el('input');search.type='search';search.placeholder='이름으로 찾기';search.value=route.q??'';searchLabel.append(search);
  const refresh=action('그래프 새로고침',()=>void load()); const status=el('p','','caption');status.setAttribute('role','status');
  controls.append(viewLabel,searchLabel,refresh,context.link('목록 보기',{page:'pipelines',project:route.project}));
  const description=el('p','','caption');const count=el('p','','project-graph-count');const warnings=el('div',undefined,'graph-error');
  const results=el('div',undefined,'project-graph-results');results.setAttribute('aria-label','검색 결과');
  const toolbar=el('div',undefined,'row graph-navigation');const zoom=el('span','100%','caption');
  const body=el('div',undefined,'project-graph-body');const graph=el('div',undefined,'project-graph-main');
  const viewport=el('div',undefined,'project-graph-viewport');viewport.tabIndex=0;viewport.setAttribute('role','region');viewport.setAttribute('aria-label','프로젝트 전체 그래프 · 가로와 세로 스크롤');
  const space=el('div',undefined,'project-graph-space');const canvas=el('div',undefined,'project-graph-canvas');space.append(canvas);viewport.append(space);
  const details=el('section',undefined,'project-graph-detail');details.setAttribute('aria-label','선택한 파이프라인 상세');
  const remember=(patch:Partial<Route>)=>{route={...route,...patch};context.onState(patch);};
  const applyScale=(value:number)=>{
    const previous=scale;scale=Math.max(0.2,Math.min(1.5,value));zoom.textContent=`${Math.round(scale*100)}%`;
    canvas.style.transform=`scale(${scale})`;space.style.width=`${(layout?.width??0)*scale}px`;space.style.height=`${(layout?.height??0)*scale}px`;
    viewport.scrollLeft=(viewport.scrollLeft+viewport.clientWidth/2)*scale/previous-viewport.clientWidth/2;
    viewport.scrollTop=(viewport.scrollTop+viewport.clientHeight/2)*scale/previous-viewport.clientHeight/2;
  };
  const center=()=>{const point=layout?.nodes.find(n=>n.key===selected);if(point) {viewport.scrollLeft=(point.x+WIDTH/2)*scale-viewport.clientWidth/2;viewport.scrollTop=(point.y+HEIGHT/2)*scale-viewport.clientHeight/2;}};
  const fit=()=>{if(layout?.width&&layout.height) {applyScale(Math.min(1,(viewport.clientWidth-24)/layout.width,(viewport.clientHeight-24)/layout.height));viewport.scrollLeft=0;viewport.scrollTop=0;}};
  const zoomControls=[action('축소',()=>applyScale(scale-0.1)),action('100%',()=>applyScale(1)),action('확대',()=>applyScale(scale+0.1)),action('화면 맞춤',fit),action('선택으로 이동',center)];
  toolbar.append(...zoomControls,zoom);graph.append(toolbar,viewport);body.append(graph,details);
  panel.append(controls,description,count,status,warnings,results,body);content.append(panel);
  function choose(key:string,move=false) {
    const node=data?.nodes.find(n=>n.key===key);if(!node||!data)return;selected=key;
    remember({pipeline:node.pipeline_id,version:view.value==='published'&&node.version ? String(node.version):undefined});
    for(const [id,b] of buttons)b.setAttribute('aria-pressed',String(id===key));
    for(const b of Array.from(results.querySelectorAll<HTMLButtonElement>('button')))b.setAttribute('aria-pressed',String(b.dataset.key===key));
    details.replaceChildren(el('h2',node.name),el('p',nodeLabel(node,view.value),'caption'));
    if(view.value==='configuration') {
      details.append(el('p',node.configuration?`분류 ${node.configuration.category||'없음'} · 작업 ${node.configuration.job_count}개`:'가져온 구성 초안이 없는 파이프라인입니다.','caption'));
    } else {
      if(node.job_count!==null)details.append(el('p',`작업 ${node.job_count}개${node.deployment?' · 승인 배포':''}`,'caption'));
      if(node.version)details.append(el('p',`입력 산출물: ${node.artifact_inputs?.join(', ')||'없음'}`,'caption'),el('p',`보관 산출물: ${node.artifacts?.join(', ')||'없음'}`,'caption'));
      if(node.latest_run) {const run=node.latest_run;details.append(el('p',`v${run.version} 최근 실행: ${statusNames[run.deployment_status??'']??statusNames[run.status]??run.status} · CL ${run.changelist} · ${new Date(run.created_at).toLocaleString('ko-KR')}`,'caption'),context.link('최근 실행 보기',{page:'run',project:route.project,pipeline:node.pipeline_id,run:run.id}));}
      else details.append(el('p',node.version?'이 버전의 실행 이력이 없습니다.':'발행된 실행 버전이 없습니다.','caption'));
    }
    const links=el('div',undefined,'row');links.append(context.link('CI 설정',{page:'ci-settings',project:route.project,pipeline:node.pipeline_id}),context.link('파이프라인 상세',{page:'pipeline',project:route.project,pipeline:node.pipeline_id}));
    if(view.value==='published'&&node.version)links.append(context.link(`v${node.version} 연결 상세`,{page:'pipeline',project:route.project,pipeline:node.pipeline_id,tab:'connections',version:String(node.version)}));
    details.append(links,el('h3','연결 관계'));
    const related=data.edges.filter(e=>e.from===key||e.to===key);if(!related.length)details.append(el('p','연결 없는 독립 파이프라인입니다.','caption'));
    const byKey=new Map(data.nodes.map(n=>[n.key,n]));
    for(const edge of related) {
      const from=byKey.get(edge.from),to=byKey.get(edge.to);if(!from||!to)continue;
      const row=el('div',undefined,'project-graph-relation');const label=(n:ProjectNode)=>n.name+(n.version?` v${n.version}`:'');
      row.append(action(label(from),()=>choose(from.key,true)),el('span','→'),action(label(to),()=>choose(to.key,true)));
      row.append(el('p',edge.kind==='configuration'?'구성 needs · 실행 예약 없음':`성공 후 실행 · ${edge.changelist_mode==='published_or_input'?'게시 결과 또는 입력 CL':'입력 CL'} · 산출물 ${edge.artifacts?.join(', ')||'없음'}`,'caption'));details.append(row);
    }
    if(move)center();
  }
  function searchNodes() {
    if(!data)return;const query=search.value.trim().toLocaleLowerCase();const found=data.nodes.filter(n=>n.name.toLocaleLowerCase().includes(query));const keys=new Set(found.map(n=>n.key));
    for(const [key,b]of buttons)b.classList.toggle('search-muted',!!query&&!keys.has(key));
    count.textContent=`전체 ${data.pipeline_count}개 파이프라인 · ${data.nodes.length}개 노드 · 연결 ${data.edges.length}개${query?` · 검색 일치 ${new Set(found.map(n=>n.pipeline_id)).size}개`:''}`;
    results.replaceChildren();results.hidden=!query;
    if(query) {results.append(el('p','전체 그래프는 유지하며 검색 결과를 강조합니다.','caption'));if(!found.length)results.append(el('p','검색 결과가 없습니다.','caption'));
      for(const node of found) {const b=action(`${node.name} · ${nodeLabel(node,view.value)}`,()=>choose(node.key,true));b.dataset.key=node.key;b.setAttribute('aria-pressed',String(node.key===selected));results.append(b);}}
  }
  function render(report:Report) {
    const prior={left:viewport.scrollLeft,top:viewport.scrollTop};data=report;canvas.replaceChildren();buttons=new Map();warnings.replaceChildren();
    description.textContent=view.value==='published'?'실선: 발행 버전의 성공 후 실행. 고정된 과거 버전은 별도 노드로 표시합니다. 최근 실행은 각 버전의 이력이며, 배포는 별도 승인이 필요합니다.':'점선: 가져온 구성 초안의 needs 방향(선행 → 후속). 이 관계는 실행이나 자동 빌드를 예약하지 않습니다.';
    for(const warning of report.warnings)warnings.append(el('p',warning.message));
    let fallback=false;try{layout=layoutProjectGraph(report.nodes,report.edges);}catch(error){layout=undefined;fallback=true;warnings.append(el('p',String(error instanceof Error?error.message:error)));}
    canvas.classList.toggle('project-graph-fallback',fallback);zoomControls.forEach(b=>b.disabled=fallback||!report.nodes.length);
    if(layout) {
      canvas.style.width=`${layout.width}px`;canvas.style.height=`${layout.height}px`;
      const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('width',String(layout.width));svg.setAttribute('height',String(layout.height));svg.setAttribute('aria-hidden','true');svg.classList.add('graph-edges');
      const points=new Map(layout.nodes.map(n=>[n.key,n]));
      for(const edge of report.edges) {const from=points.get(edge.from)!,to=points.get(edge.to)!;const x=from.x+WIDTH,y=from.y+HEIGHT/2,end=to.x,endY=to.y+HEIGHT/2;
        const path=document.createElementNS(svg.namespaceURI,'path');path.setAttribute('d',`M ${x} ${y} C ${x+40} ${y}, ${end-40} ${endY}, ${end} ${endY}`);path.setAttribute('fill','none');path.setAttribute('stroke','currentColor');path.setAttribute('stroke-width','2');if(edge.kind==='configuration')path.setAttribute('stroke-dasharray','6 5');svg.append(path);
        const arrow=document.createElementNS(svg.namespaceURI,'path');arrow.setAttribute('d',`M ${end-7} ${endY-4} L ${end} ${endY} L ${end-7} ${endY+4}`);arrow.setAttribute('fill','none');arrow.setAttribute('stroke','currentColor');arrow.setAttribute('stroke-width','2');svg.append(arrow);}
      canvas.append(svg);
    }
    const points=new Map(layout?.nodes.map(n=>[n.key,n])??[]);
    for(const node of report.nodes) {const b=action('',()=>choose(node.key));b.className='project-graph-node';b.setAttribute('aria-label',`${node.name} · ${nodeLabel(node,view.value)} · 상세 보기`);
      const point=points.get(node.key);if(point){b.style.left=`${point.x}px`;b.style.top=`${point.y}px`;}
      b.classList.toggle('historical',!!node.version&&node.version!==node.current_version);b.classList.toggle('unpublished',node.version===0);
      b.append(el('strong',node.name),el('span',nodeLabel(node,view.value),'caption'),el('span',node.job_count!==null?`${node.job_count}개 작업${node.deployment?' · 승인 배포':''}`:node.configuration?'가져온 구성 초안':'실행 초안','caption'));
      if(view.value==='published')b.append(el('span',node.latest_run?`최근 실행 · ${statusNames[node.latest_run.deployment_status??'']??statusNames[node.latest_run.status]??node.latest_run.status}`:'실행 이력 없음','caption'));
      else b.append(el('span',node.configuration?.category||'분류 없음','caption'));
      buttons.set(node.key,b);canvas.append(b);}
    body.hidden=!report.nodes.length;
    if(!report.nodes.length) {details.replaceChildren();selected=undefined;remember({pipeline:undefined,version:undefined});warnings.append(el('p','등록된 파이프라인이 없습니다. 목록 보기에서 첫 파이프라인을 추가하세요.'));}
    else {const chosen=selectProjectNode(report.nodes,route.pipeline,route.version)!;if(route.pipeline&&(chosen.pipeline_id!==route.pipeline||route.version&&chosen.version!==Number(route.version)))warnings.append(el('p','선택한 항목을 찾을 수 없어 첫 파이프라인을 표시합니다.'));choose(chosen.key);}
    if(fallback){canvas.style.width='';canvas.style.height='';canvas.style.transform='';space.style.width='';space.style.height='';zoom.textContent='목록';}
    else {applyScale(scale);viewport.scrollLeft=prior.left;viewport.scrollTop=prior.top;}
    searchNodes();
  }
  async function load() {
    const current=++request;refresh.disabled=true;status.className='caption';status.textContent='전체 파이프라인을 불러오는 중…';const requestedView=view.value;
    try {const report=await context.api<Report>(`/api/projects/${encodeURIComponent(route.project!)}/graph?view=${requestedView}`);
      if(disposed||current!==request||!content.isConnected)return;
      if(report.project_id!==route.project||report.view!==requestedView)throw new Error('프로젝트 또는 연결 기준이 응답과 일치하지 않습니다.');
      render(report);status.textContent=`${new Date(report.checked_at).toLocaleString('ko-KR')} 기준 · 조회 전용`;
    }catch(error){if(disposed||current!==request||!content.isConnected)return;
      const code=(error as {status?:number}).status;
      if([401,403,404].includes(code??0)){data=undefined;canvas.replaceChildren();details.replaceChildren();results.replaceChildren();warnings.replaceChildren();count.textContent='';body.hidden=true;}
      status.className='graph-error';status.textContent=`조회 실패${data?` · ${new Date(data.checked_at).toLocaleString('ko-KR')}의 마지막 결과`:''}: ${error instanceof Error?error.message:String(error)}`;
    }finally{if(!disposed&&current===request)refresh.disabled=false;}
  }
  search.addEventListener('input',()=>{remember({q:search.value||undefined});searchNodes();});
  view.addEventListener('change',()=>{remember({view:view.value,version:undefined});data=undefined;body.hidden=true;results.replaceChildren();warnings.replaceChildren();count.textContent='';scale=1;void load();});
  void load();return()=>{disposed=true;request++;};
}
