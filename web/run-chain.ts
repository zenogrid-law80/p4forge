import type {Route} from './navigation.js';
import {layoutConnections, CONNECTION_WIDTH, CONNECTION_HEIGHT} from './pipeline-connections.js';

export type ChainNode = {key: string; kind: 'run' | 'followup'; run_id: string | null; pipeline_id: string; name: string; version: number; status: string; cancel_requested?: boolean; changelist?: number; created_at?: string; finished_at?: string | null; deployment_status?: string | null; publication?: {status: string; changelist: number} | null; parent_run_id?: string; reason?: string};
type Report = {checked_at: string; project_id: string; root: string; selected_run_id: string; ongoing: boolean; nodes: ChainNode[]; edges: {from: string; to: string}[]};
type Context = {runId: string; api: <T>(path: string) => Promise<T>; link: (text: string, route: Route, className?: string) => HTMLAnchorElement; onState: (status: string, cancelRequested: boolean) => void};
const labels: Record<string, string> = {queued:'배정 대기',running:'실행 중',succeeded:'성공',failed:'실패',lost:'연결 유실',canceled:'취소',pending:'후속 등록 대기',blocked:'후속 등록 차단',awaiting_parent:'선행 성공 대기',not_scheduled:'실행 안 함',awaiting_approval:'배포 승인 대기',recovery_required:'배포 복구 확인 필요'};
export function chainLabel(node: ChainNode): string {
  if (node.kind === 'run' && node.deployment_status && ['awaiting_approval','recovery_required'].includes(node.deployment_status)) return labels[node.deployment_status];
  if (node.kind === 'run' && node.cancel_requested && ['queued','running'].includes(node.status)) return '취소 처리 중';
  return labels[node.status] ?? node.status;
}
export function chainRunRoute(node: ChainNode, project: string): Route | undefined {
  if (node.kind !== 'run' || !node.run_id) return undefined;
  return {page:'run',project,pipeline:node.pipeline_id,run:node.run_id,tab:['awaiting_approval','recovery_required'].includes(node.deployment_status ?? '') ? 'deployment' : 'graph'};
}
function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] {const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node;}
const time = (value: string) => new Date(value).toLocaleString('ko-KR');

