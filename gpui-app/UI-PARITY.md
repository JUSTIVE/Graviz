# 웹 UI 그대로 옮기기 — 차이 분석 & 할 일

웹앱(React/Tailwind/Pixi) UI 스펙을 정밀 추출해 GPUI 구현과 대조한 결과.
기준: 웹의 위치·치수·순서·색상을 **그대로** 재현. 각 항목 뒤 `[web ref]`는 원본 근거.

## 0. 팔레트 (완료)

웹은 shadcn OKLCH → sRGB 로 변환하면 Tailwind `neutral` 계열.
`--background #0a0a0a / --card #171717 / --border white 10% / --muted-fg #a1a1a1`
(라이트: `#fafaf9 / #ffffff / #e5e5e5 / #737373`). ✅ theme.rs 반영 완료.
`--primary` 는 **무채색**(near-black/near-white)이며, 색은 전부 Tailwind 리터럴에서 온다.

---

## 1. 노드 카드 렌더링 (가장 큰 시각 차이)

| # | 항목 | 웹 | 현재 GPUI |
|---|---|---|---|
| 1.1 | 헤더 배경 | kind 색 **불투명(α1)** | kind 색 16% 틴트 |
| 1.2 | 헤더 kind 라벨 | **좌상단** `OBJECT` 대문자, 600 9px, 흰색 α0.6, x8/baseline14 | 우측 정렬 소문자, kind 색 |
| 1.3 | 타입명 | 600 13px, **흰색**, x8/baseline30 | 본문색, y8 |
| 1.4 | 헤더 설명 | desc 모드에서 1줄, 9px 흰색 α0.75, baseline42 (헤더 42→56) | 없음 |
| 1.5 | 행 높이 | `ROW_H=14` (desc 26) | 16 |
| 1.6 | 행 기하 | bodyY=headerH+6, baseline=bodyY+i·rowH+10, 이름 x=10, 타입 우측 x=w−10 | 유사하나 상수 불일치 |
| 1.7 | 반환 타입 색 | 일반 `#f59e0b` / **빌트인 스칼라 `#b08c5a` α0.7** / 만료 `#ef4444` | 빌트인 구분 없음 |
| 1.8 | deprecated | α0.4 + **이름·타입 양쪽 취소선** | 취소선만, 이름만 |
| 1.9 | enum 행 | muted 색, **만료 시에만** 취소선, 페이드 없음 | 필드와 동일 처리 |
| 1.10 | union 멤버 행 | 피치 **항상 14**, 색 `#0ea5e9`, 잘림 없음 | 일반 행과 동일 |
| 1.11 | scalar 본문 | `italic "custom scalar"` 1줄 | 없음 |
| 1.12 | trailing 밴드 | implements=violet wash α0.1 + 상단 divider α0.4 / member-of-union=amber, 텍스트 600 10px 밴드색, **접두어 없이 1줄 1개**, `trailingSectionGeom` 기하 | 일반 행에 `implements X` 텍스트 |
| 1.13 | Relay 글리프 | 8×8 SVG path, `#F26A03` α0.85, cx=w−10−typeW−8 | 3.5px 청록 점 |
| 1.14 | 오버레이 거터 | `fillRect(3, fy−7, 2, 9)` emerald | x2, 3px 폭 |
| 1.15 | 폭 산정 | `max(220, min(900, 16+nameW₁₃), fieldMax)`; fieldMax=name₁₀+16+type₁₀+20 | 근사식 |
| 1.16 | 높이 산정 | `headerH+8+body+implGap+unionGap+10`, tight행 항상 14 | 근사식 |
| 1.17 | 호버 행 | foreground α**0.07**, inset 4, r3 | 흰색 α0.06 |
| 1.18 | 반환타입 호버 칩 | amber 채움 α0.22 + 1px 테두리 α0.9 | 없음 |
| 1.19 | 호버 링 / 포커스 링 | pad3 r9, 1.5px α0.4 / **2.5px α0.75** | 보더 색만 변경 |
| 1.20 | 핀 행 | `#f97316` 채움 α0.18 + 2px 테두리 α0.95 | amber α0.18만 |
| 1.21 | 비포커스 디밍 | α**0.1** | 0.22 |
| 1.22 | 오버레이 아우라 | emerald 3중 halo + 링 | 대시 보더만 |

## 2. 모드 탭 — **위치가 다름**

