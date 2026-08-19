# GompassQL → GPUI 포팅 현황

브랜치: `gpui-port`. 웹앱(React + PixiJS/WebGL + GraphViz-WASM)을 Rust + GPUI로 재작성.
방침: **픽셀 동일 UI가 아니라 기능 동등성** (사용자 확인됨). 성능 기준: `schema.docs.graphql`(72k 라인, GitHub 스키마).

## 구조

```
gpui-app/
  core/            # gompass-core — GPUI 무관 순수 로직 (cargo test -p gompass-core)
    src/graph.rs   # sdl-to-graph.ts 포팅: SDL → ParsedGraph (+ until/reachable/components)
    src/layout.rs  # 계층형(Sugiyama) 레이아웃 — GraphViz WASM + 청킹 오케스트레이터 대체
  src/
    model.rs       # 뷰모델: 카드 지오메트리 단일 소스 (페인트/히트테스트 공유)
    canvas.rs      # GPUI 캔버스: 팬/줌/호버/클릭 + 페인팅 (텍스트 LOD, 컬링)
    main.rs        # 엔트리: SDL 로드 → 파싱 → 레이아웃 → 윈도우
```

## 웹앱 대비 의도적으로 버린 것 (WebGL 브라우저 한계 우회 코드)

- 텍스처 캐시/DPR 버킷/모션 게이트/스프라이트 LOD 플레이스홀더 — GPUI는 즉시 페인팅이라 불필요
- SDF 글리프 아틀라스 — GPUI 텍스트 시스템이 대체
- 레이아웃 워커 풀 + 컴포넌트 청킹(500노드 단위 이등분) — 네이티브 레이아웃은 한 방에 처리
- IndexedDB 레이아웃 캐시 — 네이티브 속도면 필요성 재평가 (필요시 파일 캐시)

## 개선(최적화) 사항

- 엣지가 dot 스플라인 대신 **필드 행 포트에 앵커된** 베지어 (소스 필드 행 중심에서 출발)
- 카드 행 지오메트리 수식이 한 곳(model.rs `Card::row_y`/`row_at`) — 웹앱은 같은 수식이 3벌
- 레이아웃/파싱 모두 네이티브 — WASM OOM, 워커 직렬화 오버헤드 소멸

## 성능 목표: 모든 줌에서 120fps (프레임당 8.3ms)

`GOMPASS_PERF=1`이 프레임 비용(`perf: k=…`)과 레이아웃 품질(`layout: …`)을 같이 찍는다.
GitHub 스키마(804 노드 / 3960 엣지) 기준, k=0.02~3.0 전 구간 **5.3~7.7ms**.

프레임을 지배하던 것은 언제나 엣지였고, 세 가지로 잡았다.

1. **큐빅을 GPUI에 넘기지 않는다.** `PathBuilder::cubic_bezier_to`는 선분 대비 엣지당 약
   5배 비싸고 화면 크기에 비례해 더 비싸진다(k=0.6에서 17.7ms 중 17ms). 대신
   `canvas.rs::flatten_cubic`이 2차 차분으로 화면상 굽은 정도를 재서 필요한 만큼만
   선분을 낸다.
2. **프레임 예산.** 이상적 허용오차(0.35px)로 먼저 견적을 내고, 예산(`SEG_BUDGET`)을
   넘으면 초과분의 제곱만큼 허용오차를 완화한다(n ∝ 1/√tol). 엣지가 이미 예산 안이면
   허용오차는 이상값 그대로다 — 실제로 k≥0.45 구간은 0.4~0.8px를 유지하고, 전체 스키마가
   한 화면에 들어오는 저줌에서만 2px대로 올라간다(엣지 폭 1px, 길이 700px 스케일).
3. **웨이포인트 완화.** 레인은 랭크마다 따로 잡히므로 경로가 지그재그로 나온다.
   `layout.rs::smooth_chain`이 내부 노드를 완화(20패스)하고 `simplify`가 남은 직선
   구간을 접는다. 렌더가 평탄화할 곡률이 줄어들 뿐 아니라 교차 수도 같이 줄었다.

## 레이아웃 복잡도

복잡도 = 엣지가 길수록, 무관한 카드를 가로지를수록 나쁨(사용자 정의). 측정 지표는
`avg len` / `crossings`(엣지가 지나가는 남의 카드 수, 호길이 32px 균등 샘플).

| 변경 | avg len | crossings/edge | world |
|---|---|---|---|
| 시작점 (longest-path 랭킹) | 8399 | — | 26981×21417 |
| BFS 깊이 랭킹 + median 조임 | 5110 | 5.59 | 14746×24649 |
| 가상 노드 높이 0 (레인은 상자가 아니다) | 5147 | 6.03 | 14746×22106 |
| 역방향 엣지도 레인 라우팅 | 4725 | 4.51 | 14746×21503 |
| 웨이포인트 완화 20패스 | 4725 | **4.28** | 14746×21503 |

