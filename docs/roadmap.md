# 확장 로드맵

현재 hangul-mcp는 rhwp `DocumentCore`의 코어 편집 세트(세션·읽기·검색/치환·텍스트/문단·표)를
21개 도구로 노출한다. 아래는 **rhwp에 이미 구현돼 있어 도구 핸들러만 추가하면 되는** 확장 후보다.
대부분 `src/server.rs`에 `#[tool]` 메서드와 파라미터 구조체를 추가하는 수준의 작업이다.

도구 진입점은 모두 `rhwp::DocumentCore`의 `*_native` 메서드이며, 다수가 이미 JSON 문자열을
반환하므로 MCP 결과로 그대로 전달할 수 있다.

## 1순위 — 양식 자동화 / 문서 생성

### 누름틀(필드) 값 채우기 — ✅ 구현됨
양식 문서(보고서 양식, 신청서)의 필드를 이름으로 찾아 값을 채운다. MCP 활용 가치가 가장 높다.
`samples/`의 "기부·답례품 실적 보고서_양식.hwpx" 같은 문서 자동화가 바로 이 경로다.
`list_fields` / `get_field_value` / `set_field_value` 3개 도구로 노출했고, 저장까지 라운드트립
검증했다. 셀 안의 필드도 지원한다.

- `get_field_list_json`, `get_field_value_by_name`, `set_field_value_by_name`,
  `set_field_value_by_id`, `remove_field_at` — `queries/field_query.rs`

### 폼(양식 개체) 값 채우기 — ✅ 구현됨
체크박스·콤보박스·에디트·버튼 등 양식 개체. rhwp에 양식 개체 writer
(`serializer/hwpx/form.rs`)를 추가해 선결 조건을 해소했고(이전엔 저장 시 개체가 전부
사라졌다: 메모리 5개 → 0개), 그 위에 `list_forms`(발견) / `get_form_value` /
`get_form_info` / `set_form_value` 4개 도구로 노출했다. 본문 양식 개체를 좌표로 채우고
저장까지 라운드트립 검증했다(폼 5개 보존 + 값 유지). 본문 수준만 지원하며 표 셀 안
양식 개체는 후속 과제다.

- rhwp: `get_form_value_native`, `set_form_value_native`, `get_form_object_info_native`
  — `queries/form_query.rs`; writer — `serializer/hwpx/form.rs`
- 후속: 셀 내부 양식 개체(`set_form_value_in_cell_native`는 존재하나 조회 네이티브 비대칭),
  콤보박스 항목 추가/삭제

### HTML import
LLM이 HTML(표 포함)로 내용을 생성하면 통째로 변환해 삽입한다. 셀 단위 편집을 수십 번
호출하는 것보다 토큰·호출 효율이 훨씬 좋아 에이전트 워크플로에서는 사실상 1순위급.

- `paste_html_native`, `paste_html_in_cell_native`, `paste_html_in_cell_by_path_native`
  — `commands/html_import.rs`

## 2순위 — 객체 생성 / 서식

### 표·그림 신규 생성
지금은 기존 표 편집만 가능하다. 이걸 붙이면 빈 문단에 표를 새로 만들 수 있고,
`DocumentCore::new_empty()`와 합치면 **빈 문서에서 새 HWPX 작성**(`new_document` 도구)도 가능하다.

- `create_table_native`, `create_table_ex_native`, `insert_picture_native`,
  `delete_table_control_native`, `delete_picture_control_native` — `commands/object_ops.rs`
- `new_empty()` — `document_core/mod.rs` (빈 문서 시작점)

### 서식
제목 굵게, 가운데 정렬 같은 기본 꾸미기. 문서 생성 시나리오에서 객체 생성과 세트로 필요해진다.

- `apply_char_format_native`(굵게/크기/폰트), `apply_para_format_native`(정렬/들여쓰기),
  `apply_style_native`, `find_or_create_font_id_native` — `commands/formatting.rs`

## 3순위 — 머리말/꼬리말·각주·미리보기·기타

### 머리말/꼬리말·각주
- `create_header_footer_native`, `get_header_footer_native`, `delete_header_footer_native`,
  `insert_text_in_header_footer_native`, `toggle_hide_header_footer_native` 등
  — `commands/header_footer_ops.rs`
- `insert_text_in_footnote_native`, `delete_footnote_native`, `get_footnote_info_native` 등
  — `commands/footnote_ops.rs`

### 페이지 미리보기
페이지를 SVG/HTML로 렌더링해 반환하면 에이전트가 편집 결과를 **시각적으로 확인**할 수 있다.
`fit_table_to_page` 같은 레이아웃 작업의 검증 루프가 닫힌다.

- `render_page_svg_native`, `render_page_html_native`, `build_page_render_tree`
  — `queries/rendering.rs`

### 기타
- 북마크 CRUD: `get_bookmarks_native`, `add_bookmark_native`, `delete_bookmark_native`,
  `rename_bookmark_native` — `queries/bookmark_query.rs`
- 표 수식: `evaluate_table_formula` — `commands/table_ops.rs` / `table_calc/`
- 내부 클립보드 복사·붙여넣기: `copy_selection_native`, `paste_internal_native`,
  `copy_control_native`, `paste_control_native` — `commands/clipboard.rs`
- HWP 5.0 저장: `export_hwp_with_adapter` — `commands/document.rs`
- PDF 내보내기 (네이티브 렌더링/폰트 의존 추가됨)
- Undo/Redo 스냅샷: `save_snapshot_native` — `commands/document.rs`

## 작업 패턴

각 도구 추가 시:
1. `src/server.rs`에 `#[derive(Deserialize, schemars::JsonSchema)]` 파라미터 구조체 추가
2. `#[tool_router] impl HangulMcp` 블록에 `#[tool(description = ...)]` 메서드 추가 —
   `self.store.with_session(...)`(읽기) 또는 `with_session_edit(...)`(편집)로 감싸고
   해당 `*_native`를 호출, `.map_err(hwp_err)`로 에러 변환
3. `tests/roundtrip.rs`에 라운드트립 단언 추가 (편집 → save → `parse_hwpx` 재파싱)
4. `README.md` 도구 표 갱신
5. `cargo fmt --all` / `cargo clippy --all-targets -- -D warnings` / `cargo nextest run`