- 웹: **사이드바 내부 최상단**(트리 위). 행높이 34px, 하단 border 1px.
- 탭: px12/py8, **border-bottom 2px**, 12px medium, `flex-grow` 1(활성)/0(비활성) → 비활성은 아이콘만(40px), 라벨은 max-width 0↔200px + opacity 로 300ms 애니메이션.
- 아이콘 16px: Reachable=Waypoints(sky) / Orphaned=Unlink(amber) / Deprecated=Clock(red|amber) — **비활성에도 색 유지**(α0.7).
- 카운트 pill(10px, bg tone/15%): Orphaned=고아 수(0이면 **비활성**), Deprecated=만료+예정+deprecated 합(0이면 **DOM에서 제거**).
- 4번째 셀: 사이드바 접기 버튼(PanelLeftClose 16px, 좌측 border).
- 현재: 캔버스 위 탭바 + 별도 Root 셀렉터.

## 3. 사이드바 골격

- 폭 기본 **340**(min 260 / max 720), 6px 리사이즈 핸들이 보더를 걸침, 더블클릭 340 복귀, `gompassql:sidebarWidth`/`Collapsed` 영속화.
- 배경 `--card/30%`, 우측 border(접히면 제거), 접힘은 300ms 그리드 애니메이션 + 내용 고정폭 클리핑.
- 접힌 상태 확장 버튼: 좌상단 16/16, r10, popover/95, PanelLeftOpen 16px → 이때 캔버스 뷰컨트롤이 **+44px** 밀림.
- 현재: 고정 300px, 리사이즈·영속화·확장 버튼 없음.

## 4. 탭 본문 (사이드바) — **미구현**

- **OrphanPanel**: kind 그룹(Object→Interface→Union→Enum→Input→Scalar 고정 순), sticky 헤더(10px 대문자 + 개수), 행=kind 뱃지 + 이름 + `{n}f` 필드수, 빈 상태 문구.
- **UntilPanel**: Expired / Upcoming / Deprecated 3섹션, 섹션 배너(색상별), 타입 그룹 헤더, 필드 행=취소선 이름 + 날짜 칩 + 메타(`{n}d overdue` / `in {n}d` / 사유).
- 현재: 모드가 바뀌어도 사이드바는 항상 동일한 타입 목록.

## 5. 캔버스 오버레이

| # | 항목 | 웹 | 현재 |
|---|---|---|---|
| 5.1 | 뷰 컨트롤 | 좌상단 16/16 카드(r10, popover/95, shadow-lg, **평상시 α0.4 → 호버 1.0**), 칩 4개 = `Hide primitives` `Hide Relay` `Show descriptions` `Bundle edges`, rounded-full·10px·Filter 아이콘 10px, ON=`border-primary bg-primary/10` | 6개 사각 버튼, 라벨/순서/스타일 상이, 항상 불투명 |
| 5.2 | Investigate | **별도 카드** 좌16/상56, Microscope + "Investigate", 모드 pill `Missing descriptions`(orange), 커버리지 % 뱃지(≥90 emerald/≥50 amber/else rose) | 뷰컨트롤 안 토글 + 상태바 텍스트 |
| 5.3 | Recent | 우상단 16/16, **256px**, 헤더 History+`Recent (N)`+Trash2+chevron, **기본 접힘**, 행에 kind 뱃지·필드 amber·엣지 화살표, 행별 X 제거, max-h 60vh, cap 50 | 200px, 항상 펼침, 뱃지/삭제/접기 없음 |
| 5.4 | 포커스 엣지 위젯 | 하단 중앙, `src ▸ tgt` 뱃지 + 필드 라벨 + X | 없음 (엣지 포커스 개념 자체 없음) |
| 5.5 | FPS 패널 | 우하단 16/16, minWidth 280, **260×48 막대 차트**(60샘플, peak 기준, 저FPS 빨강) + `N fps` + `N nodes · N edges` + 타이밍 | 좌하단 상태바(다른 내용) |
| 5.6 | 레이아웃 진행 | 스크림 + 카드(스피너·진행바) | 없음 (네이티브는 즉시) — **생략 판단** |
| 5.7 | 툴팁 | 커서 +12px, **화면 중앙 넘으면 우측 앵커 / 하단 80px 내면 하단 앵커**, 4종(엣지·헤더·필드·히스토리) 각기 다른 스타일, 필드 툴팁은 kind 뱃지+이름+amber 타입+설명, 넘치면 마퀴 | 1종, pane 기준 플립, 스타일 상이 |
| 5.8 | 빈 공간 클릭 | 포커스·핀 해제 | 없음 |

## 6. 오버레이 도크