측정해 보고 **버린** 것들: 타깃별 레인 번들링(스파인이 중앙값에 앉아 엣지가 기어올라감 —
교차 +88%), 레인을 랭크 사이 거터로 이동(차이 없음), longest-path 랭킹 유지(단조성은 얻지만
깊이가 두 배라 길이·교차 모두 악화). 라우팅 자체는 확실히 기여한다 — 끄면 4.28 → 6.69.

## 상태

- [x] GPUI 빌드 환경 (Rust stable 업데이트, Metal Toolchain 설치)
- [x] core/layout.rs — 계층형 레이아웃 + 과높이 랭크 컬럼 분할 + 셸프 패킹 (테스트 포함)
- [x] core/graph/ — sdl-to-graph 충실 포팅 (68 테스트, GitHub 스키마 파싱 ~80ms)
- [x] core/search.rs — fuzzyScore/proseMatch/snippet/dotted-query (6 테스트)
- [x] canvas/model — 1차 렌더링 (카드, 엣지, 팬/줌/호버/클릭 내비게이션, LOD, 컬링, 상태바)
- [x] 트리 패널 — uniform_list + 키 캡처 검색, ↑/↓/Enter, 클릭 → 캔버스 포커스
- [x] Cmd+K 검색 포커스, Cmd+B 사이드바 토글
- [x] 모드 탭 (Reachable / Orphaned / Deprecated) — 모드 전환 = 슬라이스 후 전체 재레이아웃(~10ms)
- [x] SDL 오버레이 엔진 — overlay.ts 충실 포팅 (62 테스트) + `--overlay <file>` CLI 통합,
      오버레이 카드 에메랄드 대시 보더 / 추가·변경 행 거터 마커 / 상태바 카운트
- [x] z-order 레이어 분리(엣지 아래/카드 위) + 카드 드롭 섀도 + 크롬 그림자
- [x] 호버 툴팁 — 필드/헤더 description, deprecation, 화면 가장자리 플립
- [x] Show descriptions(⌘D) — 행 설명 줄 + 재레이아웃 (ROW_H 16→28)
- [x] 엣지 번들링(⌘E, 기본 on) — 평행 필드 엣지 병합 (GitHub 스키마 3960→3121)
- [x] Investigate 모드(⌘I) — 미문서화 타입 주황 윤곽/행 틱 + 커버리지 % 상태바.
      `GOMPASS_INVESTIGATE=1`로 검증함. GitHub 스키마에서 아무 변화가 없어 보이는 건
      그 스키마가 100% 문서화돼 있어서지 기능 문제가 아니다 (미문서화 스키마로 확인:
      주황 윤곽 + 행 스트라이프 + `desc 33%` 표시)
- [x] deprecated/until — 만료 [until] 빨간 타입, Relay 필드 청록 마커
- [x] kind 필터 칩, referencedBy 상세 패널, 포커스 히스토리 ⌘[ + 브레드크럼
- [x] 파일 감시 핫 리로드(1s mtime 폴링) + ⌘O 파일 열기 — 웹 linked-file 대응
- [x] GOMPASS_SELFSHOT=<png> 셀프 스크린샷 (자기 창 캡처, 권한 불필요 — 시각 검증용)
- [x] 라이트/다크 테마 — 시스템 외관 연동 (GOMPASS_LIGHT/DARK 오버라이드)
- [x] 허브 페이딩 (degree ≥ 50) + 레이아웃 밀도 조정
- [x] 랜딩 화면 — 최근 스키마 히스토리(recent.json) + ⌘O 열기
- [x] 설정 영속화 — settings.json (Desc/Bundle/사이드바)
- [x] 오버레이 도크(⌘U) — 파일 선택(⌘⇧O)/해제, diff 컬럼(추가·변경·제거,
      클릭 내비게이션), 오버레이 파일도 감시 대상이라 편집 즉시 반영
- [x] 인앱 SDL 에디터 (editor.rs — 커서/선택/클립보드/스크롤, ⌘↵ 적용)
- [x] Sugiyama 가상 노드 라우팅 + transpose + PAVA 좌표 (엣지 교차/드리프트 감소)
- [x] 레이아웃 복잡도 재설계 — BFS 깊이 랭킹, 역방향 엣지 레인 라우팅, 웨이포인트 완화
      (avg 엣지 길이 8399→4725, 교차 4.28/엣지, world 26981×21417→14746×21503)
- [x] 120fps 보장 — 화면 오차 기반 적응 평탄화 + 프레임 예산 (전 줌 5.3~7.7ms)
- [x] 엣지 호버 툴팁(번들 라벨), 행 핀/타입 클릭 구분, 검색 하이라이트·히스토리
- [x] Prims/Relay 토글, 루트 선택기, 타이틀바 제거, 웹 동일 노드 컬러
- [ ] 유사도 힌트(union 동료 배치) — transpose 정련으로 대체, 미구현
- [ ] 요소 단위 backdrop blur — GPUI 미지원이라 반투명+그림자로 근사 중
- [ ] 트랙패드 핀치 줌 — GPUI가 magnify 제스처 미노출(스크롤 줌으로 대체)
