# P4Forge

**Perforce 전용 self-hosted CI/CD**. TeamCity의 파이프라인 모델과 [lorehub](https://github.com/zenogrid-law80/lorehub/tree/99dfcb5c5355febc0a018e006595d5c6653fd343)의 Rust 서버 구조·Google Workspace 인증을 참고한다. 전체 로드맵은 [PLAN.md](PLAN.md)에 있다.

현재는 **Agent preview 단계**다. 프로젝트·접근 권한·Perforce 연결·stream/classic/View root, DB 기반 TOML 초안·DAG 검증·발행·복원, 고정 changelist의 빌드 실행·로그·취소를 제공한다. Linux Docker Agent와 Mac mini·DESKTOP-6N6U52L의 native Agent가 빌드하고, `workspace = "pipeline"`은 한 Agent와 checkout을 유지하며 단계별로 실행한다. 고정 버전의 후속 실행·제출 CL 전달·검증된 산출물 전달·보존 정책·Perforce 제출 감지·ticket 자동 갱신·Agent 셀프 패치를 구현했다. Mac·Windows는 0.1.6을 적용했고 실제 산출물 전달 체인과 Docker 상태 보고를 검증했다. 여러 파이프라인의 프로젝트 TOML 가져오기·편집·소스 연결·내보내기도 지원한다. 배포 승인·환경 잠금·최종 상태 확인·성공 배포 복구를 지원한다. **실제 서비스 배포 명령 연결과 지속적인 서비스 모니터링은 후속 범위다.**

`ci.toml`은 편집·import/export 형식이다. 실행 설정의 원본은 DB의 JSONB이며 Perforce에 설정 파일을 둘 필요가 없다. 발행 이력과 Run 입력은 DB trigger로 변경을 금지한다. `generation`과 `base_version`으로 동시 편집·발행 충돌을 `409`로 반환한다. 과거 버전 복원은 초안에 복사하며 다시 발행하면 새 버전이 된다.

## 웹 메뉴와 바로가기

메뉴를 **워크스페이스 / 선택한 프로젝트 / 관리**로 구분한다. 첫 화면은 워크스페이스 개요이며, 프로젝트를 선택하면 파이프라인·배포 환경·프로젝트 설정으로 이동한다. 프로젝트 설정에는 소스 루트, 구성 가져오기·내보내기, 산출물 보존 정책, 멤버·접근 권한을 둔다. 서버 관리자는 별도 관리 메뉴에서 에이전트·Perforce 연결·운영 상태에 접근한다.

파이프라인 목록은 이름 검색과 미발행·발행됨·최근 실행 상태·자동 빌드 필터를 제공하며 최신 실행과 자동 빌드 상태를 한 번의 프로젝트 목록 API로 조회한다. 파이프라인 상세는 **실행 이력 / 연결 관계 / 설정 / 변경 비교 / 자동 빌드 / 발행 이력**, 실행 상세는 **실행 그래프·로그 / 실행 체인 / 배정 점검 / 산출물 / 실행 정보 / 배포 승인·결과**로 나눈다. 배포 탭은 배포 Run에만 표시한다. 실행 등록에서는 발행 버전과 제출 CL을 지정할 수 있다. 배포 환경 목록에는 승인 대기 건수·최근 승인 요청 링크·현재 배포 상태를 함께 표시한다.

선택 상태는 hash URL에 저장한다. `#pipelines?project=<UUID>&q=<검색어>&state=failed`, `#pipeline?project=<UUID>&pipeline=<UUID>&tab=settings`, `#run?run=<UUID>&tab=artifacts`처럼 바로 열 수 있다. 새로고침·뒤로 가기에서 위치와 검색 조건을 복원하며, 최근 실행 100건 밖의 Run도 상세 API로 조회한다. 주소의 프로젝트·파이프라인·실행 관계가 다르면 오류를 표시한다. 기존 `#agents`·`#runners` 주소를 지원한다.

2026-10-09 메뉴 개편과 공용 에이전트 모델에 이어 워크스페이스 개요·전체 실행·운영 상태·에이전트 활성/폐기 필터·배정 점검까지 운영 서버에 배포했다. 파이프라인 연결 관계·실행 체인도 같은 날 운영 반영했다.

### 워크스페이스 개요·전체 실행·운영 상태

**개요**(`/#overview`)는 현재 사용자가 접근할 수 있는 모든 프로젝트의 실행 중·배정 대기·승인 대기·복구 확인 필요 건수와 최근 24시간 종료된 성공·실패를 집계한다. 배정 대기에는 승인 대기를 중복 포함하지 않는다. 최근 실행 8건과 확인할 배포 8건, 프로젝트별 현황을 표시하며 복구 확인이 필요한 배포를 우선한다. 수치는 최근 실행 목록의 건수 제한과 관계없이 전체 이력에서 집계한다.

**전체 실행**(`/#runs`)은 프로젝트·파이프라인 이름과 실행 ID 검색, 프로젝트·상태 필터를 제공한다. 한 페이지에 25건을 보여주며 **이전 실행 보기**로 과거 기록을 계속 조회한다. 등록 시각과 UUID를 함께 사용하는 커서로 같은 시각의 기록이나 새 실행 등록에 따른 중복·누락을 방지한다. 조건과 커서는 URL에 유지하고 조건 변경 시 최신 페이지로 돌아간다. `#runs?project=<UUID>&state=awaiting_approval` 같은 링크를 공유할 수 있다. 각 조회에서 프로젝트 멤버 권한을 다시 검사하므로 커서가 접근 권한을 부여하지 않는다.

서버 관리자용 **운영 상태**(`/#operations`)는 활성 공용 에이전트의 연결·작업·Docker 가용성, 관리형 Perforce 인증, 자동 빌드 점검 지연·오류, 복구 확인이 필요한 배포를 모은다. 30초 이상 연결이 없으면 오프라인으로, 인증·Docker·자동 빌드 검사가 150초 이상 지연되면 해당 확인 대상으로 분류한다. 인증은 서버·에이전트 검사 양쪽이 정상이고 남은 유효 기간이 각각 5분을 초과해야 한다. Docker 미설치는 native 실행기의 장애로 취급하지 않는다. 항목 수는 전체에서 집계하고 상세는 최대 50건을 표시한다. 배포 서비스의 실시간 응답을 직접 검사하는 기능은 아니다.

개요·운영 상태는 화면이 보일 때 15초마다 갱신하고 사용자가 자동 갱신을 끌 수 있다. 실패하면 마지막 성공 결과와 확인 시각을 유지하고 오류를 표시한다. 전체 실행은 페이지가 갑자기 바뀌지 않도록 수동 갱신한다. 새 읽기 API는 `/api/workspace/overview`, `/api/workspace/runs`, 관리자 전용 `/api/operations`이며 migration 0018이 전체 실행 조회용 인덱스를 추가한다. 에이전트 업데이트는 필요하지 않다.

### 실행 진단 요약

개요에서 **현재 확인할 항목**, **배정 대기**, **최근 24시간 실패 이력**을 구분한다. 현재 확인할 항목은 환경 잠금이 남은 배포 복구 → 기록된 후속 등록 차단 → 승인 대기 순으로, 같은 상태에서는 선행 또는 해당 실행의 오래된 등록 순으로 최대 8건을 표시한다. 후속 차단에는 아직 실행 ID가 없으므로 부모 Run의 체인과 고정 대상 버전을 안내한다. 차단 발생 시각을 추정하지 않으며 선행 완료 또는 실행 등록 시각과 개요의 조회 시각을 표시한다.

배정 대기는 승인 대기를 제외하고 오래된 등록 순 최대 8건을 보여주며 **배정 점검**으로 연결한다. 대기 시간만으로 장애를 단정하거나 현재 Agent 상태를 과거 실패 원인으로 사용하지 않는다. 실패 이력은 최근 24시간에 종료된 실패 Run을 최신 완료 순 최대 8건으로 표시한다. 이후 성공한 실행이 있어도 해당 실패 이력은 유지하며, 실패·연결 유실 작업 중 설정 순 첫 작업의 기록된 사유와 종료 코드를 보여준다. 공용 workspace는 해당 단계의 결과를 사용한다. 실패 작업이 없는 기록은 원인을 추정하지 않는다.

**실패 작업·로그**는 정확한 Run과 Job으로 이동한다. `#run?run=<UUID>&tab=graph&job=<Job UUID>` 링크는 새로고침과 뒤로 가기에서 선택 작업을 복원하며, 다른 Run의 Job이면 안내 후 현재 실행의 작업을 표시한다. 후속 차단은 부모 실행 체인, 승인·복구는 해당 배포 상세로 이동한다. 서버 관리자는 개요와 **운영 상태**를 오가며 공용 에이전트·인증·자동 빌드를 확인할 수 있다.

`GET /api/workspace/overview`의 `diagnostics`는 기존 집계와 같은 읽기 전용 DB 스냅샷을 사용한다. 각 그룹의 `total`은 표시 제한과 관계없는 전체 건수이며 프로젝트 권한을 매번 검사한다. 공용 Agent 후보·인증 정보나 작업 스크립트를 추가로 반환하지 않는다. 화면은 15초 갱신·중지, 조회 실패 시 마지막 결과 유지, 진단 링크의 키보드 초점 보존을 지원한다. 조회로 실행·재시도·승인·잠금 해제를 수행하지 않는다. 추가 migration이나 Agent 업데이트는 필요하지 않다. 2026-10-09 운영 배포를 완료했다. 실제 개요의 현재 확인 항목·대기 0건, 최근 24시간 실패 이력 12건 중 8건 표시와 실패 Run `d609f915`의 `health-check` 작업 선택·로그·새로고침 복원을 확인했다. 서버·HTTPS·활성 Agent 2대와 기존 배포 환경이 정상이며 실행·산출물·인증 이력을 보존했다. 백업·복구 이미지와 증빙은 `.local/diagnostics-deployment/`에 있다.

### 실행 배정 점검

실행 상세의 **배정 점검**(`/#run?run=<UUID>&tab=dispatch`)에서 각 작업이 기다리는 원인을 확인한다. 그래프의 대기 작업 상세에서도 바로 이동할 수 있다. 실행에 고정된 OS·아키텍처·Pool·Docker 조건을 사용하므로 이후 파이프라인 설정 변경은 과거 Run의 점검에 영향을 주지 않는다. 취소·배포 승인 및 환경 상태, 선행 작업, 동시 실행 한도, 후보 에이전트 상태를 구분한다. 배포 Run은 해당 환경에서 지정한 정확한 에이전트만 후보로 계산한다.

후보의 활성 여부·최근 연결·새 작업 중지·작업 수행 중 상태, 보고된 지원 프로토콜, Docker 점검 시각·엔진·가용성, 관리형 Perforce 읽기·게시 인증을 확인한다. OS·아키텍처·Pool 일치 후보의 등록/활성/사전 점검 통과 수와 원인별 수를 표시한다. 한 후보에 여러 원인이 있을 수 있다. 프로젝트 열람자는 요약만, 서버 관리자는 후보 이름·UUID·지원 버전과 관리 상세 링크도 볼 수 있다. 상세는 작업당 최대 50대이며 집계는 전체 후보 기준이다.

읽기 전용 API `/api/runs/{id}/dispatch`는 매 조회에서 프로젝트 접근 권한을 확인한다. 실행을 예약하거나 배정하지 않으며 실제 Agent claim 시 권한·승인·잠금·프로토콜·인증·동시 작업 조건을 다시 검사한다. 수동 ticket을 쓰는 에이전트의 Perforce 접속 성공이나 임의 스크립트의 실행 가능성까지 보장하지 않는다. 결과는 5초마다 갱신하며 중지할 수 있고, 실패하면 마지막 확인 시각과 이전 결과를 표시한다. 종료된 Run의 과거 대기 사유는 현재 에이전트 상태로 추정하지 않으며 자동 갱신을 멈춘다.

migration 0019는 인증된 claim에서 마지막 지원 프로토콜과 보고 시각을 관측할 필드를 추가한다. 150초 이내 보고가 없으면 확인 필요로 표시하며 배정 조건 자체는 변경하지 않는다. Agent 업그레이드는 필요하지 않다. 2026-10-09 migration 0018·0019와 이 화면을 운영에 적용했다.

운영 검증: HTTPS의 새 JS·CSS가 로컬 검증본과 일치하고 비인증 조회는 차단된다. 실제 로그인 화면에서 개요·전체 실행·운영 상태·배정 점검과 활성 에이전트 목록을 확인했다. `compose-demo-executor`(protocol 7)와 Windows Agent(protocol 8)는 온라인이며 `mac-mini`는 폐기 상태를 유지한다. 기존 Agent ID·토큰·인증 바인딩, 실행 attempt 48건·산출물 23개와 배포 환경을 보존했고 Compose 테스트 서비스는 기존 `ci-v1` 해시로 정상 응답한다. 검증·DB 백업은 `.local/dispatch-deployment/`, 이전 서버 이미지는 `p4forge-server:pre-dispatch-20261009-092513`에 보관한다.

## 에이전트 탭

관리 영역의 **에이전트** 메뉴(`/#agents`, `#runners`도 지원)는 서버 관리자에게 공용 에이전트를 보여준다. 기본 목록은 활성 에이전트이며 **등록·연결 상태**에서 온라인·오프라인·새 작업 중지·폐기 이력·전체를 선택한다. 폐기된 에이전트는 활성·오프라인·새 작업 중지 건수에서 제외한다. OS·아키텍처·Docker 엔진·가용성 필터와 이름·OS·Pool 검색을 함께 적용하며, 요약 카드로 등록·연결 상태를 전환할 수 있다. 카드 건수는 현재 검색·환경 조건 기준이다.

검색·필터·선택한 관리 상세는 URL에 저장한다. `#agents?state=revoked&agent=<UUID>`, `#agents?state=online&os=macos&engine=linux`처럼 직접 열고 새로고침·뒤로 가기로 복원한다. `agent`만 있는 링크는 해당 에이전트의 폐기 여부와 관계없이 전체 목록에서 상세를 연다. 주소의 에이전트가 필터에서 제외되거나 존재하지 않으면 안내와 이동 링크를 표시한다. 운영 상태의 에이전트 문제 항목은 해당 UUID의 관리 상세로 연결한다.

상태는 5초마다 갱신하며 숨겨진 탭에서는 조회하지 않는다. 다른 화면에서 등록·폐기·작업 중지 상태가 바뀌면 관리 버튼도 다시 구성한다. 등록 후 새 에이전트의 상세를 열고, 폐기 후에는 폐기 이력으로 이동한다. 폐기 이력은 조회 전용이며 기존 실행·패치 이력을 보존한다. 서버에서의 폐기는 호스트 서비스의 종료·삭제를 수행하지 않는다. 등록·배정 중지/재개·자격 증명 폐기·셀프 패치와 Perforce 인증 상세는 이 화면에서 관리한다.

등록·연결 상태 필터와 상세 URL 연결은 2026-10-09 운영에 배포했다. 이 UI 변경 자체에 추가 DB migration이나 Agent 업데이트는 필요하지 않다.

Docker는 실제 실행 계정의 기본 환경에서 `docker info --format '{{.OSType}}'`가 성공하는지 점검한다. **사용 가능**(Linux/Windows 엔진), **사용 불가**(CLI 없음 또는 엔진 연결/권한 문제), **확인 실패**, **확인 전**을 구분한다. 마지막 확인이 150초보다 오래됐거나 에이전트가 오프라인이면 마지막 결과와 **확인 지연**을 표시한다. CLI 설치나 OS만으로 사용 가능하다고 판단하지 않는다.

Native Agent 0.1.5부터 빌드 전용 계정·격리 HOME으로 대기 중 60초 간격 점검하며, 실행 중 빌드와 병행하지 않는다. 파이프라인에서 별도로 지정한 환경 변수는 이 기본 상태에 포함하지 않는다. 전용 Compose 실행기는 자신의 실제 실행 계정·환경으로 확인한다. 상태 보고를 지원하지 않는 기존 버전은 **확인 전**으로 남는다. `/agent/docker-status`는 기존 Agent 자격 증명으로 자기 상태만 기록한다.

### 공용 에이전트와 실행 조건

에이전트는 프로젝트에 속하지 않는 공용 실행 자원이다. 등록 API와 `provision-agent` CLI에는 프로젝트 ID를 받지 않는다. 서버 관리자가 이름·OS·아키텍처·Pool을 등록하고, Docker 엔진 종류와 가용성은 실제 실행 계정의 점검으로 보고한다. Pool은 전용 실행기를 구분하는 추가 조건이다.

작업은 프로젝트와 관계없이 OS·아키텍처·Pool이 맞는 에이전트에 배정한다. Docker가 필요한 작업에는 다음처럼 엔진 조건을 추가한다.

```toml
runner = { os = "macos", arch = "aarch64", pool = "default", docker = "linux" }
```

`runner.docker`는 `linux` 또는 `windows`를 받는다. Agent 0.1.6 / protocol 8 이상이며, 해당 엔진이 최근 150초 안에 사용 가능으로 보고된 경우에만 배정한다. 미보고·점검 실패·오래된 결과·엔진 불일치 상태에서는 대기한다. Docker 조건을 생략한 기존 작업은 이전 Agent에서도 계속 실행된다. 이 조건은 사용할 수 있는 엔진을 선택하며, 실행 스크립트를 자동으로 컨테이너 안에 넣거나 Docker 접근 권한을 부여하지 않는다.

Migration 0017은 기존 ID·인증 토큰·실행 이력·업데이트 이력을 유지하면서 `agents.project_id`를 제거하고 이전 소속을 감사 기록에 남긴다. 공용 자원으로 전환해도 Perforce 연결·서버 ID·명시적 Agent UUID에 대한 인증 바인딩과 배포 환경의 정확한 허용 Agent 지정은 유지한다. credential policy의 `project_id`는 감사 기록의 문맥이며 에이전트 소속이 아니다. 2026-10-09 운영 서버에 migration 0017과 메뉴 개편을 적용하고 Mac·Windows 에이전트를 0.1.6 / protocol 8로 업데이트했다. 기존 ID·토큰과 48개 attempt 이력이 유지됐고 에이전트 3대 재연결, Perforce 읽기·게시 인증, 공개 HTTPS와 배포 환경 상태를 확인했다. 검증 기록과 DB 백업은 `.local/global-agents-deployment/`에 보관한다.

## 실행 그래프

프로젝트 → 파이프라인 → 실행 이력의 실행 ID를 선택하면 **실행 그래프·로그** 탭을 표시한다. 고정된 Run의 실제 Job 의존 관계를 왼쪽에서 오른쪽으로 연결하며 병렬 작업은 위아래로 배치한다. 각 작업에는 stage, 상태, Agent와 공용 workspace 여부를 표시한다. stage 이름만으로 의존 관계를 추가하지 않는다.

노드를 클릭하거나 키보드로 선택하면 선행 작업, 종료 코드, 실패 사유와 로그를 볼 수 있다. 공용 workspace의 로그는 선택한 Job만의 로그가 아닌 해당 attempt 전체 로그임을 표시한다. 로그는 순번 cursor로 추가 조회하며 작업을 바꾸는 동안 늦게 도착한 이전 응답은 반영하지 않는다.

대기·실행 중 작업의 상태는 기본 5초마다 갱신하며 자동 갱신을 끄거나 수동 갱신할 수 있다. 다른 화면으로 이동하면 갱신을 중단하고, 숨겨진 탭에서는 조회를 건너뛴다. 조회 실패 시 마지막 상태와 오류를 함께 표시한다. 좁은 화면이나 큰 그래프는 그래프 영역 안에서 스크롤한다. 실행 그래프는 조회용이며 작업 편집은 파이프라인 설정의 폼 편집기에서 제공한다. 파이프라인 간 연결 조회는 아래 연결 관계 탭에서 제공한다.

그래프 탐색 개선(2026-10-09 운영 반영): **확대·축소 / 100% / 화면 맞춤 / 선택 작업으로 이동**을 제공한다. **이전 실패 / 다음 실패**는 실제 실패·연결 유실 작업만 순환하며 선택한 작업을 URL에 저장한다. **실행 중 작업 따라가기**를 켜면 상태 갱신 때 다음 실행 작업을 선택한다. 여러 작업이 동시에 실행 중이면 현재 작업을 유지하고, 직접 노드를 선택하거나 그래프·로그를 키보드·포인터·스크롤로 탐색하면 따라가기를 끈다. 자동 갱신을 끈 경우 수동 상태 갱신 때 반영된다.

넓은 화면에서는 그래프와 작업 상세·로그를 좌우로, 좁은 화면에서는 위아래로 표시한다. 상태 갱신과 같은 작업 재선택은 배율·로그 내용·스크롤을 유지한다. 일시 오류는 마지막 결과를 유지하지만 로그인 만료·권한 회수가 확인되면 그래프와 로그를 지우고 조회를 중지한다. 증빙은 `.local/graph-navigation-preview/`에 보관한다.

프런트엔드는 `npm run build`로 `app.js`, `navigation.js`, `agent-status.js`, `run-graph.js`, `workspace.js`, `dispatch.js`, `pipeline-connections.js`, `run-chain.js`, `pipeline-review.js`, `pipeline-editor.js`를 생성한다. 이 파일들과 CSS를 서버 바이너리에 포함하므로 설치된 서버에 반영하려면 서버를 다시 빌드해야 한다. 웹 회귀 검증은 `npm run test:web`로 실행한다.

2026-10-09 운영 서버에 반영했다. HTTPS로 제공되는 앱·그래프·CSS가 로컬 검증본과 일치하고, 서버 readiness와 Agent 3대 재연결이 정상이다. 실제 `server-windows-ci` v8 실행 `e3724063`에서 4개 성공 작업의 의존 관계, 노드 선택, 공용 workspace 로그 추가 조회와 상태 새로고침을 확인했다. 배포 기록은 `.local/graph-deployment/verification.json`에 보관한다.

## 프로젝트 CI 설정

CI 설정 작업 화면(2026-10-09 운영 반영): 프로젝트 메뉴의 **CI 설정**에서 검색 목록 → 작업 그래프 → 선택 작업 속성을 한 화면으로 조회한다. **집중 모드**, 목록·속성 접기, 확대·축소·화면 맞춤·선택 작업 이동을 제공하며 좁은 화면에서는 세로로 배치한다. 검색 결과 밖의 현재 선택은 별도로 유지한다. `#ci-settings?project=<UUID>&pipeline=<UUID>&view=visual&job=build&q=server`로 프로젝트·파이프라인·작업·보기·검색어를 복원한다.

저장된 초안은 조회 모드로 열리며 읽기 권한 사용자도 그래프·속성과 TOML을 볼 수 있다. 편집 권한 사용자는 **편집**으로 시작하고 **편집 취소**로 마지막 저장 상태를 복원한다. 저장하지 않은 변경과 미적용 작업 이름은 이동·취소 전에 확인한다. 빈 초안의 예시는 편집에 진입한 뒤에만 채운다. 초안 저장·변경 비교·발행은 기존 절차와 권한을 유지한다.

**발행 연결**은 선택한 파이프라인의 고정 발행 버전에서 이어지는 후속 연결과 산출물 전달을 보여 준다. 프로젝트의 모든 파이프라인은 아래 **전체 파이프라인 뷰**에서 모아 본다. 가져온 프로젝트 구성 초안은 실행 정의 전환 전 상태임을 표시하고 기존 설정으로 연결한다. 공용 에이전트의 OS·아키텍처와 Docker 엔진 종류·가용성 기준은 유지한다. CI 설정 브라우저 검증과 화면은 `.local/ci-workspace-preview/`에 보관한다.

2026-10-09 12:48 KST CI 설정 화면과 실행 그래프 탐색 개선을 함께 배포했다. HTTPS 웹 파일의 검증본 일치, 실제 프로젝트의 조회·검색·선택 작업 복원, 활성 에이전트 2대 재연결과 Windows 인증 점검을 확인했다. 기존 초안·발행·실행, attempt 48개와 산출물 23개가 유지됐으며 `compose-demo-executor` 활성·`mac-mini` 비활성 상태도 보존됐다. DB 백업·이전 이미지·검증 기록은 `.local/ci-workspace-deployment/`에 보관한다.

## 프로젝트 전체 파이프라인 뷰

2026-10-09 13:39 KST 운영 반영. 프로젝트 메뉴의 **전체 파이프라인 뷰**, 파이프라인 목록과 CI 설정의 바로가기에서 연결 여부와 관계없이 프로젝트의 모든 파이프라인을 조회한다. 미발행 초안도 표시하며 독립 파이프라인은 격자로 배치한다.

- **발행된 실행 연결**: 모든 현재 발행 버전에서 시작해 성공 후 실행(`on_success`)의 정확한 고정 버전을 따라간다. 현재 버전과 고정된 과거 버전은 별도 노드다. 각 노드의 최근 실행은 해당 버전의 이력이며 연결 상세에서 산출물 이름과 CL 전달 기준을 확인한다.
- **가져온 구성 의존**: 가져온 구성 초안의 `needs`를 선행 → 후속 방향의 점선으로 표시한다. 구성이 없는 파이프라인도 포함한다. 이 관계를 조회하는 것만으로 실행이 예약되거나 생성되지는 않는다.
- 이름 검색은 전체 그래프를 유지하며 일치 노드를 강조한다. 검색 결과·연결 관계에서 노드를 선택하고 확대/축소·화면 맞춤·선택으로 이동을 사용할 수 있다. 선택 상세에서 CI 설정·파이프라인·해당 버전 연결·최근 실행으로 이동한다.

`#pipeline-graph?project=<UUID>&view=published&pipeline=<UUID>&version=1&q=server`로 선택과 검색어를 복원한다. 구성 보기는 `view=configuration`이며 실행 버전을 사용하지 않는다. 그래프 새로고침으로 갱신 시각을 확인하고, 일시 오류는 마지막 결과를 표시한다. 접근 권한 오류는 기존 결과를 지우며 화면 이동/보기 전환 후의 늦은 응답은 무시한다. 순환 또는 잘못된 연결은 모든 노드를 목록으로 표시한다.

API는 `GET /api/projects/{id}/graph?view=published|configuration`이다. 프로젝트 조회 권한을 확인하고 읽기 전용 DB 스냅샷에서 반환하며 스크립트·인증 정보·산출물 경로는 반환하지 않는다. 500개 파이프라인/1,000개 발행 버전을 넘으면 일부만 표시하지 않고 명시적으로 오류를 반환한다. 신규 migration·Agent 변경은 없다. 웹 테스트 44개, 그래프 API 통합 테스트 2개, TypeScript·Rust 빌드와 clippy가 통과했다. 로컬 미리보기 접근 거절 이후 배포된 운영 Chrome 화면에서 21개 노드·발행 연결 5개·구성 의존 2개, 검색·선택 URL/새로고침 복원·보기 전환·연결 상세·화면 맞춤을 확인했다. 모바일 배치와 인위적 오류 상태의 브라우저 검증은 남아 있다. 배포 백업·복구 스크립트·검증 기록과 화면은 `.local/project-graph-deployment/`에 있다.

## 발행 변경 비교와 실행 전 점검

파이프라인 설정에서 초안을 저장한 뒤 **변경 비교·발행**으로 이동한다. **변경 비교** 탭은 저장된 초안과 기준 발행 버전 또는 두 발행 버전을 비교한다. 작업은 이름별로 묶고 순서 변화는 따로 표시하며, 스크립트·환경 변수·OS/아키텍처/Docker 조건·소스·산출물·후속 고정 버전·배포 설정을 포함한다. JSON 키 순서·TOML 주석/표기만 다른 경우는 변경으로 취급하지 않는다. `#pipeline?pipeline=<UUID>&tab=review&from=0&to=draft`의 `from=0`은 첫 발행용 빈 기준이다. `to=2` 등으로 과거 발행 버전을 선택할 수 있고 URL·새로고침·뒤로 가기에서 선택이 유지된다.

편집 권한이 있는 사용자는 비교 화면에서 **검토한 초안 발행**을 누른다. 화면에 표시한 `generation`과 `base_version`을 보내므로 다른 사용자가 초안을 수정하거나 새 버전을 발행했다면 기존 서버 충돌 검사로 중단한다. 조회 오류가 나면 이전 결과로 발행할 수 없고, 저장하지 않은 TOML은 설정에서 먼저 저장해야 한다. 구성 import 초안은 기존 설정 변환 과정을 거쳐야 한다. 이미 발행된 버전과 과거 Run은 수정하지 않는다.

**파이프라인 실행**에서는 발행 버전·제출 CL·필요한 산출물 제공 실행 ID를 입력하고 **실행 전 점검**을 누른다. 결과에서 소스·작업 실행 조건·후속 고정 버전·산출물 입력/출력·배포 환경과 점검 시각을 확인한 뒤 **검토한 조건으로 큐에 등록**한다. 입력을 바꾸면 다시 점검해야 하며 늦게 도착한 이전 응답은 등록을 활성화하지 않는다. 실제 등록 요청의 중복 방지 ID는 동일 조건의 오류 재시도에 유지한다.

`GET /api/pipelines/{id}/review?from=1&to=draft`는 프로젝트 열람 권한, `GET /api/pipelines/{id}/preflight?version=2&changelist=12&source_run_id=<UUID>`는 실행 권한을 확인한다. 각각 읽기 전용 반복 읽기 스냅샷에서 조회하며 Run·승인·환경 잠금·산출물 입력 고정을 만들지 않는다. 산출물은 같은 프로젝트의 성공한 제공 실행인지, 최종 제출 CL과 현재 연결이 일치하는지, 성공 attempt의 검증된 파일이 하나이고 삭제 예약되지 않았는지 및 총 크기 제한을 확인한다. 실제 실행 등록에서 Perforce 서버 ID·소스 스냅샷·보관 상태를 다시 검사하고 입력을 고정한다.

에이전트는 프로젝트와 독립된 공용 자원이다. OS·아키텍처·pool·Docker 엔진·상태 보고 시각·프로토콜·사용 중 여부를 집계하고 배포 작업은 환경에 지정된 에이전트로 제한한다. 에이전트 이름·ID·인증 정보는 추가로 노출하지 않는다. 후보 부족과 배포 환경 사용 중 상태는 대기 가능성을 알리는 참고 정보이며 큐 등록 자체를 막지 않는다. 이 점검은 Perforce 접속이나 CL 존재 여부를 검증하지 않으며 실행 등록·발행·배정 시 기존 서버 검사가 최종 판단을 한다.

2026-10-09 구현·검증 후 사용자 배포 승인을 받아 운영에 반영했다. 추가 migration이나 Agent 업데이트는 없다. 통합 테스트 41개(실제 Perforce fixture가 필요한 기존 1개 제외), 웹 31개, 브라우저 10개 시나리오를 검증했다. 로컬 기록과 화면은 `.local/review-preview/`에, 운영 백업·복구 이미지 식별자·HTTPS/DB/로그 검증 및 실제 화면은 `.local/review-deployment/`에 보관한다. 실제 `server-staging-validation`의 v2→v3 비교, 선택 복원, CL 12의 산출물 제공 실행 점검 및 입력 변경 시 등록 비활성화를 확인했으며 검증 중 새 실행이나 발행은 하지 않았다.

## 파이프라인 시각적 편집

파이프라인 **설정 → 폼으로 편집**에서 현재 TOML을 검증한 뒤 초안의 작업·단계·의존 관계·스크립트·작업 디렉터리·제한 시간과 OS/아키텍처/Docker 엔진 조건을 편집한다. 그래프 노드나 작업 선택 목록으로 이동할 수 있고, 작업 추가·삭제·순서 변경과 명령 추가·삭제·순서 변경을 제공한다. 여러 줄 스크립트는 하나의 명령으로 보존한다. 작업 이름 변경은 해당 이름의 의존 참조와 배포 상태 확인 참조를 함께 갱신한다. 참조 중인 작업은 참조를 먼저 수정해야 삭제할 수 있다.

공유 workspace는 동일한 실행 조건, 동시 작업 수 1과 단계 순서가 필요하다. 그래프는 명시적 의존 관계와 앞 단계 완료 대기 관계(점선)를 함께 표시한다. 순환, 없는 작업, 잘못된 단계와 실행 조건은 오류로 표시하며 저장 시 서버가 기존 도메인·프로젝트 범위·후속 고정 버전·배포 참조 검사로 최종 검증한다. 작업별 workspace의 단계 이름은 표시용이며 실행 순서를 추가하지 않는다. 에이전트는 계속 프로젝트와 독립된 공용 자원이며 OS·아키텍처와 Docker 엔진 종류·가용성으로 배정한다.

폼은 전체 정의를 유지하고 지원하는 필드만 수정한다. 산출물·생성 파일 제출·후속 버전·배포 설정과 환경 변수는 읽기 전용으로 보여주고 TOML에서 편집한다. **TOML로 변환**과 폼 저장은 주석과 서식을 다시 작성하므로 **변환 전 TOML 내려받기**로 원문을 보관할 수 있다. **변환 전 TOML로 되돌리기**는 확인 후 폼 변경을 버린다. DB의 기존 초안 저장 방식은 정규화된 정의 저장이므로 주석을 영구 보관하지 않는다. 기존 구성 import 초안은 기존 변환 편집 경로를 유지한다.

**초안 저장·변경 비교**는 초안만 저장하고 비교 화면으로 이동한다. 발행은 비교 화면에서 별도로 검토한 세대와 기준 버전으로 수행한다. 저장 충돌·검증 오류에는 편집 내용이 유지되고, 미적용 이름 변경은 저장에서 누락되지 않도록 먼저 적용하도록 안내한다. 저장하지 않은 변경으로 화면 이동·새로고침·로그아웃할 때 확인하며 요청 처리 중 이동을 차단한다. 열람자에게 편집·저장 기능을 제공하지 않는다.

`POST /api/pipelines/{id}/definition-preview`는 편집 권한으로 구조화된 정의를 검증하고 TOML·실행 순서·해시를 반환한다. 초안·버전·실행·감사 기록을 쓰지 않는다. 실제 저장은 기존 세대/기준 버전 충돌 검사를 재사용한다. 추가 migration과 Agent 업데이트는 없다. 2026-10-09 사용자 배포 승인을 받아 운영에 반영했다. 실제 `server-staging-validation` v3/초안 g6에서 폼 전환·작업 선택·원래 해시를 유지하는 검증과 산출물/배포 설정 보존을 확인했다. 검증 중 초안 저장·발행·실행은 하지 않았다. 구현·검증 기록은 `.local/editor-preview/`, 운영 백업·복구 이미지·HTTPS/DB/에이전트 검사 및 실제 화면은 `.local/editor-deployment/`에 보관한다.

### 설정 진단 (CI-2, 로컬 구현)

설정 상단에 발행 버전·초안 세대·기준 버전·미저장 변경과 검사 상태를 함께 표시한다. **설정 검사**는 조회 중에는 저장된 초안, 편집 중에는 현재 입력을 검사한다. TOML 구문·정의·의존 관계 오류, 소스·후속 고정 버전·배포 참조 오류, 에이전트 가용성 안내를 구분한다. 정의 검증은 기존 규칙에서 처음 발견한 오류를 반환하며, 정의 오류를 고친 후 참조·가용성 검사를 진행한다.

**위치 보기**는 해당 작업과 폼 입력으로 이동하거나 TOML 구문 오류의 행·열을 선택한다. 폼에서 지원하지 않는 고급 설정은 TOML에서 수정한다. 존재하지 않는 선행 작업과 자기 참조는 폼에서도 제거할 수 있다. 오류가 있는 폼도 TOML로 변환할 수 있지만 저장 시에는 기존 검증을 통과해야 한다. 검사 뒤 입력·편집 모드가 바뀌면 이전 결과로 표시하고 오류 위치 이동을 비활성화한다. 늦은 응답과 화면을 떠난 뒤 도착한 응답은 반영하지 않는다.

`GET /api/pipelines/{id}/diagnostics`는 조회 권한, `POST`는 편집 권한을 요구한다. POST는 `generation`, `base_version`과 `toml` 또는 `definition` 중 하나를 받는다. 권한·초안·참조 조회는 동일한 읽기 전용 스냅샷에서 처리하며, 초안·발행·실행·감사 기록을 변경하지 않는다. 기존 저장·발행 경로의 충돌·검증 규칙을 유지하고 후속 파이프라인의 전이 관계·Perforce 연결·산출물 검사도 재사용한다. 가용성은 검사 시점의 안내이며 실행 배정을 보장하지 않는다.

2026-10-09 로컬 구현과 웹 47개·도메인 11개·서버 통합 10개 테스트, TypeScript 빌드·clippy를 통과했다. DB migration·Agent 변경 및 운영 배포는 없다. 앞선 로컬 브라우저 접근 권한 거절로 실제 화면·반응형·키보드 이동 검증은 남아 있다. 변경 전 파일과 검증 기록은 `.local/ci-diagnostics-preview/`에 보관한다.

## 파이프라인 연결 관계

파이프라인 상세의 **연결 관계**에서 선택한 발행 버전의 `on_success`를 따라 성공 후 실행될 파이프라인을 조회한다. 후속 버전은 발행 당시 지정한 버전에 고정되며 이후 현재 버전이 바뀌어도 그래프의 연결은 달라지지 않는다. 같은 파이프라인의 서로 다른 버전은 별도 노드로 표시한다. 목록의 **연결 보기**는 현재 발행 버전으로, 발행 이력의 **연결 보기**는 해당 버전으로 이동한다. `#pipeline?pipeline=<UUID>&tab=connections&version=1`로 특정 버전을 직접 열고 새로고침·뒤로 가기에서 복원할 수 있다.

노드를 선택하면 입력·보관 산출물 이름, Perforce 제출 CL 전달 조건, 배포 승인 여부와 해당 버전의 최근 실행을 확인한다. 최근 실행은 각 버전의 개별 이력이며 하나의 실제 실행 체인을 의미하지 않는다. 승인 대기·복구 확인이 필요한 실행은 배포 탭으로 연결한다. 넓은 그래프는 내부에서 스크롤하며 키보드로도 노드를 선택할 수 있다. 수동 새로고침 오류에는 마지막 결과와 확인 시각을 유지한다.

읽기 전용 `GET /api/pipelines/{id}/connections?version=<양의 정수>`는 매 요청마다 프로젝트 열람 권한을 검사하고 일관된 DB 스냅샷에서 최대 65개 발행 버전(시작 버전과 후속 64개)을 조회한다. 버전을 생략하면 현재 발행 버전을 사용하며 미발행 파이프라인은 빈 결과를 표시한다. 스크립트·게시 계정·산출물 경로는 응답하지 않는다.

2026-10-09 로컬 구현·검증 완료. 서버 통합 테스트 34개(실제 Perforce가 필요한 1개 제외), 웹 테스트 22개와 TypeScript·clippy·format 검사를 통과했다. 2026-10-09 운영 서버에 배포했으며 추가 DB migration과 Agent 업데이트는 필요하지 않다. 샘플 API 브라우저 검증과 화면은 `.local/pipeline-connections-preview/`에 보관한다.

## 실행 체인 추적

실행 상세의 **실행 체인**(`/#run?run=<UUID>&tab=chain`)은 해당 Run의 자동 선행 실행을 거슬러 올라간 뒤 실제 후속 실행과 다른 분기를 함께 표시한다. 서버에 기록된 `run_followups`의 실행 ID로만 연결하므로 같은 파이프라인·버전·CL의 다른 실행이 섞이지 않는다. 현재 발행 설정이 바뀌어도 Run에 고정된 정의를 사용한다. 수동 실행·재실행·복구 실행은 자동 후속 체인에 합치지 않으며, 산출물 제공 관계는 **산출물 → 제공 실행 보기**에서 별도로 조회한다.

아직 생성되지 않은 즉시 후속 실행은 점선으로 표시하고 **선행 성공 대기 / 후속 등록 대기 / 후속 등록 차단 / 실행 안 함**을 구분한다. 차단 사유는 기록된 원인을 보여주며 자동 재시도하지 않는 상태임을 안내한다. 실행이 생성된 후에만 실행 ID와 확정 CL을 표시한다. 노드에서 실행 그래프·배정 점검·산출물·고정 발행 버전 연결로 이동하며, 배포 승인 대기·복구 확인이 필요한 Run은 해당 배포 탭으로 연결한다. Perforce 제출 결과와 각 실행의 입력 CL도 확인할 수 있다.

자동 갱신은 보이는 탭에서 5초마다 수행하고 중지할 수 있다. 체인 전체에 진행 중인 실행이나 후속 등록 대기가 없어지면 자동으로 멈춘다. 새 후속 Run이 등록되면 그래프에 반영하며 기존 노드 선택·키보드 초점·스크롤 위치를 보존한다. 실패하면 마지막 결과와 확인 시각을 유지하며, 다른 화면으로 이동한 뒤 도착한 응답은 반영하지 않는다.

읽기 전용 `GET /api/runs/{id}/chain`은 요청마다 프로젝트 열람 권한을 검사하고 동일 프로젝트 내 실제 실행·미생성 후속을 합쳐 최대 65개 노드를 일관된 DB 스냅샷에서 조회한다. 범위를 넘거나 순환·버전 불일치가 있는 연결은 오류로 표시한다. 실행 생성·재시도·승인·배정은 수행하지 않는다.

2026-10-09 로컬 구현·검증 완료. 서버 통합 테스트 36개(실제 Perforce가 필요한 1개 제외), 웹 테스트 25개와 TypeScript·clippy·format 검사를 통과했다. 2026-10-09 운영 서버에 배포했다. 추가 DB migration이나 Agent 업데이트는 필요하지 않다. 샘플 API 브라우저 검증과 화면은 `.local/run-chain-preview/`에 보관한다.

### 연결 관계·실행 체인 운영 검증

2026-10-09 두 기능을 운영 서버에 함께 배포했다. 실제 로그인 화면에서 `server-windows-ci` v8의 고정 후속 Mac v1·Windows v2, 입력 산출물 세 개를 확인했다. Windows 후속 Run `3a0cf219`에서 선행 `e3724063`과 다른 분기 `70986d25`가 동일 체인으로 조회되고, 세 실행의 성공 및 자동 갱신 종료가 표시된다.

공개 HTTPS의 새 JS·CSS와 검증 소스가 일치하며 새 API의 비로그인 요청은 차단된다. 기존 migration 19개의 checksum, Agent ID·자격 증명·등록 상태, attempt 48건·산출물 23개와 두 배포 환경을 보존했다. `compose-demo-executor`와 Windows Agent는 온라인이고 `mac-mini`는 폐기 상태다. Windows 읽기 인증과 자동 빌드 v8/cursor CL 12, 테스트 서비스 ci-v1이 정상이다. 복구 이미지는 `p4forge-server:pre-chain-20261009-103737`, 접근 제한된 DB 백업과 검증·화면은 `.local/chain-deployment/`에 보관한다.

## 개발 환경

Docker Compose, Rust 1.99+, Node 26+, `p4` CLI가 필요하다.

```sh
npm ci --ignore-scripts
npm run build
./scripts/dev-perforce.sh
./scripts/verify-perforce.sh
cargo build --locked --workspace
```

전용 `p4forge` Compose 프로젝트에 PostgreSQL 18과 Perforce 2026.1을 구성한다. 공식 Perforce 바이너리를 내려받아 SHA-256을 확인하고 Docker 이미지를 빌드한다. 무료 사용 범위는 최대 5 사용자·20 workspace이며 데모는 관리자·빌드 계정 2명을 만든다. [Perforce 무료 사용 범위](https://www.perforce.com/products/p4/free-version-control)

Perforce 2026.1의 인증 기본값을 유지한다. 비밀번호는 컨테이너 볼륨 `/data/.p4forge-credentials/`에서 생성하고 최초 비밀번호 변경을 완료한다. 호스트의 `.local/p4tickets`, `.local/p4-build-password`는 권한 `600`으로 보관한다. DB 포트는 loopback에만 열고 P4의 수신 주소와 포트는 `P4FORGE_P4_BIND`·`P4FORGE_P4_PORT`로 설정한다. 기본값은 `127.0.0.1:16660`이다. 이 스크립트는 **전용 개발 컨테이너에서만** 사용하며 기존 운영 Perforce에는 적용하지 않는다. 반복 실행은 데모를 다시 제출하지 않고 ticket을 갱신한다.

| 항목 | 개발 값 |
| --- | --- |
| PostgreSQL | `127.0.0.1:15432`, database/user `p4forge` |
| Perforce | `p4forge.zenogrid.co.kr:1666`, user `p4forge` |
| Demo stream | `//streams/main` |
| Root stream | `//streams/root`, CL `6` · 688 files |
| Submitted CL | `2`: first revision, `3`: second revision |

현재 설치는 `.env`에서 다음과 같이 설정한다. 도메인은 해당 사내 IP를 가리키며, 호스트·Server·Agent에서 P4PORT를 `p4forge.zenogrid.co.kr:1666`으로 사용한다. 개발·검증 스크립트는 실행 중인 컨테이너의 공개 포트를 조회하며 `P4PORT` 환경 변수로 접속 주소를 지정할 수도 있다.

```dotenv
P4FORGE_P4_BIND=10.48.100.171
P4FORGE_P4_PORT=1666
```

2026-10-08에 `local-p4` 연결로 위 주소를 등록하고, 관리 화면의 연결 확인·Server/Agent 인증·CL 2 checkout을 검증했다.

```sh
cargo run --locked -p p4forge-server -- validate examples/ci.toml
cargo run --locked -p p4forge-server -- perforce-check \
  --port p4forge.zenogrid.co.kr:1666 --stream //streams/main --changelist 2 \
  --tickets-file .local/p4tickets
```

Stream spec은 `stream@CL`, client view는 `client -o -S ... -c CL`로 조회하여 Run에 보존한다. Import·components·ChangeView는 파일 revision manifest 구현 전까지 거절한다. Classic root는 API에서 `{"kind":"classic","view":["//depot/path/..."]}`로 등록한다. Agent는 Perforce 서버 identity와 저장된 view를 확인하고 전용 client로 `sync @CL`을 수행한다. Classic view의 각 depot 경로는 `root0/`, `root1/` 아래에 배치된다.

**Perforce View 직접 지정:** 프로젝트 → 소스 루트 → 소스 루트 추가에서 소스 종류를 `Perforce View`로 선택한다. 포함 경로 뒤에 작업 폴더 기준 목적지를 적고, 제외 경로는 `-`로 시작한다.

```text
//stream/root/Server/... ...
-//stream/root/Client/...
```

오른쪽 `...`는 `Server` 안의 파일을 작업 폴더 최상위에 배치한다. `server/...`를 쓰면 작업 폴더의 `server/` 아래에 배치한다. 위에서 아래로 적용하며 뒤의 규칙이 우선한다. 오른쪽을 생략한 제외 규칙은 앞선 포함 매핑에서 해당 depot 경로를 제외한다. 포함 범위 밖의 제외 경로는 추가 파일을 가져오지 않는다. 공백이 있는 경로는 큰따옴표로 감싼다. 현재 지원 범위는 `/...` 재귀 경로이며 `*`, `+`, `&`, revision 지정과 workspace 밖의 경로는 거절한다. 예시의 `//stream`은 실제 depot 이름으로 바꾼다. 현재 개발 서버는 `//streams/root`를 사용한다.

DB에는 `{"kind":"view","view":["//stream/root/Server/... ...","-//stream/root/Client/..."]}`로 저장한다. 실행에는 입력 규칙과 해석된 client View를 함께 고정하고, Agent가 checkout할 때 같은 매핑인지 검증한다. 새 View는 protocol 4 Agent부터 배정한다. Windows는 최신 ZIP의 `Update-ThisMachine.cmd`, Mac은 최신 패키지의 `update.sh`로 업데이트한다. 기존 Stream/Classic 실행은 계속 지원한다. View 기반 generated publication은 아직 지원하지 않는다.

검증: parser/경로 제한 단위 테스트, 격리 DB의 View API roundtrip·잘못된 입력 거절·구버전 Agent 배정 차단, 실제 `//streams/root` CL 6의 최상위/하위/공백 경로 매핑과 Client 제외 checkout을 통과했다.

## Google Workspace OIDC

`.env.example`의 항목을 **기존 `.env`에 보충**한다. 기존 Cloudflare 값을 덮어쓰지 않는다. Google OAuth Web application client에 다음 redirect URI를 정확히 등록한다.

```text
https://p4forge.zenogrid.co.kr:8444/auth/google/callback
```

Google Cloud Console → Google Auth Platform → Clients에서 `.env`의 client ID에 해당하는 Web application을 열어 **승인된 리디렉션 URI**에 위 주소를 추가하고 저장한다. 포트 `8444`를 포함하고 callback 뒤에 `/`를 붙이지 않는다. 기존 다른 서비스의 URI는 유지한다. `redirect_uri_mismatch`는 해당 client에 요청한 URI가 등록되어 있지 않거나 정확히 일치하지 않을 때 발생한다. [Google OAuth 오류 설명](https://developers.google.com/identity/protocols/oauth2/web-server#authorization-errors-redirect-uri-mismatch)

```dotenv
P4FORGE_PUBLIC_URL=https://p4forge.zenogrid.co.kr:8444/
P4FORGE_BOOTSTRAP_ADMIN=law80@zenogrid.co.kr
GOOGLE_CLIENT_ID=your-client-id.apps.googleusercontent.com
GOOGLE_CLIENT_SECRET=your-secret
```

`zenogrid.co.kr` 계정만 허용한다. Google JWKS/RS256 서명·issuer·audience·expiry·nonce·verified email·`hd`·`azp`를 검사하고 PKCE와 브라우저에 묶인 일회용 state를 사용한다. Google `sub`로 사용자를 식별한다. 최초 관리자 지정은 해당 이메일의 **첫 사용자 생성 시** 적용한다. 이후 로그인은 관리자 권한을 덮어쓰지 않는다.

세션은 DB에 해시만 저장하고 8시간 후 만료한다. HTTPS에서 Secure·HttpOnly·SameSite=Lax 쿠키를 사용하며 변경 API는 세션에 묶인 CSRF cookie/header를 검사한다. 비활성 사용자와 만료·삭제된 세션은 거절한다. Google 설정이 없으면 로그인은 `503`, 보호 API는 `401`이며 인증 우회는 없다.

사용자가 먼저 로그인한 뒤 전역 관리자가 프로젝트 역할을 지정한다. Viewer는 조회, Operator는 실행 등록·취소, Editor는 설정 편집·발행, 프로젝트 Admin은 역할 관리가 가능하다. 전역 관리자만 프로젝트 생성과 Perforce 연결·소스 할당을 할 수 있다.

## 서버 실행

로컬 HTTP 확인용 명령은 다음과 같다. Rust CLI는 `.env`를 자동으로 읽지 않는다. 로컬 Google 로그인에는 별도의 loopback redirect URI 등록과 Google 환경 변수 설정이 필요하다.

```sh
DATABASE_URL=postgres://p4forge:p4forge-local-only@127.0.0.1:15432/p4forge \
P4FORGE_P4_TICKETS_FILE=.local/p4tickets \
cargo run --locked -p p4forge-server -- serve
```

Docker 서버는 `.env`의 Google 설정과 공개 URL을 읽는다. 현재 설치의 연결 등록에서는 P4PORT를 **`p4forge.zenogrid.co.kr:1666`**으로 지정한다. 네트워크가 없는 일회성 `ticket-init` 컨테이너가 ticket을 서버 UID 10002 소유·권한 600으로 복사하고, 서버는 해당 볼륨을 read-only로 읽는다. ticket을 갱신하면 `ticket-init`을 `--force-recreate`로 다시 생성하고 서버를 재시작한다.

```sh
docker compose -f compose.yaml -f compose.server.yaml --profile server up -d --build --wait
```

UI는 `http://127.0.0.1:8088/`, liveness는 `/healthz`, DB readiness는 `/readyz`다. 로그인 버튼은 `P4FORGE_PUBLIC_URL`의 인증 경로로 이동한다. 공개 URL이 HTTPS이면 Secure 쿠키를 사용하는 실제 로그인과 관리 작업은 HTTPS로 접속한다. 프로젝트 → 연결 → source root → pipeline을 만들고 설정의 `source_root_id`에 root UUID를 지정한다. 초안 저장 → 검증 → 발행 → submitted CL 등록 순서로 진행한다.

## root 프로젝트 설정

사용자가 지정한 `~/lorehub/root/.lore-ci.toml`을 기준으로 [root.p4forge.toml](examples/root.p4forge.toml)에 7개 파이프라인·12개 작업을 변환하고, 2026-10-08에 `root` 프로젝트의 DB 초안으로 반영했다. 이후 `root-source` (`//streams/root`)를 7개 설정에 연결했다. 입력 파일과 DB 해시의 일치도 확인했다. 이 파일은 가져오기 자료이며 DB의 원본은 `configuration_drafts.definition` JSONB다. 최초 입력 TOML과 SHA-256은 `configuration_imports`에 보관한다. UI의 **프로젝트 TOML 가져오기**로 저장하고 파이프라인별 **TOML 편집**에서 수정한다. 세대 번호로 동시 편집을 검사하며 같은 파일 재요청은 중복 생성하지 않는다. 다른 파일에 기존 이름이 포함되면 전체 가져오기를 취소한다.

| 파이프라인 | 실행 OS | 단계 |
| --- | --- | --- |
| data-table-generate | macOS / aarch64 | generate → publish |
| client-editor-build | Windows / x86_64 | build |
| client-windows-package | Windows / x86_64 | package |
| client-android-googleplay-package | macOS / aarch64 | package |
| client-ios-package | macOS / aarch64 | package |
| server | macOS / aarch64 | check → test → build → publish |
| server-windows-build | Windows / x86_64 | build → publish |

`category`, `changes`, 작업 위치, stage 순서, timeout, 두 서버 파이프라인의 `data-table-generate` 의존성을 보존한다. `sparse_view`는 Perforce의 실제 client view에 연결할 `checkout_view` 요구 사항으로 변환한다. macOS는 현재 장비에 맞춰 aarch64, Windows는 x86_64로 지정했다. 설정은 DB에서 관리하므로 변경 경로의 `.lore-ci.toml` 항목은 제거했다.

Lore 인증 변수와 stage/commit/push 명령은 실행 설정에서 제거했다. 이미지 태그의 `LORE_REVISION`은 `P4FORGE_CHANGELIST`로 바꿨으며 ECR 게시 명령은 원본대로 유지한다. 생성 파일과 Windows 실행 파일의 게시는 [p4-publish.py](scripts/p4-publish.py)가 명시된 생성 디렉터리에만 `p4 reconcile -a -e -d -c CL`을 적용한 뒤 `p4 submit -c CL`로 처리한다. 기존 열린 파일이 있으면 중단하고, 변경이 없으면 빈 changelist만 삭제한다. 실패 정리는 자신이 만든 pending changelist에 한정하며 `revert -k`로 로컬 생성 파일을 보존한다. 이 도구는 Python 3.9+와 p4 CLI가 필요하다.

게시용 전용 runner에는 `P4FORGE_PUBLISH_TOOL`(설치한 도구 경로), `P4PORT`, `P4USER`, `P4CLIENT`, `P4TICKETS`, 고정된 `P4FORGE_CHANGELIST`가 필요하다. 생성 경로에 한정된 쓰기 계정을 별도로 구성해야 한다. 현재 읽기 전용 Linux Agent에 게시 자격 증명을 제공하지 않는다.

**이 7개 구성은 실행 준비 중인 DB 초안이다.** 실제 소스 제출과 root 연결, 네이티브 실행 정의의 공용 workspace·stage barrier와 macOS Agent를 구현·검증했다. Windows 실제 호스트 검증, 생성 파일 제출 후 downstream에 새 changelist 전달, checkout view 연결과 구성의 실행 버전 변환이 남아 있다. `changes`의 자동 트리거 및 ECR secret 전달도 별도 구현 대상이다. 준비 전에는 UI와 API 모두 이 구성의 발행·실행을 막는다. 기존 단일-root 실행 정의는 계속 사용할 수 있다.

```sh
python3 scripts/test_p4_publish.py
cargo run --locked -p p4forge-domain --example convert_root -- \
  "$HOME/lorehub/root/.lore-ci.toml" examples/root.p4forge.toml
```

## Linux Docker Agent

관리 → **에이전트 → Agent 등록**에서 이름, OS, 아키텍처와 Pool을 지정한다. 인증 파일은 최초 응답에서만 내려받을 수 있다. 다운로드한 파일을 `.local/agent-token`에 저장하고 다음 명령으로 실행한다.

```sh
chmod 600 .local/agent-token
docker compose -f compose.yaml -f compose.server.yaml -f compose.agent.yaml \
  --profile tls --profile agent up -d --build --wait
```

Agent는 모든 프로젝트에서 pool·OS·arch와 선택한 Docker 조건이 일치하는 Job을 가져간다. Agent당 1 slot이며 `max_parallel_jobs`는 Run 안에서 동시에 실행할 Job 수다. 여러 Agent에는 각각 다른 인증 파일과 state 볼륨을 제공한다. 등록된 architecture가 실제 컨테이너와 다르면 claim을 거절한다. 기본 이미지는 `sh`와 `p4`를 포함하며 실제 프로젝트의 compiler·SDK는 별도 Agent 이미지에 설치한다.

관리자용 로컬 CLI로도 등록할 수 있다. DB 관리 권한이 필요한 운영 명령이며 token 파일은 새로 생성하고 콘솔에 token을 출력하지 않는다.

```sh
cargo run --locked -p p4forge-server -- provision-agent \
  --database-url postgres://p4forge:p4forge-local-only@127.0.0.1:15432/p4forge \
  --name linux-agent --pool default --arch aarch64 \
  --token-file .local/agent-token
```

기본 `workspace = "job"`에서는 각 attempt에 별도의 client와 workspace가 생긴다. 성공·실패·취소·timeout 뒤에 프로세스와 client를 정리한 다음 완료를 보고한다. container supervisor는 root, 빌드는 UID/GID 10003으로 실행한다. 빌드 환경에는 Agent token·P4 ticket·Google secret을 전달하지 않으며 credential 파일의 접근도 분리한다. Docker socket, host PID namespace, privileged mode를 사용하지 않는다. Agent 실행은 전용 Linux Docker 컨테이너를 기준으로 한다.

heartbeat는 10초, lease는 60초다. HTTP 요청과 별도로 동작하는 watchdog이 갱신 실패·timeout·종료 신호를 감시한다. lease가 만료되거나 Agent가 폐기되면 서버는 attempt를 `lost`로 기록하고 **자동 재실행하지 않는다**. 오래된 attempt의 로그·완료 보고는 거절한다. 취소 요청은 실행 중 프로세스의 정리가 확인될 때까지 `running`과 `cancel_requested`로 표시한다. 연결이 끊겨 종료를 확인할 수 없으면 `lost`/`failed`로 남는다.

Agent의 **새 작업 중지**는 현재 작업을 끝낸 뒤 새 배정을 멈춘다. **자격 증명 폐기**는 영구적이며 새 Agent 등록으로 대체한다. Agent state는 컨테이너 재시작 시 이전 client 정리를 위해 유지한다. 해당 state를 공유하는 중복 프로세스는 파일 잠금으로 거절한다.

실행 ID를 눌러 Job 상태·의존 관계·Agent·exit code와 로그를 조회한다. stdout/stderr는 합계 1 MiB까지 표시하고 이후 출력은 계속 읽되 저장하지 않는다. 로그는 sequence로 중복 전송을 구분하며 서버 상한은 attempt당 4 MiB·4096 chunk다. Preview에서는 제한된 로그를 PostgreSQL에 저장한다. 파일 기반 로그 저장소·보존 정책·artifact 전달은 후속 단계다.

## Perforce root 이관과 공용 workspace

2026-10-08에 사용자 승인으로 `~/lorehub/root`의 688개 파일(45,262,412 bytes)을 **`//streams/root` CL 6**에 제출했다. 원본 폴더는 변경하지 않았다. `.lore`, `.idea`, `target`, `.env` 3개, 이전 Lore CI 설정·안내 파일을 제외했다. 읽기 계정으로 새 client에 다시 sync한 뒤 688개 SHA-256을 모두 비교했다. 제외 목록과 파일 manifest는 `.local/root-import/manifest.json`에 있다. `root-source` 연결을 DB에 등록하고 프로젝트 설정 전체에 적용한다.

공용 workspace는 네이티브 실행 TOML에 다음을 지정한다. 모든 Job은 같은 pool·OS·arch를 사용하고 `max_parallel_jobs = 1`이어야 한다. 이전 stage의 모든 Job 성공을 의존 edge로 저장하며, 같은 stage에서는 `needs`와 설정 순서로 순차 실행한다. 한 attempt가 모든 Job을 예약하고 같은 source/home/tmp를 사용한다. 각 Job 상태·exit code·시작/종료 시간은 별도로 기록한다. 로그는 attempt 단위로 모으며 작업 이름과 stage를 구분한다.

```toml
workspace = "pipeline"
stages = ["generate", "test", "publish"]
max_parallel_jobs = 1
```

Job별 timeout과 전체 lease watchdog을 함께 적용한다. 성공·실패·취소·lease 만료·재시작 시 프로세스와 workspace/client를 정리한다. 중간 단계 이후 lease가 끊겨도 다른 Agent에서 뒤 단계만 재실행하지 않는다. protocol version 2를 알리지 않는 이전 Agent에는 공용 workspace를 배정하지 않는다. Docker 통합 테스트에서 공용 파일·HOME 전달, 단계 순서, 실패, timeout, 취소, 강제 종료 후 복구를 검증했다.

## macOS Agent 설치

[설치기](deploy/macos/install.sh)와 [검증 설정](deploy/macos/macos-smoke.toml)을 제공한다. `sh scripts/package-macos-agent.sh`로 native 실행 파일·launchd plist·설치 안내·SHA-256 manifest를 묶는다. 설치기는 새 `/Library/P4Forge`와 숨김 비로그인 계정 `_p4forge`(UID/GID 10003)를 생성한다. 기존 계정이나 설치 경로가 있으면 덮어쓰지 않는다. Rust toolchain은 로그인 사용자의 HOME에 접근하지 않도록 설치 경로에 별도로 복사한다.

Supervisor는 root, 빌드 스크립트는 전용 계정으로 실행한다. Agent token과 P4 ticket은 root 소유 0600 파일이며 빌드 환경에 전달하지 않는다. macOS 전용 EOF watchdog은 Supervisor와 state lock을 공유하고 비정상 종료 뒤에도 빌드 UID 프로세스 정리가 끝날 때까지 잠금을 유지한다. 종료 helper는 UID를 내린 후 커널의 동일 UID 신호 제한을 사용해 `setsid`로 분리된 프로세스도 종료한다. [Apple kill(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/kill.2.html)

```sh
sudo sh /absolute/unpacked-package/install.sh /absolute/agent-token /absolute/p4tickets /absolute/p4 /absolute/rust-toolchain
sudo launchctl print system/kr.co.zenogrid.p4forge.agent
sudo tail -n 30 /Library/P4Forge/logs/agent.log
```

위 설치 명령은 **패키지 압축을 푼 디렉터리의 `install.sh`**로 실행한다. 소스 트리에는 실행 바이너리·SHA256SUMS가 없으므로 소스 경로를 직접 설치 대상으로 사용하지 않는다.

2026-10-08에 migration 0005와 macOS 등록 UI를 배포하고 `root` 프로젝트의 `mac-mini` Agent를 설치했다. `macos-validation` CL 6에서 정상 실행, source/HOME 유지, credential 읽기 차단, detached 자식 정리, timeout, 실행 중 취소, SIGKILL 후 launchd 재시작과 후속 정상 실행을 검증했다. 실패·취소 이전 성공 단계는 보존하며, 강제 종료된 단계는 lease 만료 후 lost로 확정하고 뒤 단계는 건너뛴다.

최초 설치에서 macOS POSIX broadcast signal이 정리 helper 자신을 종료하던 문제는 개별 UID 프로세스 열거·자기 PID 제외 방식으로 수정했다. 패키지의 `test-process-cleanup.py`는 root로 서비스 시작 전에 빈 UID 정리, detached 프로세스 종료, watchdog EOF와 lock 유지를 실제 검사한다. 기존 설치 갱신은 작업을 drain한 뒤 패키지의 `sudo sh update.sh`로 수행한다. 계정·토큰·Rust toolchain은 유지한다.

실제 원본 명령도 [root-data-validation.toml](examples/root-data-validation.toml)로 검증했다. `data-table-validation` v1 / CL 6에서 `cargo run -p data_table_gen_v2 --manifest-path GameDesign/Tools/Cargo.toml -- --sqlite --rust`가 성공했다. 최초 의존성 컴파일은 5분 50초, 생성은 약 1.2초였으며 SQLite 무결성 검사, 테이블 43개, Rust 출력 37개를 확인했다. `.env`는 이관하지 않고 기존 파일에서 확인한 비밀이 아닌 `VERSION=0.0.1`, `VCS=Perforce`와 대상 stream만 실행 환경에 지정했다. 생성 도구가 기존 추적 파일을 갱신할 수 있도록 전용 checkout의 Tools/Sqlite 경로를 쓰기 가능하게 했다. 생성 결과는 검증 후 임시 workspace와 함께 정리했으며 Perforce submit·산출물 보관은 아직 실행하지 않았다.

## Windows Agent 설치 패키지

[Windows ZIP 다운로드](https://p4forge.zenogrid.co.kr:8444/downloads/p4forge-windows-x64-preview.zip) · [SHA-256](https://p4forge.zenogrid.co.kr:8444/downloads/p4forge-windows-x64-preview.zip.sha256). 서버에 연결되는 사내망/VPN에서 받는다. ZIP을 풀고 `Diagnose.cmd`를 실행하면 설치 변경 없이 DNS·HTTPS·Perforce 포트·예약 작업·파일 존재 여부를 검사하고 `windows-host-report.json`을 생성한다. 자격 증명 내용과 빌드 로그는 수집하지 않는다. 보호된 설치 파일 확인은 관리자 실행이 필요할 수 있다.

다운로드 서비스는 `.local/downloads`에 복사한 Windows ZIP과 체크섬 두 파일만 제공한다. 패키지 생성 후 이 두 파일을 갱신한다. Agent token과 Perforce ticket은 다운로드 경로에 두지 않는다.

`scripts/package-windows-enrollment.py`는 별도로 승인된 `DESKTOP-6N6U52L` 설치를 위해 기존 Agent token과 읽기 전용 P4 ticket을 `.local/enrollment`의 0600 ZIP에 묶는다. 이는 공개 preview ZIP과 다르다. `Setup.ps1`은 대상 PC와 ticket 기한을 검사하고 기존 `p4.exe`를 찾아 관리자 권한을 요청한 뒤 설치·시작한다. 결과는 `installation-result.txt`·`perforce-install-check.json`·`windows-host-report.json`에 남긴다.

전용 ZIP의 웹 전달은 인증 정보와 수신 경로에 대한 사용자 승인 후에만 배포한다. 준비된 `/downloads/p4forge-desktop-6n6u52l-setup.zip` 경로는 기존 `/api/agents`의 Google 세션·전역 관리자 검사를 매 요청마다 거치며, ZIP은 공개 디렉터리와 분리된 볼륨에서 0400으로 읽는다. 비로그인·일반 사용자·인증 서버 오류는 차단한다. `python3 scripts/test-download-auth.py`로 이 계약을 자격 증명 없는 격리 환경에서 확인한다. 설치 확인 후 전용 다운로드 사본은 제거하고, Windows에 받은 ZIP과 압축을 푼 인증 파일도 정리한다.

[설치 설명](deploy/windows/README.txt), [설치 스크립트](deploy/windows/Install.ps1), [Windows smoke 설정](deploy/windows/windows-smoke.toml)을 제공한다. `sh scripts/package-windows-agent.sh`로 `.local/releases/p4forge-windows-x64-preview.zip`과 SHA-256 파일을 생성한다. Windows x64 실행 파일, 설치·점검 스크립트, 안내서, smoke TOML을 포함하며 자격 증명과 p4.exe는 포함하지 않는다.

관리 화면의 **Agent 등록**에서 Windows / x86_64 / Pool을 선택해 인증 파일을 발급받는다. Windows 관리자 PowerShell에서 다음을 실행한다. Rust·Unity·C++ SDK는 실제 프로젝트 요구 사항에 따라 별도로 설치한다.

```powershell
.\Install.ps1 -AgentTokenFile C:\Private\agent-token -P4TicketsFile C:\Private\p4tickets -P4Exe C:\Tools\p4.exe
C:\P4Forge\Test-Installation.ps1
Start-ScheduledTask -TaskName P4Forge-Agent
```

설치기는 전용 표준 계정 `p4forge-build`, SYSTEM으로 실행하는 시작 예약 작업, SYSTEM/Administrators 전용 credential/state ACL을 구성한다. 실행 시 build 계정의 관리자 그룹 가입 여부와 인증 파일 읽기 가능 여부를 검사한다. 빌드 프로세스는 별도 계정으로 시작하며, 생성 시점부터 `KILL_ON_JOB_CLOSE`가 설정된 Job Object에 속한다. 프로세스에 전달하는 환경과 상속 handle도 제한한다. [Microsoft CreateProcessAsUser](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw), [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)

설치 대상은 사용자가 지정한 **DESKTOP-6N6U52L**이다. 전용 Agent `desktop-6n6u52l`은 당시 `root` 프로젝트에 등록했다. 공용 에이전트 전환은 위 로컬 구현 절을 따른다. 2026-10-08 사용자 실행 결과에서 설치와 예약 작업 Running을 확인했고, 서버의 인증 접속도 확인했다. 현재 Mac 도구에는 Windows 원격 실행이 노출되지 않으므로 설치·업데이트는 사용자가 ZIP을 실행하고 결과를 전달하는 방식으로 진행한다.

**Windows 설치·연결 및 진단 smoke는 통과했고, Agent 자체 경로 수정본의 적용·취소·재시작 검증은 남아 있다.** 최초 실제 실행은 한글 파일명에서 실패했다. 사용자 승인 후 Perforce를 Unicode 모드로 전환하고 DB 연결 charset을 `utf8`로 변경해 해결했다. 복구용 체크포인트·depot·설정 백업은 `.local/backups/perforce-before-unicode-20261008/perforce-restore.tgz`에 보관한다. 전환은 임시 복사본에서 먼저 검증했으며, 비 Unicode 복귀는 백업 복원이 필요하다. 기존 Mac의 `data-table-consume` CL 9 재검증도 성공했다.

다음 실패는 Rust가 정규화한 `\\?\C:\...` 경로를 Windows PowerShell 5.1의 작업 디렉터리로 전달한 데서 발생했다. `windows-validation` v3은 스크립트 안에서 경로를 보정해 `generate`·`consume` 두 단계, 전용 build 계정, 공유 workspace와 credential 환경변수 미전달을 검증했다(실행 `022b02cf-b38e-4baf-b120-b0f238dc3c9b`). Agent 수정본은 내부 containment 검사 후 같은 디렉터리를 가리키는 일반 DOS/UNC 경로를 shell에 전달한다. 이후 Agent 0.1.0 수정본과 0.1.1 셀프 패치에서 스크립트 경로 보정 없는 Windows/View 검증을 통과했다.

기존 설치 업데이트는 관리 화면에서 Agent의 새 작업을 중지하고 현재 작업이 끝난 뒤, 최신 공개 ZIP의 `Update-ThisMachine.cmd`를 실행한다. 업데이트기는 패키지 해시를 확인하고 기존 실행 파일을 백업한 다음 Agent와 진단 스크립트만 교체한다. 계정·인증 파일·예약 작업 설정은 유지하며, 실패하면 이전 파일 복구를 시도한다. `update-result.txt`와 `windows-host-report.json`으로 결과를 확인한 뒤 새 작업을 재개한다.

현재 이관한 Client CI 스크립트는 Unity 프로젝트나 Editor가 없으면 예제 메시지를 출력하고 exit 0하는 내용을 포함하므로, 이를 실제 클라이언트 패키징 성공으로 취급하면 안 된다. 실제 Client 프로젝트/빌드 명령 연결이 필요하다.

## SSL / self-hosted

공개 주소는 **`https://p4forge.zenogrid.co.kr:8444/`**다. Traefik 3.7.14가 Cloudflare DNS-01로 Let's Encrypt 인증서를 발급·갱신한다. 서버·DB·P4는 자체 인프라에서 운영하며 프록시에 Docker socket을 제공하지 않는다.

```dotenv
ACME_EMAIL=law80@zenogrid.co.kr
CF_ZONE_ID=acme-challenge-zone-id
CF_API_TOKEN=zone-read-and-dns-edit-token
P4FORGE_TLS_BIND=127.0.0.1
```

`CF_ZONE_ID`는 ACME challenge 대상 zone의 사전 읽기 검증에 사용한다. 기존 DNS는 `_acme-challenge.p4forge.zenogrid.co.kr` → `_acme-challenge.ytlaw80.com` CNAME 위임을 사용하므로 `ytlaw80.com`의 zone/token으로 발급할 수 있다. ACME provider는 `CF_API_TOKEN`을 `CF_DNS_API_TOKEN`으로 전달받아 위임 대상 zone의 challenge TXT를 생성·삭제한다. 토큰은 TLS 컨테이너에만 전달한다. 해당 zone의 **Zone Read·DNS Edit** 권한이 필요하다. [Lego Cloudflare 설정](https://go-acme.github.io/lego/dns/cloudflare/), [Traefik ACME](https://doc.traefik.io/traefik/reference/install-configuration/tls/certificate-resolvers/acme/)

```sh
./scripts/start-tls.sh
curl --fail https://p4forge.zenogrid.co.kr:8444/readyz
```

8444는 Cloudflare 일반 HTTPS 프록시 지원 포트가 아니므로 해당 레코드는 **DNS only**로 설정한다. A/AAAA/CNAME은 self-hosted 서버를 가리켜야 하며 방화벽/NAT에서 8444를 연결한다. 개발 기본값은 loopback이다. 전용 서버에서는 `P4FORGE_TLS_BIND`를 실제 수신 IP로 지정한다. [Cloudflare 지원 포트](https://developers.cloudflare.com/fundamentals/reference/network-ports/)

인증서와 ACME 개인키는 `certificates` Docker 볼륨에 보관한다. 테스트 발급은 `ACME_CA_SERVER=https://acme-staging-v02.api.letsencrypt.org/directory`와 별도 프로젝트·인증서 볼륨으로 운영 인증서와 분리한다.

2026-10-08에는 기존 CNAME 위임과 `.env`의 Cloudflare 자격 증명으로 Let's Encrypt 인증서를 발급했다. Linux Agent preview 서버와 migration 0002를 반영하고 서버·Agent 이미지에서 신뢰된 HTTPS 연결과 `/readyz` 성공을 확인했다. Google Cloud Console 수정 후 `redirect_uri_mismatch`가 해소됐으며, `law80@zenogrid.co.kr`의 실제 Google Workspace 로그인과 관리자 화면 진입을 확인했다. DB에서도 활성 관리자 계정과 유효한 로그인 세션을 확인했다.

운영 DB는 개발 비밀번호를 교체하고 `P4FORGE_DB_PASSWORD`와 URL-encoded `P4FORGE_DOCKER_DATABASE_URL`을 함께 설정한다. 이미 초기화한 PostgreSQL 볼륨의 비밀번호는 환경 변수 변경만으로 바뀌지 않는다. 운영 P4에는 별도 읽기 계정·SSL trust·ticket 갱신 절차를 사용한다. 현재 개발 P4의 TCP 1666은 지정한 사내 IP에서 수신한다. 웹 UI의 HTTPS 8444와 Perforce 접속 포트는 별개다.

## Agent 셀프 패치

전역 관리자는 **관리 → 에이전트 → 관리 → 패치 버전 → 셀프 패치 적용**으로
Mac·Windows Agent를 업데이트한다. 실행 중인 빌드가 끝날 때까지 기다린 후
동일한 HTTPS 서버에서 해당 Agent에 배정된 바이너리만 내려받는다. 고정된
SHA-256·크기·플랫폼 호환성을 검사하고, 별도 updater 프로세스가 이전 바이너리를
백업한 뒤 Agent를 재시작한다. 새 Agent가 90초 안에 서버와 정상 통신하지 못하면
이전 바이너리로 복구한다. 최종 결과를 서버가 확인하면 패치 요청 전 drain 상태로
돌아간다. 적용 전 요청은 취소할 수 있으며 패치 중에는 수동 작업 재개가 차단된다.

기존 설치에는 최초 한 번 최신 Windows `Update-ThisMachine.cmd` 또는 macOS
`sudo sh update.sh` 실행이 필요하다. 이때 `P4Forge-Updater` 예약 작업 또는
`kr.co.zenogrid.p4forge.updater` launchd 서비스가 설치된다. 이후 바이너리 패치는
화면에서 수행한다. 서버, Perforce ticket, 계정, SDK, updater 자체는 자동 교체하지
않는다. updater 프로토콜 변경은 설치 패키지를 사용한다.

릴리스 등록은 서버 운영자가 로컬 CLI로 수행한다. 인증된 관리자 화면에 표시되는
바이너리·플랫폼·해시와 감사 이력은 DB에 보관한다. 임의 URL이나 원격 셸 명령은
업데이트 요청으로 받지 않는다. `DATABASE_URL`은 기존 보호된 실행 환경에서 제공한다.

```sh
cargo run --locked -p p4forge-server --bin p4forge -- register-agent-release \
  --file target/release/p4forge-agent --os macos --arch aarch64 --version 0.1.0-selfpatch.1
cargo run --locked -p p4forge-server --bin p4forge -- register-agent-release \
  --file target/x86_64-pc-windows-gnu/release/p4forge-agent.exe \
  --os windows --arch x86_64 --version 0.1.0-selfpatch.1
```

`state/self-patch`는 root/SYSTEM과 관리자만 접근한다. 저널로 OS 재시작 후 교체 또는
복구를 이어가며, `previous.bin`은 다음 패치 전까지 유지한다. 복구 자체가 실패하면
저널과 drain을 유지하므로 호스트에서 저장된 이전 바이너리와 updater 상태를 점검한다.

## 검증

실제 Linux Agent를 포함한 검증은 전용 테스트 컨테이너에서 수행한다. 성공·실패·timeout·실행 중 취소·강제 종료 후 재시작, stream/classic view의 CL 2 고정 checkout, credential 분리, workspace/client 정리를 검사한다. 테스트 fixture는 격리된 SQLx DB에만 생성한다.

```sh
docker compose -f compose.yaml -f compose.test.yaml --profile test build tests
COMPOSE_IGNORE_ORPHANS=1 docker compose -f compose.yaml -f compose.test.yaml --profile test run --rm tests
```

```sh
npm run build
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
DATABASE_URL=postgres://p4forge:p4forge-local-only@127.0.0.1:15432/p4forge \
P4FORGE_TEST_P4PORT=p4forge.zenogrid.co.kr:1666 \
P4FORGE_TEST_P4_TICKETS_FILE="$PWD/.local/p4tickets" \
cargo test --locked --workspace -- --include-ignored
```

SQLx 통합 테스트는 임시 DB를 생성한다. 테스트 계정에는 DB 생성 권한이 필요하며 운영 계정을 사용하지 않는다. 실제 P4 테스트는 개발 stream의 CL 2/3을 사용한다. 인증·CSRF·state 재사용·사용자 비활성화, 권한, 발행 경쟁·불변성·복원, 실행 멱등성·버전 고정·취소를 검증한다. 2026-10-08에 실제 Linux Agent와 P4 테스트를 포함한 18개 테스트가 통과했으며, 실제 Google Workspace 관리자 로그인도 확인했다. 상시 Agent는 실제 프로젝트와 인증 파일을 등록한 뒤 시작한다.

| 경로 | 역할 |
| --- | --- |
| `crates/domain` | 설정 모델, TOML, DAG, 입력 검증 |
| `crates/perforce` | p4 CLI, submitted CL 및 stream snapshot |
| `crates/server` | Google OIDC, 권한, API, DB 트랜잭션, Job 배정·lease |
| `crates/agent`, `crates/protocol` | Linux/macOS/Windows 빌드 실행·정리, Server/Agent 요청 계약 |
| `migrations` | 설정·실행·감사·인증 테이블, 불변성 제약 |
| `web` | TypeScript UI; Rust 빌드 전 `npm run build` 필요 |
| `deploy`, `compose*.yaml` | Perforce·서버 이미지, SSL 구성 |

프로젝트 구성 단계에서 Rust 테스트 20개와 Perforce 게시 도구 모의 테스트 5개를 통과했다. 이후 macOS Agent와 실제 데이터 생성기를 검증했다. Windows 실제 빌드와 ECR 배포는 아직 실행하지 않았다.

Windows 실제 호스트와 Perforce View 검증 및 Agent 셀프 패치는 완료했다. 후속 구현은 원본 프로젝트 구성의 일반화된 실행 버전 변환, artifact 전달·보존 정책, Perforce polling·예약 실행과 CD 승인·배포다. root 서버 CI의 현재 연결은 아래 절에 기록한다. 전체 CI/CD 운영 완료 기준은 [PLAN.md](PLAN.md)에 있다.

공용 workspace 추가 후 Docker에서 Rust 테스트 **23개**, Python 게시 도구 테스트 **5개**를 통과했다. Windows 대상 release build와 clippy를 통과했으며, 설치 스크립트 2개의 PowerShell AST 구문 검사도 통과했다. Windows 실제 실행과 원본 7개 파이프라인 전체 실행은 아직 검증하지 않았다.

## 생성 파일 제출과 후속 파이프라인

Native 실행 설정의 `publish_generated`는 모든 단계가 성공한 후 Supervisor가 수행한다. `workspace = "pipeline"`과 Unix Agent protocol 3이 필요하다. 빌드 스크립트는 publisher ticket이나 P4 환경 변수를 받지 않는다. [데이터 생성·제출 설정](examples/root-data-publish.toml)과 [후속 검증 설정](examples/root-data-consume.toml)이 실제 root 프로젝트에 대응한다.

```toml
[publish_generated]
user = "p4forge-publisher"
paths = ["GameDesign/Tools/data_table/data_table/src", "GameDesign/Output/Sqlite"]

[[on_success]]
pipeline_id = "91ff0e4a-d970-4a76-aba0-5dd6a43e9130"
version = 1
```

제출 경로는 `...` 없는 디렉터리다. Agent의 root 전용 policy에서도 같은 서버 ID·접속 주소·stream·계정·경로를 승인해야 한다. 빌드 프로세스를 종료한 뒤 regular file만 root 전용 staging에 복사한다. symlink·hardlink·특수 파일·누락된 최상위 생성 디렉터리를 거부하며, 10,000파일/256MiB로 제한한다. 디렉터리 안의 파일 삭제도 제출에 포함한다. 별도 현재 stream client의 view가 실행 snapshot과 일치하는지 확인하고, input CL의 have revision을 기준으로 reconcile한다. 충돌을 강제 resolve하지 않는다.

서버에는 submit 전에 번호를 예약하고, submit 후 Perforce에서 작성자·client·run/attempt/base 식별자·변경 경로를 독립적으로 확인한다. Perforce가 번호를 바꾸면 원래 번호와 최종 제출 번호를 함께 보관한다. [Perforce describe -O](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/p4_describe.html)는 원래 번호로 최종 제출을 확인한다. 로그에 출력한 임의의 changelist 값은 제어 입력으로 사용하지 않는다.

변경이 없으면 입력 CL을 유지한다. 제출 결과가 불명확하거나 Agent가 종료되면 예약 기록을 남기고 자동 재제출·후속 실행을 하지 않는다. 운영자는 표시된 예약 번호로 `p4 describe -s -O NUMBER`를 확인한다. 제출된 change를 자동으로 되돌리지 않으며, 성공 확인 뒤에도 실행 취소·lease 만료가 발생하면 후속 작업을 시작하지 않는다.

`on_success`는 같은 프로젝트·Perforce 연결의 이미 발행된 **명시적 버전**만 참조한다. 후속 실행은 제출된 CL과 자신의 소스 View로 새로운 immutable snapshot을 가진다. 전체 stream에서 데이터를 생성한 뒤 좁은 ServerBuildView로 빌드할 수 있으며, 다른 Perforce 연결은 거절한다. 그 CL 이전의 다른 제출도 새 snapshot에 포함될 수 있다. 트리는 최대 64개 pipeline, 직접 후속은 16개로 제한하며 중복·diamond 합류를 거부한다. 각 연결의 고유 request ID로 서버 재시작이나 중복 scheduler 호출에도 한 번만 실행을 생성한다. 원래 실행자의 권한이 사라지면 후속 생성은 blocked로 표시된다. 현재 버전을 나중에 발행해도 이미 지정한 후속 버전은 바뀌지 않는다.

개발 서버의 제한된 계정은 `python3 scripts/configure-root-publisher.py`로 준비한다. 기존 Perforce protections를 보존하며 위 두 경로에만 write를 부여한다. policy/ticket은 `.local`의 0600 파일로 만들어진다. Mac 관리자 설치는 패키지의 `enable-publisher.sh POLICY_JSON TICKETS` 다음 `update.sh`를 사용한다. `/Library/P4Forge/secrets`에 root 전용으로 설치하고 launchd에 policy 경로만 전달한다. 티켓 만료 시 발급 스크립트를 다시 실행하고, 작업을 drain한 뒤 새 ticket을 설치한다.

원본 `data-table-generate` 등 7개 프로젝트 구성 초안은 자동 변환하지 않는다. named checkout view, ECR 인증/배포, 실제 Client 빌드 연결이 남아 있다. Windows의 generated publication은 실행 검증 후 지원할 예정이다.

2026-10-08 실제 검증: `data-table-publish` v1은 CL 6에서 생성·SQLite integrity 검증 후 **CL 9**를 제출했다. 실제 변경은 `GameDesign/Output/Sqlite/data_table.sqlite` 한 파일이며, 37개 Rust 생성 파일은 기존 내용과 같아 재제출되지 않았다. 자동 실행된 `data-table-consume` v1이 CL 9를 새로 checkout하고 SQLite 43개 테이블, Rust 37개 파일과 credential 접근 차단을 검증해 성공했다. 두 실행의 client는 모두 정리됐다. 검증 기록은 `.local/publication-validation-result.json`, 화면 증거는 `.local/publication-chain-success.png`다. Rust 단위 테스트 13개와 격리 PostgreSQL 서버 통합 테스트 13개가 통과했고, fmt/clippy/TypeScript 빌드 및 Windows cross build도 통과했다. 실제 Windows 실행은 아직 검증하지 않았다.


## root Windows 서버 CI

`server-windows-ci`는 원본 `server-windows-build`에서 컴파일·테스트 범위를 별도 실행 정의로 구성한 DB 파이프라인이다. [실행 TOML](examples/root-server-windows-ci.toml)은 공용 workspace에서 준비 → 단위 테스트 → Release 빌드 → 실행 파일 검증 순서로 실행한다. 원본의 Perforce 실행 파일 게시 단계는 프로젝트 구성 초안에 보존하며 이번 CI에서 수행하지 않는다.

`ServerBuildView`는 다음 매핑을 사용한다. 기존 `server-view`의 최상위 매핑과 달리 원본 빌드 스크립트의 `Server/` 상대 경로를 유지한다.

```text
//streams/root/Server/... Server/...
//streams/root/GameDesign/Tools/data_table/data_table/src/... GameDesign/Tools/data_table/data_table/src/...
//streams/root/GameDesign/Output/Sqlite/... GameDesign/Output/Sqlite/...
-//streams/root/Client/...
```

DESKTOP-6N6U52L의 전용 `p4forge-build` 계정은 관리자 프로필의 Rust를 사용하지 않는다. 공식 rustup 1.28.2 설치기의 SHA-256을 고정하고, 각 attempt의 HOME 아래에 Rust 1.90.0 MSVC minimal 도구를 설치한다. MSVC는 실제 확인한 Visual Studio 18 Community 경로를 사용한다. `ComSpec`, `PATHEXT` 등 필요한 환경은 빌드 프로세스에만 설정하며 시스템 PATH나 다른 사용자 프로필을 변경하지 않는다. 도구도 작업 정리 시 삭제되므로 현재는 실행마다 다운로드가 필요하다. [Rust 설치 위치 설정](https://rust-lang.github.io/rustup/installation/index.html#choosing-where-to-install), [minimal profile](https://rust-lang.github.io/rustup/concepts/profiles.html).

`data-table-publish` v3는 생성물 제출/변경 없음 확인 후 `data-table-consume` v1과 `server-windows-ci` v6를 자동 실행한다. 후속 버전과 제출 CL은 고정되며, 다음 서버 버전을 적용하려면 upstream 설정의 버전도 새로 발행해야 한다. 데이터 생성에서 시작할 때는 `data-table-publish`를 실행하고, 이미 제출된 소스만 빌드할 때는 `server-windows-ci`를 수동 실행한다.

현재 제출된 Server 소스는 두 개의 작은 테스트 프로젝트다. 각 crate의 단위 테스트, `Server/ci/build-windows-server.ps1`, 생성된 `api_server.exe`·`config_server.exe`의 실행 결과와 SHA-256을 검증한다. 실행 파일과 manifest는 모든 단계 성공 후 서버 산출물 저장소에 검증·업로드하고, 임시 workspace는 정리한다. 실행 상세의 **빌드 산출물**에서 다운로드한다. 실제 서비스의 통합 테스트나 운영 배포를 검증한 것으로 간주하지 않는다.


2026-10-08 실기 검증: `data-table-publish` v2 실행 `f25dc29a-502f-489f-a9a2-3f97f49f01ef`가 CL 9에서 생성한 데이터를 **CL 10**으로 제출했다. 자동 실행된 `data-table-consume` v1(`392da515-245e-4226-88ec-8717f76d74a4`)과 `server-windows-ci` v5(`bafa505d-f1cb-4f26-a439-01aad97d53e9`)가 모두 CL 10으로 성공했다. Windows에서는 전용 build 계정으로 Rust 1.90.0 설치, 각 서버 crate의 단위 테스트, Release 빌드, 두 EXE 실행과 SHA-256 검증을 통과했다. EXE 크기는 각각 133,120 bytes였다. 서버 통합 테스트 15개 통과(별도 live 검증 2개 제외), clippy·fmt·PowerShell 문법 검증을 통과했다. 증빙: `.local/server-ci-validation.json`, `.local/server-windows-ci-success.log`, `.local/server-ci-success.png`.


## 빌드 산출물 보관과 다운로드

Agent 0.1.2 / protocol 5부터 `workspace = "pipeline"`의 명시적 파일 목록을 수집한다. 설정은 다른 실행 정의와 함께 PostgreSQL 발행 버전과 Run snapshot에 고정된다. 예:

```toml
[[artifacts]]
name = "api_server.exe"
path = "GameDesign/LocalServer/bin/api_server.exe"
```

`name`은 다운로드 파일명(ASCII 문자·숫자·점·밑줄·하이픈), `path`는 checkout 기준 상대 파일 경로다. 와일드카드·절대 경로·상위 이동은 허용하지 않는다. 최대 16개 파일, 파일당 64 MiB, 실행당 합계 256 MiB다. 디렉터리, 누락 파일, symlink, Windows reparse point, hardlink와 특수 파일을 거부한다. 모든 작업이 성공한 후 빌드 계정이 접근할 수 없는 staging에 복사하여 SHA-256을 계산한다.

256 KiB 청크를 인증된 Agent API로 전송한다. 서버는 Agent/attempt/fence, lease, 취소, 완료된 작업과 고정된 파일 목록을 검사하며 manifest 변경을 거부한다. 같은 청크의 재전송은 내용이 같을 때만 허용하고, SQL에 기록되지 않은 디스크의 잔여 바이트는 재시도 때 잘라낸다. 서버의 전체 파일 해시 확인과 선언한 모든 파일의 업로드 완료 전에는 attempt가 성공할 수 없다. 오래된 Agent에는 산출물이 설정된 작업을 배정하지 않는다.

파일은 `P4FORGE_ARTIFACT_DIR`(직접 실행 기본 `.local/artifacts`)에 서버 UUID로 저장하고 메타데이터는 DB에 저장한다. Compose에서는 `server_artifacts` named volume을 `/var/lib/p4forge/artifacts`에 마운트한다. 성공한 Run과 attempt의 검증 완료 파일만 Google Workspace 세션과 해당 프로젝트 Viewer 이상 권한으로 내려받을 수 있다. 공개 `/downloads` 경로와 분리되어 있다.

프로젝트별 자동 보존 정책과 종료된 실행의 미완료 업로드 정리를 지원한다(아래 참조). 저장소 총량에 따른 제한·정리와 DB에 기록되지 않은 파일 정리는 아직 지원하지 않으므로 디스크 사용량을 감시한다. 백업은 DB와 artifact volume을 함께 보관하고 일관된 시점에 복구해야 한다. 실행 간 artifact 전달과 64 MiB 초과 파일은 후속 범위다.

2026-10-08 검증: Mac·Windows Agent를 0.1.2로 셀프 패치했다. `server-windows-ci` v6 실행 `a24ed8a7-7b58-4131-8c50-199de18e30f7`이 CL 10에서 4단계를 성공하고 `api_server.exe`·`config_server.exe`(각 133,120 bytes), `manifest.json`(399 bytes)을 보관했다. 관리자 브라우저에서 세 파일의 다운로드 해시와 EXE manifest를 확인했고, 서버 재시작 뒤 다시 내려받은 파일도 동일했다. 격리 DB 테스트는 무권한 접근·미완료 다운로드·잘못된 해시·재전송·fence·취소·만료·완료 조건을 검증한다. 별도 실제 Linux Agent 테스트는 300 KiB 파일의 청크 업로드, workspace 삭제, 서버 재생성 후 다운로드까지 통과했다. 증빙: `.local/artifact-validation/verification.json`, `.local/artifact-validation/artifacts-success.png`.

자동 갱신이 설정되지 않은 기존 설치에서는 Perforce 읽기 ticket이 만료되면 서버용 ticket을 갱신해 `ticket-init`으로 복사하고, 설치된 Agent의 보호된 ticket 파일도 갱신해야 한다. 기존 DESKTOP-6N6U52L 관리자 전용 설치 ZIP에는 `Renew-PerforceTicket.cmd`가 포함되어 있으며 재설치 없이 현재 credential 파일의 ACL을 유지해 ticket만 교체한다. 설치된 PC 이름과 새 ticket 유효성을 확인하고 실행 중인 attempt가 있으면 거부한다. Agent 0.1.2 이하의 바이너리 셀프 패치만으로는 ticket이 교체되지 않는다. 0.1.3부터 아래 서버 갱신 설정과 함께 자동으로 전달한다. 수동 설치의 Publisher ticket은 별도 갱신 대상이다.


## 산출물 보존 정책

프로젝트 화면의 **산출물 보존 정책**에서 프로젝트 관리자 또는 전역 관리자가 기간과 최근 성공 유지 건수를 지정한다. 설정은 DB에 저장하며 수정 시 generation을 비교해 다른 관리자의 변경을 덮어쓰지 않는다. 화면에서 **정리 대상 미리보기**를 확인한 뒤 저장한다. 새 프로젝트의 자동 정리는 기본적으로 꺼져 있다.

root 프로젝트의 선택한 정책은 **30일 보관 + 파이프라인별 최근 성공 5건 유지**다. 성공한 실행의 종료 시점부터 기간을 계산하며, 두 조건을 모두 충족해야 파일을 지운다. 최근 성공 5건은 30일이 지나도 보호하고, 산출물이 없는 실행은 유지 건수에 포함하지 않는다. 실패·취소·lost 등으로 남은 파일은 실행과 attempt가 모두 종료되고 마지막 생성/종료 시점에서 24시간이 지난 뒤 정리한다. lease가 만료되어도 상태가 아직 running인 attempt가 있으면 건드리지 않는다.

서버는 1분마다 정책을 확인한다. 주기당 최대 32개 프로젝트에서 각각 100개 정리 대상을 기록하고, 최대 100개 파일을 삭제한다. backlog가 많으면 여러 주기에 걸쳐 진행된다. DB에는 먼저 삭제 의도·정책 버전·사유를 확정하고, 실제 파일 삭제와 디렉터리 동기화 후 삭제 완료 시각을 기록한다. 파일 삭제 실패는 다음 주기에 재시도하며, 파일 삭제 직후 서버가 종료된 경우에도 재시작 후 완료 처리할 수 있다. 정책을 끄면 새 삭제 예약을 중지하며 이미 확정한 삭제는 마무리한다.

실행 이력, 파일명, 크기, SHA-256과 삭제 기록은 남는다. 정리 중·삭제 완료 파일은 더 이상 다운로드할 수 없고 기존 URL은 권한 확인 후 HTTP 410을 반환한다. 삭제 전에 이미 열린 다운로드는 같은 파일 핸들로 완료한다. 실제 삭제된 파일을 복원하려면 DB와 artifact volume의 백업이 필요하다. 미리보기와 실제 실행 사이의 새 빌드·상태 변경은 정리 직전 다시 평가한다.

검증에는 기간 초과와 최근 성공 보호, 파이프라인별 분리, 실행 중/종료 직후 업로드 보호, 미리보기 무변경, 관리자 권한과 동시 수정, 중복 정리, 삭제 실패 재시도, 삭제 후 중단 복구, 다운로드 경합을 포함한다. 운영 적용 전에 `.local/backups/pre-artifact-retention-*`에 DB와 산출물 볼륨을 함께 백업한다. 정책·삭제 감사 기록은 `audit_events`에 `artifact.retention_policy`, `artifact.retention_scheduled`, `artifact.deleted`로 남는다.

운영 적용 확인: root 정책 generation 1을 활성화했고 첫 자동 점검에서 정리 대상은 0개였다. 기존 Windows CI 산출물 3개를 정책 적용 후 다시 다운로드해 SHA-256이 같은 것을 확인했다. Linux 전체 테스트 44개(서버 통합 18개 포함)가 통과했다. 증빙은 `.local/retention-validation/verification.json`과 `.local/retention-validation/policy-enabled.png`에 보관한다.


## Perforce ticket 자동 갱신

Agent **0.1.3 / protocol 6**은 서버의 ticket broker에서 자신에게 허용된 읽기·게시 ticket을 받아 Supervisor 전용 디렉터리에 적용한다. Perforce 암호는 서버의 private 파일에만 보관한다. DB에는 Agent별 허용 계정·연결·검사/만료 시각·상태와 갱신 감사 기록만 저장하며 암호와 ticket은 저장하지 않는다. API는 기존 Agent Bearer 인증과 명시적 Agent UUID·연결·서버 ID 바인딩을 검사하고 `Cache-Control: no-store`를 사용한다. 폐기된 Agent에는 전달하지 않는다. 게시 grant는 로컬 Publisher 정책의 계정·연결·서버 ID와 일치해야 한다.

서버는 1분마다 `p4 login -s`로 만료 시간을 확인하고 남은 시간이 4시간 미만이면 보호된 암호를 CLI stdin으로 보내 `p4 login -a`를 실행한다. 만료 후 서버가 재시작해도 새 ticket을 발급하고 전달한다. 유효기간 무제한 설정은 사용하지 않는다. 현재 설치의 그룹 유효기간은 12시간이다. 갱신 실패는 다음 주기에 재시도하며 API·로그에는 암호나 CLI 진단을 포함하지 않는다. 관련 동작: [Perforce login 문서](https://help.perforce.com/helix-core/server-apps/cmdref/2024.2/Content/CmdRef/p4_login.html).

Agent도 1분마다 후보 ticket으로 서버 ID와 실제 인증을 확인한 후 파일을 원자적으로 교체한다. Windows는 SYSTEM/Administrators 전용 state 디렉터리의 ACL을 상속하고, Unix는 0700 디렉터리와 0600 파일을 사용한다. 빌드 계정 접근 불가 검사를 수행하며, 빌드 명령의 환경·workspace·로그에 credential을 전달하지 않는다. 실행 중에도 독립된 갱신 루프가 유지된다. 기존 설치 파일은 초기 seed로만 사용하고, 이후에는 `state/perforce/read.tickets`와 `publish.tickets`를 사용한다. 따라서 자동 관리 중인 Agent의 ticket을 예전 수동 갱신 ZIP으로 바꿀 필요가 없다.

**Build Agents**에서 읽기/게시 인증 상태, 만료 시각, 마지막 서버 갱신을 볼 수 있다. 관리되는 Agent에는 서버·Agent 검사 결과가 모두 정상이고 150초 이내이며, 남은 ticket 유효기간이 5분을 초과할 때만 새 작업을 배정한다. 게시 작업은 게시 계정도 확인한다. 확인 실패·만료·상태 지연 시 배정을 중지하고 복구 후 자동 재개한다. 관리자의 수동 draining 상태는 별도로 유지한다. 이미 배정된 attempt를 강제로 취소하지 않으며, 네트워크 단절 중 실제 Perforce 작업이 실패하는 경우까지 숨기지는 않는다.

실행 설정은 기존처럼 DB가 기준이다. 서버의 비밀 파일 경로와 허용 Agent 목록은 서버 운영자가 보호된 `.local/credential-policy.json`에 설정한다. `examples/credential-policy.json`의 UUID·계정·서버 ID를 실제 값으로 바꾸고 파일 권한을 0600으로 지정한다. 현재 Compose overlay는 `.local/p4-build-password`, `.local/p4-publisher-password`를 기존 계정의 암호 파일로 사용하며 `credential-init`이 서버 UID 10002 전용 볼륨으로 복사한다. 암호를 CLI 인수, `.env`, 공개 다운로드에 넣지 않는다.

```sh
docker compose -f compose.yaml -f compose.server.yaml -f compose.credentials.yaml up -d server
```

자동 갱신을 사용하는 이 설치의 서버 재생성에는 항상 `compose.credentials.yaml`을 포함한다. 설정·암호를 수정하면 같은 파일 목록으로 `run --rm --no-deps credential-init`을 실행한 뒤 서버를 재생성한다. 서버 비밀 볼륨과 `managed_credentials` 볼륨을 보호된 백업 대상으로 포함한다. Broker는 해당 디렉터리의 프로세스 잠금을 사용하므로 단일 서버 인스턴스에서 운영한다. Agent당 읽기 1개·게시 1개 계정을 지원한다. 계정 권한 변경과 암호 재설정은 Perforce 관리자가 수행한다.

정책 바인딩을 제거해도 `agents.perforce_managed`는 자동으로 해제하지 않는다. 서버를 예전 설정으로 실행하거나 Agent를 0.1.2로 롤백하면 새 작업이 계속 차단된다. 최신 서버·정책·Agent를 복구한 뒤 인증 확인을 기다린다. 이 보호를 SQL로 임의 해제해 인증 확인을 우회하지 않는다.

2026-10-09 실기 검증: Mac·Windows를 0.1.3으로 셀프 패치하고 읽기 2개·게시 1개 바인딩의 자동 갱신과 실제 인증을 확인했다. `data-table-publish` v3(`31f11e6d-4165-4518-9c22-d489f852304b`)가 CL 10에서 생성한 데이터를 CL 11로 제출했고, 후속 Mac 확인(`0272e541-fcc1-4cb5-8e6e-eb9043a2c5b2`)과 Windows CI v6(`6c17d6c3-a44c-4b89-bc1b-b4b5afbf062c`)가 CL 11에서 모두 성공했다. 빌드 중 인증 검사, 서버 재시작 후 세 바인딩 정상 복구, Windows 산출물 세 파일 다운로드 SHA-256을 확인했다. Linux 전체 48개 테스트(서버 통합 19개 포함)가 통과했으며 별도 live 테스트 4개는 이 자동 테스트 실행에서 제외했다. 실제 Mac·Windows 실행 결과는 위와 같이 별도로 확인했다. 증빙: `.local/credential-validation/verification.json`, 배포 전 백업: `.local/backups/pre-credential-renewal-20261008T150531Z/`.

## Perforce 제출 감지와 자동 빌드

발행된 파이프라인 화면의 **Perforce 자동 빌드 → 자동 빌드 설정**에서 프로젝트/전역 관리자가 활성화한다. 설정은 DB(`pipeline_triggers`)에 저장한다. 실행할 발행 버전, 포함 경로, 제외 경로를 명시한다. 경로는 `//streams/root/Server/...`처럼 끝의 `/...`를 사용하는 디렉터리 또는 정확한 depot 파일 경로다. 대소문자를 구분하며 제외가 우선한다. 삭제된 파일도 변경으로 판단한다. `*`, revision spec, View의 대상 경로는 필터에 사용하지 않는다. 여러 포함 경로 중 하나라도 제외되지 않은 변경 파일과 일치하면 해당 CL로 한 번 실행한다.

최초 저장은 현재 제출된 head를 기준으로 시작하며 과거 CL을 실행하지 않는다. 끄기/켜기와 서버 재시작은 DB cursor를 보존하므로 중단 중의 제출을 순서대로 처리한다. **현재 제출 번호로 새로 시작**을 명시적으로 선택할 때만 미처리 이력을 건너뛴다. 새 버전 발행은 자동 빌드 버전을 바꾸지 않으며 설정에서 새 버전을 선택해 저장해야 한다. 소스 연결/Root/서버 ID 변경은 기존 cursor로 진행하지 않는다.

서버는 15초 간격으로 최대 64개 설정을 검사하고 설정당 최대 16개 CL을 처리한다. 한 CL씩 `p4 changes -s submitted -m 1 -r -e N`으로 순서대로 가져와 `p4 describe -s`로 전체 변경 파일을 검사한다. 이 방식은 다중 `-ztag` 레코드의 동일 키가 덮어써지는 문제를 피한다. 조회는 기존 30초 timeout과 출력 4 MiB 제한을 사용하며 초과/오류 시 처리 위치를 진행하지 않는다. 큰 backlog나 느린 서버는 여러 주기에 걸쳐 따라잡는다. 읽기 계정에는 감지 대상 전체 경로의 list/read 권한이 필요하며, 계정에 보이지 않는 CL은 감지할 수 없다. CLI 동작 근거: [Perforce changes](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/p4_changes.html), [Perforce describe](https://help.perforce.com/helix-core/server-apps/cmdref/current/Content/CmdRef/p4_describe.html).

트리거 행 잠금으로 설정 변경/끄기와 감지를 직렬화한다. 고정 snapshot, Run, job DAG, 감지 이벤트와 cursor를 하나의 DB 트랜잭션으로 확정한다. 처리 도중 종료되면 모두 롤백되며 재시작 후 다시 검사한다. `(pipeline_id, server_id, changelist)` 고유 키로 중복을 방지한다. 설정한 사용자의 활성 상태와 현재 관리자 권한을 매 실행 시 다시 확인한다. 인증 실패·권한 회수·서버 변경·snapshot 실패·DB 오류에는 cursor를 보존하고 화면에 사유를 표시한다. 관리자가 다시 저장하면 설정 소유자를 변경할 수 있다. 끄기는 이미 등록된 빌드를 취소하지 않는다.

현재 운영 적용 범위는 `server-windows-ci` **v8**, 포함 `//streams/root/Server/...`다. 산출물 후속 전달 적용 때 v6에서 v8로 갱신했으며 cursor CL 12를 유지했다. 생성 데이터는 이 경로에 포함되지 않아 기존 데이터 게시 → 후속 Windows 빌드와 중복되지 않는다. 다른 파이프라인에 트리거를 추가할 때는 생성물 경로를 제외해 재귀 실행을 막아야 한다. 트리거 감지 이벤트와 실행 이력은 보존하며 별도 보존/집계 정책, 변경 묶음 처리, 대규모 backlog 최적화는 후속 범위다.

운영 검증 완료(2026-10-09): `server-windows-ci` v6의 자동 감지를 CL 11 기준으로 켰다. `Server/P4Forge-CI.md` 운영 문서 한 파일을 CL 12로 제출하자 실행 `3e2d957d-8140-4467-bbbc-c7ffc1b6ece1`이 자동 등록됐고 DESKTOP-6N6U52L의 네 단계가 모두 성공했다. EXE 두 개와 manifest 다운로드 SHA-256 및 manifest의 CL 12를 검증했다. 서버 재시작 후 cursor는 12, 해당 CL 실행은 1건으로 유지됐고 Mac/Windows ticket 상태도 정상 복구됐다. Linux 50개 자동 테스트가 통과했고 별도 live 테스트 4개는 해당 실행에서 제외했다. 실제 Windows 실행은 위와 같이 따로 확인했다. 검증 기록: `.local/trigger-validation/verification.json`, 화면: `.local/trigger-validation/automatic-build-success.png`, 배포 전 백업: `.local/backups/pre-change-triggers-20261008T154215Z/`.

## 후속 실행으로 산출물 전달

Agent **0.1.4 / protocol 7**부터 파이프라인에 `artifact_inputs = ["api_server.exe", "config_server.exe", "manifest.json"]`를 선언할 수 있다. 공용 pipeline workspace가 필요하며, 이름은 선행 파이프라인의 `artifacts` 이름과 정확히 일치해야 한다. 예시는 `examples/artifact-consumer.toml`에 있다. 자식 파이프라인을 먼저 발행하고 부모의 `on_success`에 자식 UUID와 고정 버전을 넣어 발행한다. 발행 시 각 직접 부모가 자식의 모든 입력 파일을 제공하는지 확인한다. 여러 부모의 병합과 최신 성공 실행 검색은 지원하지 않는다.

선행 실행이 성공하면 해당 Run의 검증된 산출물만 후속 Run에 바인딩한다. 제공 Run·artifact UUID·파일명·크기·SHA-256은 DB의 변경 불가능한 `run_artifact_inputs`와 실행 `input_hash`에 고정된다. 이후 같은 파이프라인의 새 빌드가 끝나도 이전 후속 실행의 파일은 바뀌지 않는다. 수동 실행은 화면의 **산출물 제공 실행 ID** 또는 API `source_run_id`로 성공한 실행을 명시한다. 동일 프로젝트·Perforce 연결/서버·CL이어야 하며, 생성 파일 게시가 있는 부모는 게시 결과 CL을 사용한다. 없는 파일, 실패한 실행, 삭제 예약/삭제된 파일은 실행 등록을 거절한다. 입력이 필요한 파이프라인은 Perforce 변경 감지의 직접 대상으로 설정할 수 없다.

Agent는 자기 실행의 유효한 attempt·fence·lease로 바인딩된 파일만 256 KiB 청크로 읽는다. 다른 실행의 artifact UUID나 다른 Agent의 attempt는 거절하고, 취소·만료·폐기된 Agent의 새 요청을 중단한다. 다운로드에는 기존 Bearer 인증을 사용하며 별도 공개 URL이나 빌드 스크립트용 token을 만들지 않는다. 네트워크 오류는 같은 offset으로 최대 세 번 시도한다. 파일당 64 MiB, 총 256 MiB, 최대 16개이며 크기·SHA-256을 모두 확인한 뒤 완성된 입력 디렉터리를 노출한다. 부분 파일이나 해시 불일치는 빌드 시작 전에 실패한다.

빌드 명령은 `P4FORGE_INPUTS` 디렉터리와 `P4FORGE_SOURCE_RUN_ID`를 사용한다. 입력은 checkout과 분리된 Supervisor 소유 디렉터리에 보관한다. Unix는 읽기 전용(파일 0444/디렉터리 0555), Windows는 전용 빌드 계정의 Read/Execute ACL을 사용한다. 수정할 파일은 checkout 또는 임시 작업 디렉터리로 복사한다. Unix 실행 파일은 작업 디렉터리에 복사해 실행 권한을 부여한 뒤 사용한다. 정규 파일만 제공하며 압축 해제·원본 경로 복원·symlink 전달은 수행하지 않는다. 실행 화면의 **전달받은 산출물**에서 원본 Run과 해시를 확인할 수 있다.

자동 정리는 후속 Run 생성 전의 대기 중인 `on_success` 소비자와 등록된 queued/running 소비자를 보호한다. 취소된 소비자의 Agent attempt가 아직 실행 중이어도 보호한다. 부모 Run 잠금과 재검사로 새 바인딩과 정리 예약의 경합을 막는다. 모든 소비자가 종료되고 정책의 다른 조건도 충족되면 원본 파일을 정리할 수 있다. 종료 후 재실행 시 이미 삭제된 파일을 다른 빌드로 대체하지 않는다. 바인딩과 원본 해시 이력은 계속 남는다. 영구 보존 고정과 대용량 파일 전송은 후속 범위다.

구버전 Agent에는 입력이 필요한 실행을 배정하지 않는다. 설치된 Agent는 셀프 패치로 0.1.4 이상을 적용하면 된다. 운영 서버 재생성에는 기존처럼 `compose.credentials.yaml`을 포함해 ticket 자동 갱신을 유지한다.

2026-10-09 실기 검증 완료: `server-windows-ci` v8 / CL 12(`e3724063-ccf4-49cc-b17d-ab8b126b0370`)가 EXE 두 개와 manifest를 보관하고, `server-artifact-check-macos` v1(`70986d25-18d0-4168-a148-a669655a5d27`)과 `server-artifact-check-windows` v2(`3a0cf219-3555-4c67-9dd8-5ede600ab2f0`)가 자동으로 전달받아 성공했다. 양쪽 Agent의 SHA-256·읽기 전용 권한, Windows의 전달받은 EXE 실행, 내려받은 검증 보고서 해시를 확인했다. 최초 Windows 검증 스크립트의 PowerShell 배열 처리 오류를 v2에서 수정했고 Agent의 전송 기능 변경은 필요하지 않았다.

자동 감지는 v8/generation 3/cursor CL 12로 유지하며 `data-table-publish` v5의 Windows 후속 대상도 v8이다. 서버 재시작 후 성공한 세 실행, 입력 바인딩 여섯 개, 후속 실행 두 건과 자동 감지 cursor가 그대로이고 양쪽 Agent 0.1.4와 읽기/게시 ticket 세 바인딩이 정상이다. Linux 전체 테스트 52개 통과(별도 live 테스트 4개는 기본 실행에서 제외), 별도로 실제 Linux Agent의 다중 청크 다운로드·변조 차단·정리 검증 1개, macOS Agent 테스트 11개와 Windows 교차 빌드/clippy 검사를 통과했다. 공개 Windows 설치 ZIP도 자격 증명 없이 0.1.4로 갱신했다.

증빙: `.local/input-validation/verification.json`, 두 OS의 `*-input-report.json`, `artifact-inputs-success.png`. 배포 전 백업: `.local/backups/pre-artifact-inputs-20261008T161149Z/`. 검증 대상은 기존 작은 Rust 테스트 프로젝트이며 실제 서비스 배포 검증은 다음 단계다.

## 배포 승인·환경 잠금·복구

프로젝트의 **배포 환경**에서 관리자가 이름과 전용 파이프라인을 연결한다. 표시된 환경 UUID를 TOML의 `[deployment] environment_id`에 넣고 `health_check`에 마지막 상태 확인 Job 이름을 지정한다. `examples/deployment.toml`은 workspace 안에서만 파일을 복사·검증하는 리허설 예시다. 배포 정의는 고정 `artifact_inputs`, 공용 workspace, 배포 단계와 별도 마지막 상태 확인 단계를 요구한다. 상태 확인 Job은 마지막 stage의 유일한 Job이어야 한다. Perforce 게시와 자동 후속 실행은 배포 정의에서 허용하지 않는다.

수동 또는 선행 빌드에서 등록한 배포 Run은 **승인 대기**로 남고 Agent가 가져갈 수 없다. 프로젝트 관리자 또는 전역 관리자가 발행 버전·CL·제공 Run·파일 해시·입력 SHA-256을 검토하고 사유를 남겨 승인한다. 승인은 해당 Run에만 적용되며 입력 해시와 환경 generation이 달라지면 거절한다. 요청자와 승인자를 분리하는 2인 승인은 현재 필수가 아니다. 배정 직전 승인자의 활성 상태와 현재 관리자 권한을 다시 확인한다. 승인자가 권한을 잃으면 관리자가 대기 실행을 취소하고 새 실행을 검토한다.

환경 하나에는 승인된 실행 하나만 존재한다. 승인이 환경을 점유하고, 배정부터 마지막 상태 확인 Job과 산출물 업로드까지 잠금을 유지한다. 모든 작업이 성공해야 현재 성공 배포와 확인 시각을 갱신하고 잠금을 해제한다. 상태는 해당 배포 때의 검사 결과이며 지속적인 서비스 모니터링을 뜻하지 않는다. 실행 전 거절·취소는 잠금을 풀지만, 실행이 시작된 뒤 실패·취소·lease 유실·Agent 폐기가 발생하면 `recovery_required`로 남겨 환경을 계속 잠근다. 서버 재시작이나 시간 경과만으로 해제하지 않는다.

실패 후 관리자는 대상 호스트의 외부 배포 프로세스가 종료됐음을 확인하고 **외부 작업 종료 확인 및 잠금 해제**에서 근거를 남긴다. lease 유실은 외부 작업 종료의 증거가 아니다. 잠금 해제는 환경을 정상으로 표시하지 않는다. 환경의 **성공 배포 복구 요청**에서 현재 또는 직전 성공 Run을 고르면 그 Run의 발행 버전·Perforce snapshot·원본 artifact UUID/해시로 새 승인 대기 실행을 만든다. 새 실행을 검토·승인한 뒤 같은 배포 명령과 상태 확인을 다시 수행한다. 설정이 새로 발행됐어도 복구 명령은 선택한 성공 버전에 고정된다. 없는 파일이나 바뀐 artifact 계보는 거절하며 request ID로 중복 요청을 방지한다.

현재·직전 성공 배포와 실패 후 환경을 점유한 실행의 입력 파일은 기존 기간/최근 성공 건수 정책과 무관하게 자동 정리에서 보호한다. 정상 배포가 전진해 두 성공 포인터에서 제외되고 다른 소비자도 종료되면 일반 보존 정책을 적용한다. 승인·거절·환경 생성·완료·종료 확인·복구 요청은 `audit_events`에 `deployment.*`로 기록한다. 환경과 Run 바인딩은 migration 0014의 DB 테이블에 저장하며 실행 기록의 환경·복구 원본은 변경할 수 없다.

서버의 환경 잠금은 P4Forge를 통해 실행하는 해당 배포 파이프라인 사이의 직렬화를 보장한다. 외부 운영자가 수행하는 작업을 중단하거나 대상 서버 자체를 잠그지는 않는다. 실제 서비스 연결 시 전용 배포 계정/Agent pool과 대상 시스템의 권한·동시성 제어를 함께 구성한다. 현재 명령은 설치된 Agent 0.1.4 / protocol 7과 호환되므로 추가 셀프 패치는 필요하지 않다.

2026-10-09 운영 리허설 완료: `server-staging-validation` / `staging-validation` 환경을 생성했다. v1 / CL 12 실행 `d5f30acd-32b2-444e-9fca-77ce1941427c`는 온라인 Windows Agent가 있어도 승인 전 attempt 0건으로 대기했고, 승인 후 두 EXE 복사·해시·실행 검증과 보고서 보관에 성공했다. 의도적인 v2 health 실패(`d609f915-fa0c-4d00-a046-327ddf8b33bb`)는 환경을 잠근 채 남았고 서버 재시작 후에도 잠금·원본 계보·감사 기록이 유지됐다. 격리 작업 종료 확인 후 v1 복구 실행 `16890e91-6005-4896-b759-a5d8237ce377`을 별도 승인해 성공했다. 원본·복구의 입력 해시 `f8c4b8ced2cf6ec5abb39a9ab1b246a29bad5dea1607c30ec9e1ffff59ed4a63`, artifact UUID·크기·SHA-256과 내려받은 보고서 내용이 일치한다.

리허설 환경은 generation 11, 정상, 잠금 해제 상태다. 최신 파이프라인 v3에는 정상 v1 정의를 다시 발행해 두었고 자동 트리거는 연결하지 않았다. 기존 Windows CI v8 자동 감지는 cursor CL 12를 유지한다. Mac·Windows Agent 0.1.4와 세 ticket 바인딩도 정상이다. Linux 54개(별도 live 4개 제외), macOS 55개(별도 live 2개 제외) 자동 테스트와 clippy/TypeScript/format 검사가 통과했다. 실제 Windows 리허설은 별도로 수행했다. 이는 workspace 내부 리허설로, 외부 서비스 배포나 지속 모니터링 검증을 포함하지 않는다.

증빙: `.local/deployment-validation/verification.json`, `deployment-recovery-success.png`, `approval-waiting.png`, `failure-lock-retained.png`. 배포 전 DB·산출물 백업: `.local/backups/pre-deployments-20261008T165031Z/`.

### Docker Compose 테스트 서비스

별도 테스트 서비스는 [테스트 화면](https://p4forge.zenogrid.co.kr:8444/deployment-demo/)과 [상태 확인 JSON](https://p4forge.zenogrid.co.kr:8444/deployment-demo/healthz)으로 접속한다. 기존 TLS 프록시의 경로 하나를 사용하며 `compose.demo.yaml`의 전용 Compose 프로젝트 `p4forge-deployment-demo`로 실행한다. Docker image digest를 고정하고, 비특권 사용자·읽기 전용 파일·자원 제한을 적용한다. 호스트 포트나 Docker socket을 서비스에 제공하지 않는다. 공개 응답에는 테스트 메시지·버전·릴리스 SHA-256만 포함한다.

호스트 운영자가 저장소 루트에서 실행한다(Python 3, Docker Compose와 기존 `p4forge_default` 네트워크 필요).

```sh
python3 deploy/test-service/manage.py apply --version demo-v2 --message '두 번째 테스트 릴리스'
python3 deploy/test-service/manage.py status
python3 deploy/test-service/manage.py rollback
```

버전·메시지로 만든 파일은 `.local/deployment-demo/releases/<SHA-256>/`에 보관한다. 배포 전 파일 무결성을 검사하고, Compose 컨테이너 검사와 실제 HTTPS 응답의 전체 JSON 일치 여부를 확인한 뒤 현재/직전 정상 릴리스 포인터를 저장한다. 명령 간 파일 잠금으로 중복 실행을 막는다. 실패하면 `pending`과 `recovery_required`를 남기고 마지막 성공 포인터를 보존한다. 원인을 해결한 뒤 `retry`로 동일 릴리스를 재시도하거나, `rollback`으로 마지막 정상 릴리스를 복구한다. 첫 배포 실패로 성공 이력이 없을 때는 `retry`를 사용한다. 현재는 컨테이너 교체 중 짧은 응답 중단이 가능한 테스트 구성이다.

위 명령은 **운영자 실행 방식**이다. 아래 전용 실행기의 `policy.json`을 설치하면 직접 apply/rollback/retry를 차단하고 P4Forge 승인 Run으로만 전환한다. 현재 테스트 메시지는 별도 fixture이며 기존 Perforce 서버 EXE를 서비스로 실행하는 구성은 아니다.

2026-10-09 초기 수동 검증: `demo-v1` 배포 → `demo-v2` 전환 → `demo-v1` 복구 → 컨테이너 재시작 후 동일 SHA-256 응답을 확인했다. 당시 릴리스는 `53533c24cf34931834b8b10c49e9b063a450cb05c766fe244992117337a93c69`였다. 무결성/HTML escape와 상태 확인 실패 후 복구 자동 테스트 2개가 통과했다. 증빙: `.local/deployment-demo/verification.json`. 현재 운영 상태는 아래 승인 Run 연동 검증을 따른다.

### 승인 Run 전용 Compose 실행기

`deploy/test-service/executor.py`는 Agent protocol 8로 작업을 수신하는 별도 실행기다. `compose-demo` pool의 `macos/aarch64` Agent로 등록한다. 배포는 `examples/compose-demo-deployment.toml`의 두 고정 작업을 허용하고, `demo-release.json`의 크기와 SHA-256, 엄격한 JSON 형식(`examples/compose-demo-release.json`)을 확인한다. 배포 이미지·Compose 파일·프로젝트·서비스·HTTPS 주소는 호스트 운영자가 관리하는 코드에 고정되어 있다. 선택적인 Docker 빌드 설정은 아래 절을 따른다. 두 모드 모두 임의의 호스트 shell을 실행하지 않는다.

각 실제 변경 직전에 `POST /agent/attempts/{id}/deployment`로 Agent 소유 attempt, fence, lease, 취소, 현재 승인자 권한과 환경 잠금을 재검사한다. 반환된 프로젝트·파이프라인·환경은 로컬 정책의 UUID와 일치해야 한다. 두 작업과 HTTPS 응답 검사가 끝난 뒤 서버가 `succeeded`로 확정한 경우에만 로컬 성공 포인터를 이동한다. 완료 직전 취소되면 성공으로 기록하지 않는다. API는 현재 상태 확인이며 네트워크가 끊겨도 사용할 수 있는 독립적인 배포 허가증이 아니다.

설정은 `.local/deployment-demo/executor/`에 두며 디렉터리 0700, 파일 0600으로 제한한다. 배포 정책은 `project_id`, `pipeline_id`, `environment_id`이고 Docker 빌드는 별도의 `docker_build` 항목으로 켠다. `token.json`의 `token`에는 이 전용 Agent에 발급한 token을 보관한다. 배포만 사용할 때는 Perforce 인증이 필요 없다. 발급 후 Docker Desktop을 사용하는 Mac 계정으로 `python3 deploy/test-service/install-executor.py`를 실행하면 사용자 LaunchAgent `kr.co.zenogrid.p4forge.compose-demo`를 설치한다. 해당 사용자 로그인과 Docker Desktop 실행이 필요하다. 코드·Compose 파일·정책을 수정할 수 있는 호스트 운영자는 신뢰 경계 안에 있다.

실행 중에는 heartbeat와 제한 시간을 확인한다. 취소·lease 유실 시 Compose CLI를 종료하고 실패를 보고한다. Docker daemon에서 진행 중인 작업까지 중단됐다는 보장은 없으므로 기존 서버의 환경 잠금을 유지하고 대상 확인 후 복구한다. 실행기가 중단됐다 다시 시작하면 미확정 배포를 재실행하지 않으며, 저장된 완료 요청만 동일하게 재전송한다. 승인된 복구 Run도 원본의 고정 artifact를 다시 내려받아 같은 검증을 거친다.

2026-10-09 초기 배포 실행기 검증: 기존 Rust 자동 테스트 macOS 55개·Linux 54개 및 전용 Python 테스트 8개 통과. 잘못된 환경·임의 명령·artifact 변조·lease 만료·취소와 재시작 후 중복 실행 방지를 검증했다. 전용 실행기 `compose-demo-executor`를 사용자 승인에 따라 등록하고 이 Mac에서 자동 시작하도록 설치했다. 당시 token과 정책 파일은 0600이며 Perforce 권한은 추가하지 않았다.

운영 연동 검증 완료: `compose-demo-release`의 성공 산출물로 `compose-demo-deploy` v1을 실행했다. 온라인 실행기가 있어도 승인 전 attempt 0건으로 유지됐으며 승인 후 `ci-v1`(`e3ff7d0a-7247-491b-a6d4-4bf888c48aa1`) 배포가 성공했다. 생성 파이프라인 v3에는 성공 시 배포 v1을 연결해 `ci-v2` 빌드 → 자동 승인 대기 → 별도 승인 → 배포 성공(`b3aa762d-b424-49fd-ba11-eca28e195a1a`)을 검증했다. 화면에서 직전 성공을 선택해 생성한 복구 Run(`8995b2ec-00e1-4825-a34c-4a64070c2b85`)도 새 승인 뒤 성공했다. 복구 Run의 입력 해시 `4e9df2f79909ffd6c65034e0a5ad78d46dc910abe1a421b95432d8b41441de10`과 artifact UUID·크기·SHA-256은 최초 `ci-v1`과 같다.

현재 테스트 서비스는 `ci-v1`이며 HTTPS 릴리스 해시 `63d064c509fea41c2852b194d4e273e4a0af42137f5f47c2bee7cf9811b09ade`가 최초 성공 응답과 일치한다. `compose-demo` 환경은 generation 10, 정상, 잠금 해제 상태다. 배포/전환/복구는 각 attempt 1건이며, 실행기 재시작 후에도 완료 기록과 서비스 상태가 유지되고 중복 Run/attempt가 생기지 않음을 확인했다. 신규 테스트 파이프라인에는 Perforce 변경 감지를 켜지 않았고 기존 Windows CI 자동 감지는 유지한다. 검증 기록: `.local/compose-executor-validation/verification.json`.

사용 흐름은 `compose-demo-release`의 원하는 발행 버전·CL 실행 → 성공 후 자동 생성된 `compose-demo-deploy` Run에서 입력 검토·승인 → 두 단계 성공 확인이다. 복구는 프로젝트 **배포 환경 → compose-demo 성공 배포 복구 요청**에서 성공 Run을 선택한 뒤 새 Run을 별도로 승인한다. 일반 빌드 Agent에 Docker 접근 권한을 주거나 수동 Compose 명령을 실행할 필요가 없다.

### compose-demo-executor Docker 이미지 빌드

`examples/compose-demo-docker-build.toml`은 같은 Agent에서 Perforce의 고정 CL을 받아 Docker 이미지를 만드는 실행 정의다. `workspace = "pipeline"`, 작업 1개, `runner = { os = "macos", arch = "aarch64", pool = "compose-demo", docker = "linux" }`, `script = ["p4forge-docker:build"]`를 사용한다. shell 표기는 `sh`지만 실행기가 이 식별자를 해석해 검증된 Docker CLI 인수로 실행한다. 일반 shell 스크립트는 지원하지 않는다.

작업의 `[jobs.environment]`에는 `BUILD_CONTEXT`, `BUILD_DOCKERFILE`, `BUILD_PLATFORM`, `BUILD_IMAGE`, 선택적인 `BUILD_TARGET`만 지정한다. 이 값들은 호스트 환경변수로 전달하지 않는다. 경로는 Perforce View로 동기화한 workspace 기준이며 Dockerfile은 context 안에 있어야 한다. `ServerBuildView`처럼 `Server/`를 유지하면 context는 `Server`, Dockerfile은 `Server/Dockerfile`이다. `server-view`처럼 Server 내용을 루트에 매핑하면 각각 `.`, `Dockerfile`이다. 플랫폼은 `linux/amd64`(기본) 또는 `linux/arm64` 중 하나이며 작업 제한 시간은 30~3600초다.

운영자는 `cargo build --release -p p4forge-perforce --bin p4forge-checkout`으로 신뢰된 checkout 도구를 만들고 실행기 전용 경로에 설치한다. `examples/compose-demo-executor-policy.json`의 `docker_build`에 도구와 p4 CLI의 절대 경로, 기존 Perforce 연결·서버 ID를 지정한다. 서버의 관리형 인증 정책에는 기존 Agent ID를 **read** 역할에만 추가하고 credential-init·서버를 재시작해 반영한다. 실행기를 재시작하면 protocol 8과 Docker Linux 가용성을 보고하고, 읽기 ticket을 0600 파일로 갱신·검증한다. ticket은 소스 workspace 밖에 보관하며 Docker context에 포함하지 않는다. 암호·쓰기 ticket·Agent token은 Docker 빌드에 전달하지 않는다.

이미지는 Docker Desktop의 `desktop-linux` 엔진에서 `--load`하며 `p4forge-build/<BUILD_IMAGE>:cl-<CL>-<attempt UUID>` 태그를 사용한다. 기존 배포 이미지 태그를 덮어쓰지 않는다. 선택적 산출물 `docker-image.json`에는 이미지 ID, 태그, 플랫폼, CL, Run·attempt ID를 기록한다. 이미지 자체는 로컬 Docker 엔진에 남는다. 레지스트리 push, 컨테이너 실행·서비스 배포는 포함하지 않으며 기존 Compose 승인은 그대로 필요하다.

빌드 종료 시 임시 Perforce client와 workspace를 정리한다. 취소·시간 초과·lease 만료 때 CLI 프로세스 그룹을 종료하고 실행기 프로세스가 강제 종료되어도 별도 감시 프로세스가 자식 작업을 중단한다. Docker daemon의 이미지 캐시나 이미 만들어진 이미지까지 삭제하지는 않는다. 재시작 후 미확정 빌드는 재실행하지 않고 실패를 보고하며, 확정 직전 저장한 완료 요청은 동일하게 재전송한다. 빌드 실패·취소가 Compose 배포의 정상 상태나 현재 릴리스 포인터를 바꾸지 않는다.

2026-10-09 13:25 KST 운영 검증: 사용자가 읽기 권한 연결과 재시작을 승인한 뒤 같은 `compose-demo-executor` ID·token을 유지하며 적용했다. root의 `server-docker-build` v1(`dae89272-a0bb-4916-8fd8-09eac96c4dcc`)을 `ServerBuildView`와 CL 12로 실행해 Run `12e8fdba-0a14-45cc-b36c-8de05c3f3f36`이 성공했다. 이미지 `p4forge-build/server:cl-12-a1180e6684104fb4845e618b1c8fe9dc`는 `linux/amd64`, ID `sha256:e85c78d26113ef7f3c2f3529ff6571cc9edb07128d870c34c41afbaa970af334`이고, 네트워크 없는 읽기 전용 컨테이너로 실행해 샘플 출력과 종료 코드 0을 확인했다. `docker-image.json` 450바이트의 SHA-256과 서버 산출물 상태도 검증했다. 초기 실행에서 발견한 산출물 업로드 순서 오류를 수정하고 회귀 테스트를 추가했으며, Python 17개·Perforce 8개 테스트와 clippy가 통과했다. 기존 Compose ci-v1, 두 배포 환경, Windows 에이전트 활성·mac-mini 비활성 상태를 보존했다. DB·정책 백업과 검증 결과는 `.local/docker-build-executor/`에 있다. 자동 빌드 트리거나 레지스트리 push는 켜지 않았다.

### 배포 환경의 허용 Agent

프로젝트 **배포 환경 → 환경 이름 허용 Agent 설정**에서 정확한 Agent를 선택하고 사유를 저장한다. 프로젝트 관리자만 변경할 수 있으며 공용 에이전트 목록에서 활성 Agent를 지정할 수 있다. Agent를 새로 등록하거나 자격 증명을 발급하는 기능은 아니다. 환경을 만든 뒤 처음 지정하기 전에는 배포 승인이 차단된다.

승인 시 지정 Agent의 pool·OS·아키텍처가 고정 파이프라인과 일치하는지 확인하고 Agent ID를 Run 승인 이력과 감사 기록에 남긴다. 서버는 작업 배정 시 이 ID를 다시 검사하므로 같은 pool의 다른 Agent도 해당 배포를 받을 수 없다. 전용 Compose 실행기는 대상 변경 직전에도 같은 제한을 확인한다. Agent가 오프라인이거나 새 작업 중지 상태이면 해당 Agent가 돌아올 때까지 대기한다.

Agent 변경에는 현재 환경 generation과 사유가 필요하다. 변경 후 기존 승인 화면은 새로고침해 다시 검토해야 한다. 승인·실행·복구 확인으로 환경이 잠겨 있으면 Agent를 바꿀 수 없다. 실행 전에는 승인 Run을 취소하고, 실행이 시작됐다면 기존 외부 작업 종료 확인 절차를 거친다. 다른 호스트로 옮길 때는 그 호스트의 배포 대상·접근 권한을 별도로 준비해야 한다.

migration 0015는 기존 환경과 과거 승인에 Agent를 자동 추정해 넣지 않는다. 서버 업그레이드는 실행 중 배포가 없는 상태에서 수행하고, 기존 환경에 허용 Agent를 지정한 뒤 다음 배포를 승인한다. 과거 Run에는 지정 이력이 없는 것으로 표시되며 실제 attempt의 Agent 기록은 보존된다. Agent protocol 7과 호환하므로 Mac·Windows Agent 업데이트는 필요하지 않다.

2026-10-09 운영 적용: `compose-demo → compose-demo-executor`, `staging-validation → desktop-6n6u52l`로 지정했다. 기존 성공 입력으로 새 승인 Run `34bf7192-d122-41d8-b1e7-8efaf30a680d`와 `dee39707-fc3c-4a16-9ea8-54940a135250`을 실행해 각각 지정 Agent에서 1회 성공했다. 승인 전 실행은 없었고 원본 입력 SHA-256이 유지됐다. Compose 환경은 g14, Windows 리허설 환경은 g15이며 둘 다 정상·잠금 해제 상태다. 테스트 서비스의 ci-v1 HTTPS 전체 응답도 동일하다. macOS 56개·Linux 55개 자동 테스트와 clippy/TypeScript/format 검사를 통과했다. 증빙은 `.local/executor-binding-validation/verification.json`에 보관한다.
