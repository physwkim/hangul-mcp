# hangul-mcp

[rhwp](https://github.com/physwkim/rhwp) 기반 HWPX 문서 편집 MCP(Model Context Protocol) 서버.
.hwpx(또는 .hwp) 파일을 열어 메모리에서 편집하고 HWPX로 저장한다.

## 빌드 / 등록

rhwp는 git rev 고정 의존성이라 별도 체크아웃이 필요 없다. cargo가 받아온다.

```bash
cargo build --release

# Claude Code에 등록
claude mcp add hangul-mcp -- /Users/stevek/codes/hangul-mcp/target/release/hangul-mcp
```

## 워크플로

1. `open_document(path)` → `doc_id` 발급
2. `get_structure(doc_id)` → 편집 좌표(section/para/control) 파악
3. 편집 도구 호출 (메모리에서만 변경)
4. `save_document(doc_id, output_path?)` → HWPX 저장

좌표는 모두 0-기반이고 문자 위치는 char 단위다. rhwp `DocumentCore`와 동일한 좌표계를 쓴다.

## 도구 목록 (29개)

**세션**

| 도구 | 설명 |
|------|------|
| `open_document(path)` | 문서 열기 → `{doc_id, sections, paragraphs_per_section, page_count}` |
| `save_document(doc_id, output_path?)` | HWPX 저장. `output_path` 생략 시 원본 덮어쓰기 |
| `close_document(doc_id)` | 세션 닫기. 미저장 변경은 버려지며 결과에 표시 |
| `list_documents()` | 열린 문서와 dirty 상태 |

**읽기**

| 도구 | 설명 |
|------|------|
| `get_structure(doc_id)` | 구역→문단 인덱스, 미리보기, 표 컨트롤 위치 |
| `get_text(doc_id, section, para_start?, para_end?)` | 문단 텍스트 전문 |
| `get_table(doc_id, section, para, control)` | 표 크기 + 셀별 `cell_idx`/`row`/`col`/텍스트 |

**검색/치환**

| 도구 | 설명 |
|------|------|
| `search_text(doc_id, query, case_sensitive?, include_cells?)` | 전체 매치 위치 |
| `replace_text(doc_id, query, replacement, all?, case_sensitive?)` | 본문 치환 (전체 또는 첫 매치) |

**텍스트/문단 편집**

| 도구 | 설명 |
|------|------|
| `insert_text(doc_id, section, para, char_offset, text)` | 텍스트 삽입 |
| `delete_text(doc_id, section, para, char_offset, count)` | 텍스트 삭제 |
| `insert_paragraph(doc_id, section, para)` | 빈 문단 삽입 |
| `delete_paragraph(doc_id, section, para)` | 문단 삭제 |
| `split_paragraph(doc_id, section, para, char_offset)` | 문단 분할 |
| `merge_paragraph(doc_id, section, para)` | 이전 문단과 병합 |

**표 편집**

| 도구 | 설명 |
|------|------|
| `set_cell_text(doc_id, section, para, control, cell, cell_para?, text)` | 셀 문단 텍스트 교체 |
| `insert_table_row(doc_id, section, para, control, row, below?)` | 행 삽입 |
| `insert_table_column(doc_id, section, para, control, col, right?)` | 열 삽입 |
| `delete_table_row(doc_id, section, para, control, row)` | 행 삭제 |
| `delete_table_column(doc_id, section, para, control, col)` | 열 삭제 |
| `fit_table_to_page(doc_id, section, para, control)` | 표 폭을 본문 폭 이내로 축소 |
| `set_table_column_widths(doc_id, section, para, control, widths)` | 열별 폭 직접 지정 |

**누름틀(필드)** — 양식 문서 자동 채우기

| 도구 | 설명 |
|------|------|
| `list_fields(doc_id)` | 모든 필드 목록 (이름/종류/안내문/현재 값/위치) |
| `get_field_value(doc_id, name)` | 이름으로 필드 값 조회 |
| `set_field_value(doc_id, name, value)` | 이름으로 필드 값 설정 (셀 안 필드 포함) |

**양식 개체(폼)** — 버튼/체크박스/라디오/콤보박스/에디트

| 도구 | 설명 |
|------|------|
| `list_forms(doc_id)` | 본문 양식 개체 목록 (위치 `section/para/control`, 종류/이름/값/캡션/텍스트) |
| `get_form_value(doc_id, section, para, control)` | 양식 개체 값 조회 (종류/값/텍스트/캡션/활성) |
| `get_form_info(doc_id, section, para, control)` | 상세 정보 (크기·색·속성, 콤보 항목 포함) |
| `set_form_value(doc_id, section, para, control, value?, text?, caption?)` | 값 설정 (셋 중 최소 하나) |

## 알려진 제약

- **양식 개체 도구는 rhwp의 양식 개체 직렬화기(`serializer/hwpx/form.rs`)를 필요로 한다.**
  `Cargo.toml`이 고정한 rev가 해당 writer를 포함한 버전이어야 저장 시 양식 개체가 보존된다.
  writer가 없는 rhwp 버전으로 빌드하면 양식 개체가 포함된 문서를 저장할 때 개체가 사라진다.
- **표 셀 안의 양식 개체는 아직 노출하지 않는다.** `list_forms`/`get_form_*`/`set_form_value`는
  본문 문단 수준의 양식 개체만 다룬다. 셀 내부 양식 개체 설정은 rhwp에 네이티브 API가
  있으나(조회 네이티브는 비대칭) 현재 도구로는 제공하지 않는다. (로드맵 참조)
- 누름틀(필드, `ClickHere`)과 양식 개체는 모두 정상 저장·라운드트립된다.

## 테스트

```bash
cargo nextest run
```

`tests/roundtrip.rs`가 픽스처(`tests/fixtures/*.hwpx`, rhwp samples에서 복사)로
open → 편집 → save → `parse_hwpx` 재파싱 라운드트립을 단언한다.

## 비범위 (후속 후보)

- 서식(char/para format, style), 머리말/꼬리말, 각주, HTML import — rhwp `DocumentCore`에 API가 이미 있어 도구만 추가하면 됨
- HWP 5.0 저장(`export_hwp_with_adapter`), PDF 내보내기
- 편집 배치 최적화(`begin_batch_native`/`end_batch_native`)
