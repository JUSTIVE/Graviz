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
- [x] Investigate 모드(⌘I) — 미문서화 타입 빨간 윤곽/행 틱 + 커버리지 % 상태바
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
- [x] 엣지 호버 툴팁(번들 라벨), 행 핀/타입 클릭 구분, 검색 하이라이트·히스토리
- [x] Prims/Relay 토글, 루트 선택기, 타이틀바 제거, 웹 동일 노드 컬러
- [ ] 유사도 힌트(union 동료 배치) — transpose 정련으로 대체, 미구현
- [ ] 요소 단위 backdrop blur — GPUI 미지원이라 반투명+그림자로 근사 중
- [ ] 트랙패드 핀치 줌 — GPUI가 magnify 제스처 미노출(스크롤 줌으로 대체)