- **기본 접힘**: 27px 스트립 = Layers(emerald) + "Overlay" + 상태(에러 빨강 / 카운트 pill / 안내문) + ChevronUp.
- 펼침: 높이 280(160–720, ≤70vh), 상단 6px 리사이즈 핸들, 더블클릭 280 복귀, `gompassql:overlayHeight`.
- 헤더: Layers + "Overlay" + **카운트 pill** `+n`(emerald) `~n`(sky) `−n`(U+2212, red) + `Highlight` 토글(emerald, 비활성 시 α0.4) + `⌘↵` 힌트 + **Apply/Re-apply**(emerald 버튼, dirty 아니면 비활성) + Clear + 접기 chevron.
- 에디터: CodeMirror(줄번호·폴딩·GraphQL 하이라이트) + 고정 플레이스홀더 예제.
- 상태 스트립(도크 하단, ≤45%): 에러 블록(red/10) / 적용 블록(emerald/10, **추가 타입은 클릭 가능한 칩**, 추가·변경·제거 목록) / 경고 블록(amber/10).
- 현재: 항상 펼침 240px, 카운트 pill·Highlight·상태 스트립·플레이스홀더 없음, diff는 4열.

## 7. 앱 셸 — **미구현**

- sticky 헤더 40px(≥1024px 56px), 하단 border, `background/80` + blur.
- 좌: 아이콘 16px + 워드마크 **"Graviz"**(600) / 내비 `New` `View`(스키마 있을 때만) `About` — 활성=secondary 배경.
- 우: **테마 토글**(32px, outline, Sun/Moon/Monitor + Light/Dark/System, 클릭 순환).
- 커밋 뱃지: **화면 하단 중앙 고정**, 10px mono, muted α0.4.
- 현재: 중앙 "GompassQL" 타이틀 스트립만.

## 9. 사이드바 내용 (TreePanel)

| # | 항목 | 웹 | 현재 |
|---|---|---|---|
| 9.1 | 검색 입력 | 높이 30px, r4, Search 아이콘 12px(좌8), placeholder `Search types & fields…`, 우측 `⌘K`(10px α0.5) / 입력 시 X 지우기, Esc=clear+blur | 박스만, 아이콘·클리어·힌트 없음 |
| 9.2 | 검색 히스토리 | **칩이 아니라 행 목록** — 입력 포커스 + 쿼리 비었을 때 트리 대신 전체 차지, 헤더 `Recent`+`Clear all`, 행=Clock 12px + 쿼리 + X, `graviz:search-history` 최대 10 | 칩 6개 |
| 9.3 | kind 필터 칩 | 쿼리 있을 때만, rounded-full, 라벨+**개수**(mono 9px), 활성=kind 틴트+투명 보더, `Clear`(X) | 항상, 개수 없음, 색만 |
| 9.4 | 결과 행 | 2줄: ① kind 뱃지(solid) + `Type` muted + `.` + 필드명(하이라이트) + **우측 원본 타입**(10px muted) ② 스니펫(`desc`/`deprecated` 태그칩 + italic). 하이라이트=**bold + primary색**, 배경 없음. **"N results" 라벨 없음** | 1줄, dot+이름+상세, 하이라이트=accent bold |
| 9.5 | 루트 선택 | 드롭다운 아님 — 스키마명 라벨 + 루트별 토글 버튼(활성=primary) | 탭바 우측 순환 버튼 |
| 9.6 | All types | 접이식 헤더 `All types (N)` **기본 접힘**, 본문 max-h 192px 가상 리스트, 행 24px, 선택=primary 배경 | 항상 펼침 전체 목록 |
| 9.7 | 컨텍스트 섹션 | `Implemented by (N)` / `Members (N)` / `Referenced by (N)` 각 max-h 192px, 행=뱃지+이름+**후행 칩**(다른 인터페이스/유니온/참조 필드명) | referencedBy만, 칩 없음 |
| 9.8 | TypeDetail | 헤더(뱃지+14px 이름) → 설명 → `implements` 칩 행 → FieldRow 목록 | 헤더+설명+referencedBy |
| 9.9 | FieldRow | 이름 + **인자 수 뱃지 `(1/3)`** + 우측 타입 칩(호버 시 chevron 이동) + Relay 10px `#F26A03` + deprecation 줄 + 설명 줄 + **호버 시 인자 목록** + 호버 툴팁. 행 클릭=핀, 타입 칩 클릭=이동 | 없음 (캔버스에만 존재) |
| 9.10 | 상태 배경 | deprecated=amber/10 α0.6, expired=red/10, 핀=orange/10 + ring | 없음 |

## 10. 랜딩

- 폭 768px 중앙, gap 16px, padding 24px.
- 순서: ① 타이틀 24px `Visualize your GraphQL schema` + 태그라인 14px ② 액션바 `Open file`/`Link file`/`Load sample`(32px outline, 아이콘 14px) + 파생 이름 ③ **Recent schemas 카드**(기본 접힘, 행 3줄: 이름 14px / `updated YYYY-MM-DD HH:MM` 11px / `N types · N enums · N unions · N lines` + 해시 칩) ④ 에디터(CodeMirror, placeholder `# Paste your GraphQL SDL here…`) ⑤ 에러 배너(destructive) ⑥ 경고 배너(amber) ⑦ 우측 정렬 `Visualize` 버튼(40px, Wand2) ⑧ 드래그 시 전체 화면 스크림 + 점선 카드.
- 현재: 타이틀 + Open 버튼 + 최근 목록(1줄)만.