export function createRunChainView(content: HTMLElement, context: Context): () => void {
  const panel = el('section',undefined,'card run-chain');
  panel.append(el('h2','실행 체인'),el('p','이 실행이 속한 자동 후속 실행의 흐름입니다. 실제 실행 ID로 연결하며, 수동 실행·재실행·복구 실행과 산출물 제공 관계는 별도입니다.','caption'));
  const controls = el('div',undefined,'row workspace-refresh');
  const refresh = el('button','체인 새로고침','small'); refresh.type = 'button';
  const autoLabel = el('label','5초 자동 갱신','retention-toggle'); const auto = el('input'); auto.type = 'checkbox'; auto.checked = true; autoLabel.prepend(auto);
  const status = el('span','불러오는 중…','caption'); status.setAttribute('role','status'); controls.append(refresh,autoLabel,status); panel.append(controls);
  const body = el('div'); panel.append(body); content.append(panel);
  let disposed = false; let loading = false; let ongoing = true; let checked: string | undefined; let selected = context.runId; let lastContent = '';
  const render = (data: Report) => {
    const oldViewport = body.querySelector('.connections-viewport');
    const scroll = {left:oldViewport?.scrollLeft ?? 0,top:oldViewport?.scrollTop ?? 0};
    const focused = document.activeElement instanceof HTMLElement ? document.activeElement.dataset.chainKey : undefined;
    const focusViewport = document.activeElement === oldViewport;
    ongoing = data.ongoing;
    const current = data.nodes.find(node=>node.run_id === context.runId);
    if (current) context.onState(current.status,Boolean(current.cancel_requested));
    if (!data.nodes.some(node=>node.key === selected)) selected = context.runId;
    body.replaceChildren();
    const actual = data.nodes.filter(node=>node.kind === 'run').length;
    body.append(el('p',`실제 실행 ${actual}개 · 아직 생성되지 않은 후속 ${data.nodes.length-actual}개`,'caption'));
    body.append(el('p','점선 노드는 아직 실행 ID가 없습니다. 노드를 선택하면 상세와 이동 링크를 볼 수 있습니다. 넓은 그래프는 좌우로 스크롤하세요.','caption'));
    if (!data.edges.length) body.append(el('p','이 실행에 연결된 자동 선행·후속 실행이 없습니다.','caption'));
    const viewport = el('div',undefined,'connections-viewport'); viewport.tabIndex=0; viewport.setAttribute('role','region'); viewport.setAttribute('aria-label','실행 체인 그래프 · 넘치는 영역은 스크롤하여 확인');
    const canvas = el('div',undefined,'connections-canvas'); viewport.append(canvas);
    let layout: ReturnType<typeof layoutConnections> | undefined;
    try {layout=layoutConnections(data.nodes,data.edges);} catch {canvas.classList.add('connections-fallback');body.append(el('p','연결 그래프를 그릴 수 없어 목록으로 표시합니다.','graph-error'));}
    if (layout) {
      canvas.style.width=`${layout.width}px`;canvas.style.height=`${layout.height}px`;
      const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('width',String(layout.width));svg.setAttribute('height',String(layout.height));svg.setAttribute('aria-hidden','true');svg.classList.add('graph-edges');
      const defs=document.createElementNS(svg.namespaceURI,'defs');const marker=document.createElementNS(svg.namespaceURI,'marker');marker.setAttribute('id','chain-arrow');marker.setAttribute('viewBox','0 0 10 10');marker.setAttribute('refX','9');marker.setAttribute('refY','5');marker.setAttribute('markerWidth','6');marker.setAttribute('markerHeight','6');marker.setAttribute('orient','auto');const tip=document.createElementNS(svg.namespaceURI,'path');tip.setAttribute('d','M 0 0 L 10 5 L 0 10');tip.setAttribute('fill','none');tip.setAttribute('stroke','#93a4ba');marker.append(tip);defs.append(marker);svg.append(defs);
      for (const edge of data.edges) {
        const from=layout.nodes.find(node=>node.key===edge.from)!;const to=layout.nodes.find(node=>node.key===edge.to)!;
        const x=from.x+CONNECTION_WIDTH,y=from.y+CONNECTION_HEIGHT/2,tx=to.x-5,ty=to.y+CONNECTION_HEIGHT/2;
        const path=document.createElementNS(svg.namespaceURI,'path');path.setAttribute('d',`M ${x} ${y} C ${x+42} ${y}, ${tx-42} ${ty}, ${tx} ${ty}`);path.setAttribute('fill','none');path.setAttribute('stroke','#93a4ba');path.setAttribute('stroke-width','1.5');path.setAttribute('marker-end','url(#chain-arrow)');svg.append(path);
      }
      canvas.append(svg);
    }
    const details=el('section',undefined,'connection-detail');details.id='run-chain-details';details.setAttribute('aria-label','선택한 실행 체인 상세');
    const buttons=new Map<string,HTMLButtonElement>();
    const choose=(key:string)=>{
      selected=key;const node=data.nodes.find(node=>node.key===key)!;
      for(const [id,button] of buttons) button.setAttribute('aria-pressed',String(id===key));
      details.replaceChildren(el('h3',`${node.name} · v${node.version}`),el('p',chainLabel(node),'chain-state'));
      const related=data.edges.filter(edge=>edge.from===key||edge.to===key);
      const actions=el('div',undefined,'row');
      const route=chainRunRoute(node,data.project_id);
      if(route){
        details.append(el('p',`실행 ID: ${node.run_id}`,'caption'),el('p',`입력 CL ${node.changelist} · 등록 ${time(node.created_at!)}${node.finished_at ? ` · 종료 ${time(node.finished_at)}` : ''}`,'caption'));
        if(node.publication) details.append(el('p',node.publication.status==='prepared' ? `Perforce CL ${node.publication.changelist} 제출 예약 · 결과 확인 필요` : node.publication.status==='submitted' ? `Perforce 제출 확인 · 후속 입력 CL ${node.publication.changelist}` : `Perforce 변경 없음 · 후속 입력 CL ${node.publication.changelist}`,'caption'));
        actions.append(context.link(route.tab==='deployment'?'배포 승인·결과 →':'실행 그래프·로그 →',route),context.link('배정 점검 →',{...route,tab:'dispatch'}),context.link('산출물·제공 실행 →',{...route,tab:'artifacts'}));
        if(node.run_id!==context.runId) actions.append(context.link('이 실행의 체인 열기 →',{...route,tab:'chain'}));
      }else{
        const descriptions:Record<string,string>={awaiting_parent:'선행 실행이 성공하면 후속 등록을 시작합니다.',pending:'후속 실행을 등록 중입니다. 일시적인 오류는 서버가 다시 시도합니다.',blocked:'후속 등록이 차단됐습니다. 원인을 확인하세요. 자동 재시도하지 않습니다.',not_scheduled:'선행 실행의 실패·취소로 성공 후 실행 조건을 충족하지 못했습니다.'};
        details.append(el('p',descriptions[node.status]??'아직 실행이 생성되지 않았습니다.','caption'));
        if(node.reason) details.append(el('p',`차단 사유: ${node.reason}`,'chain-blocked-reason'));
        details.append(el('p','실행 ID와 확정된 입력 CL은 생성 후 표시합니다.','caption'));
      }
      actions.append(context.link('발행 버전 연결 →',{page:'pipeline',project:data.project_id,pipeline:node.pipeline_id,tab:'connections',version:String(node.version)}));details.append(actions);
      if(related.length){
        details.append(el('h4','선행·후속 관계'));const list=el('ul',undefined,'chain-relations');
        for(const edge of related){
          const other=data.nodes.find(item=>item.key===(edge.from===key?edge.to:edge.from))!;const item=el('li');
          const button=el('button',`${edge.from===key?'후속':'선행'} · ${other.name} v${other.version} · ${chainLabel(other)}`,'small');button.type='button';
          button.addEventListener('click',()=>{choose(other.key);buttons.get(other.key)?.focus();});item.append(button);list.append(item);
        }
        details.append(list);
      }
    };
    for(const node of data.nodes){
      const button=el('button',undefined,`connection-node chain-node ${node.kind==='followup'?'chain-uncreated':''}`);button.type='button';button.dataset.chainKey=node.key;button.dataset.state=node.status;button.setAttribute('aria-label',`${node.name} v${node.version} · ${node.run_id?`실행 ${node.run_id.slice(0,8)}`:'미생성 후속'} · ${chainLabel(node)} 상세`);button.setAttribute('aria-controls',details.id);
      const point=layout?.nodes.find(point=>point.key===node.key);if(point){button.style.left=`${point.x}px`;button.style.top=`${point.y}px`;}
      button.append(el('strong',node.name),el('span',`v${node.version} · ${node.run_id?`CL ${node.changelist} · ${node.run_id.slice(0,8)}`:'실행 미생성'}`,'caption'),el('span',chainLabel(node),'chain-state'));
      if(node.run_id===context.runId)button.append(el('span','이 화면의 실행','caption'));
      button.addEventListener('click',()=>choose(node.key));buttons.set(node.key,button);canvas.append(button);
    }
    body.append(viewport,details);choose(selected);viewport.scrollLeft=scroll.left;viewport.scrollTop=scroll.top;
    if(!oldViewport && layout){const point=layout.nodes.find(node=>node.key===selected)!;viewport.scrollLeft=Math.max(0,point.x-(viewport.clientWidth-CONNECTION_WIDTH)/2);viewport.scrollTop=Math.max(0,point.y-(viewport.clientHeight-CONNECTION_HEIGHT)/2);}
    if(focused)buttons.get(focused)?.focus({preventScroll:true});else if(focusViewport)viewport.focus({preventScroll:true});
  };
  const fetchChain=async()=>{
    if(disposed||loading)return;loading=true;refresh.disabled=true;
    try{const data=await context.api<Report>(`/api/runs/${encodeURIComponent(context.runId)}/chain`);if(disposed||!content.isConnected)return;const signature=JSON.stringify([data.nodes,data.edges,data.ongoing]);if(signature!==lastContent){render(data);lastContent=signature;}checked=data.checked_at;status.className='caption';status.textContent=`${time(checked)} 기준${ongoing?'':' · 자동 갱신 종료'}`;}
    catch(error){if(!disposed&&content.isConnected){status.className='graph-error';status.textContent=`체인 조회 실패${checked?` · ${time(checked)}의 마지막 결과입니다`:''}: ${error instanceof Error?error.message:String(error)}`;}}
    finally{loading=false;refresh.disabled=false;}
  };
  const visible=()=>{if(!document.hidden&&auto.checked&&ongoing)void fetchChain();};
  refresh.addEventListener('click',()=>{void fetchChain();});auto.addEventListener('change',visible);document.addEventListener('visibilitychange',visible);const timer=window.setInterval(visible,5000);void fetchChain();
  return()=>{disposed=true;clearInterval(timer);document.removeEventListener('visibilitychange',visible);};
}