## 8. 진행 결과

- [x] 1. 노드 카드 렌더링 전면 정합 (1.1–1.22) — 지오메트리 테스트로 검증
- [x] 2. 모드 탭 사이드바 이동 + 탭 본문 Orphan/Until (2, 4)
- [x] 3. 캔버스 오버레이 (5.1 뷰 컨트롤, 5.2 Investigate, 5.3 Recent,
      5.5 FPS 차트) + 툴팁 3종·플립 규칙 (5.7) + 빈 공간 클릭 해제 (5.8)
- [x] 4. 앱 셸 헤더·테마 토글·커밋 뱃지 (7)
- [x] 5. 오버레이 도크 정합 (6) — 접힘 스트립·카운트 pill·Highlight·상태 블록
- [x] 9. 사이드바 내용 전면 정합 (검색·히스토리 행·kind 칩·2줄 결과·
      루트 토글·All types·컨텍스트 섹션·TypeDetail/FieldRow)
- [x] 10. 랜딩 (타이틀·액션바·Recent 카드·에디터·배너·Visualize·드롭 오버레이)

### 2차 스윕 (웹앱 전체 재대조) — 진행 결과

| # | 기능 | 웹 위치 | 상태 |
|---|---|---|---|
| A1 | 도트 그리드 배경 (24px 격자, muted-fg @ 0.18, 월드 공간) | `SchemaCanvas.tsx:1450` | [x] 줌아웃 시 격자 간격을 2배씩 늘려 화면상 18px 이상 유지 |
| A2 | 포커스 엣지 — 클릭 시 그 엣지만 고정, 나머지 디밍, 하단 중앙 카드 + `Clear edge focus` | `SchemaCanvas.tsx:1703,2252,5369` | [x] Esc·빈 캔버스 클릭·카드 클릭으로 해제 |
| A3 | 포커스 링 리플 애니메이션 (1600ms 주기 ×3) | `SchemaCanvas.tsx:3503` | [x] `reduce_motion` 존중 |
| A4 | Recent 히스토리에 **엣지** 항목 | `SchemaCanvas.tsx:1704` | [x] `Source → Target`, 클릭 시 재방문 |
| C1 | 사이드바 폭 드래그 리사이즈 + 영속화 | `View.tsx:186` | [x] 260–720px, settings.json |
| C2 | 오버레이 도크 높이 드래그 리사이즈 + 영속화 | `View.tsx:818` | [x] 160–720px, 뷰포트 70% 상한 |
| C3 | About 라우트 + 헤더 nav 링크 | `routes/About.tsx` | [x] GPUI 파이프라인 기준으로 내용 재작성 |
| B1 | 유니온 멤버 인접 힌트 | `lib/similarity.ts` | **미채택** — 아래 참조 |

**B1을 채택하지 않은 이유.** 유니온 멤버쌍을 정렬 스윕의 비제약 힌트로 넣어 pull
0.1/0.25/0.5 및 "마지막 스윕에만 적용" 변형까지 측정했다. 형제 간 평균 거리는 3405 → 3266
(−4%)로 의도대로 줄지만, 대가로 평균 엣지 길이 4725 → 4746~4884, **최장 엣지 14847 →
16323 (+10%)**, world 높이 21503 → 23069이 된다. 복잡도 기준(길이·교차)에서 손해라 되돌렸다.
웹이 이 힌트로 이득을 본 건 dagre 정렬의 실패 양상이 달라서로 보인다 — 여기서는 BFS 랭킹과
transpose가 이미 유니온 멤버를 한 랭크에 모아 둔다.

### 비해당으로 확정
레이아웃 진행 오버레이(네이티브는 즉시), 모바일 단일 패널 레이아웃, 트랙패드 핀치 줌
(GPUI가 magnify 제스처 미노출), IndexedDB 레이아웃 캐시, About의 WebGL 렌더링 기법 서술
(해당 파이프라인이 존재하지 않음 — GPUI 기준으로 다시 씀).

### 디밍 알파가 웹과 다른 이유
웹은 PixiJS **컨테이너** 알파라 겹친 엣지가 한 번만 합성된다. GPUI에는 그룹 알파가 없어
스트로크마다 합성되고, 0.1로는 20겹만 겹쳐도 88% 커버리지가 되어 오히려 밝아진다. 그래서
엣지 고정 시 디밍은 `PIN_DIM_ALPHA = 0.015`로 따로 둔다 (노드 포커스 디밍은 0.35 유지).
