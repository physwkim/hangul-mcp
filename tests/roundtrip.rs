//! 라운드트립 통합 테스트: open → edit → save → 재파싱 단언.
//!
//! MCP 도구 메서드를 직접 호출한다 (stdio 전송 계층 제외 전 구간 검증).

use std::path::PathBuf;

use hangul_mcp::cell_format::{BorderSide, BorderStyle, CellFormat, VerticalAlign};
use hangul_mcp::format::{
    Alignment, CellTarget, CharFormat, LineSpacing, LineSpacingKind, ParaFormat,
};
use hangul_mcp::page::{Binding, Orientation, PageSetup};
use hangul_mcp::server::{
    ApplyCellFormatParams, ApplyCharFormatParams, ApplyParaFormatParams, ApplyStyleParams,
    BreakKind, CreateTableParams, DeleteTextParams, DocIdParams, FieldByNameParams,
    FitTableToPageParams, FormAtParams, GetCellFormatParams, GetCharFormatParams, GetTableParams,
    GetTextParams, HangulMcp, HeaderFooterKind, HeaderFooterPages, HeaderFooterParams,
    InsertBreakParams, InsertNoteParams, InsertPictureParams, InsertTableColumnParams,
    InsertTableRowParams, InsertTextParams, MergeParagraphParams, MergeTableCellsParams, NoteKind,
    ObjectAtParams, OpenDocumentParams, ParaParams, ParaTargetParams, RenderPageParams,
    ReplaceTextParams, SaveDocumentParams, SectionParams, SetCellTextParams, SetFieldValueParams,
    SetFormValueParams, SetHeaderFooterParams, SetPageSetupParams, SplitParagraphParams,
    SplitTableCellParams, StyleRef,
};
use rmcp::handler::server::wrapper::Parameters;
use serde_json::Value;

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn temp_out(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("hangul-mcp-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).display().to_string()
}

fn open(server: &HangulMcp, path: &str) -> (String, Value) {
    let out = server
        .open_document(Parameters(OpenDocumentParams {
            path: path.to_string(),
        }))
        .expect("open_document");
    let info: Value = serde_json::from_str(&out).unwrap();
    (info["doc_id"].as_str().unwrap().to_string(), info)
}

#[test]
fn text_insert_replace_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, info) = open(&server, &fixture("ref_text.hwpx"));
    assert!(info["sections"].as_u64().unwrap() >= 1, "구역 없음: {info}");

    // 1) 본문 첫 문단에 마커 삽입
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 0,
            text: "MCP마커원본".to_string(),
            cell: None,
        }))
        .expect("insert_text");

    // 2) 치환: 마커 일부를 바꿔 replace 경로도 검증
    let replaced = server
        .replace_text(Parameters(ReplaceTextParams {
            doc_id: doc_id.clone(),
            query: "마커원본".to_string(),
            replacement: "마커치환".to_string(),
            all: Some(true),
            case_sensitive: Some(false),
        }))
        .expect("replace_text");
    let replaced: Value = serde_json::from_str(&replaced).unwrap();
    assert_eq!(replaced["count"].as_u64(), Some(1), "치환 수: {replaced}");

    // 3) 저장 → rhwp 파서로 재파싱하여 마커 단언
    let out_path = temp_out("text_roundtrip.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(out_path.clone()),
        }))
        .expect("save_document");

    let bytes = std::fs::read(&out_path).unwrap();
    let doc = rhwp::parser::hwpx::parse_hwpx(&bytes).expect("저장본 재파싱");
    let all_text: String = doc.sections[0]
        .paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect();
    assert!(
        all_text.contains("MCP마커치환"),
        "저장본에 치환 마커 없음: {all_text:?}"
    );
    assert!(
        !all_text.contains("마커원본"),
        "치환 전 텍스트가 남음: {all_text:?}"
    );

    // 4) close 후 미저장 변경 경고 없음 (방금 저장했으므로)
    let closed = server
        .close_document(Parameters(DocIdParams {
            doc_id: doc_id.clone(),
        }))
        .expect("close_document");
    let closed: Value = serde_json::from_str(&closed).unwrap();
    assert_eq!(closed["discarded_unsaved_changes"].as_bool(), Some(false));
}

/// get_structure에서 첫 표의 (section, para, control)을 찾는다.
fn find_first_table(structure: &Value) -> Option<(usize, usize, usize)> {
    for sec in structure["sections"].as_array()? {
        for para in sec["paragraphs"].as_array()? {
            if let Some(tables) = para["tables"].as_array() {
                if let Some(t) = tables.first() {
                    return Some((
                        sec["section"].as_u64()? as usize,
                        para["para"].as_u64()? as usize,
                        t["control"].as_u64()? as usize,
                    ));
                }
            }
        }
    }
    None
}

#[test]
fn table_cell_and_row_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_table.hwpx"));

    let structure = server
        .get_structure(Parameters(DocIdParams {
            doc_id: doc_id.clone(),
        }))
        .expect("get_structure");
    let structure: Value = serde_json::from_str(&structure).unwrap();
    let (section, para, control) =
        find_first_table(&structure).expect("픽스처에 표 없음 — ref_table.hwpx 확인");

    let table_before = server
        .get_table(Parameters(GetTableParams {
            doc_id: doc_id.clone(),
            section,
            para,
            control,
        }))
        .expect("get_table");
    let table_before: Value = serde_json::from_str(&table_before).unwrap();
    let rows_before = table_before["rows"].as_u64().unwrap();

    // 1) 셀 0 텍스트 교체
    server
        .set_cell_text(Parameters(SetCellTextParams {
            doc_id: doc_id.clone(),
            section,
            para,
            control,
            cell: 0,
            cell_para: None,
            text: "MCP셀편집".to_string(),
        }))
        .expect("set_cell_text");

    // 2) 행 삽입
    server
        .insert_table_row(Parameters(InsertTableRowParams {
            doc_id: doc_id.clone(),
            section,
            para,
            control,
            row: 0,
            below: Some(true),
        }))
        .expect("insert_table_row");

    // 3) 저장 → 재파싱 단언
    let out_path = temp_out("table_roundtrip.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(out_path.clone()),
        }))
        .expect("save_document");

    let bytes = std::fs::read(&out_path).unwrap();
    let doc = rhwp::parser::hwpx::parse_hwpx(&bytes).expect("저장본 재파싱");
    let para_ref = &doc.sections[section].paragraphs[para];
    let table = para_ref
        .controls
        .iter()
        .find_map(|c| match c {
            rhwp::model::control::Control::Table(t) => Some(t),
            _ => None,
        })
        .expect("저장본에서 표 소실");
    assert_eq!(
        table.row_count as u64,
        rows_before + 1,
        "행 삽입이 저장본에 반영되지 않음"
    );
    let cell_text: String = table.cells[0]
        .paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect();
    assert!(
        cell_text.contains("MCP셀편집"),
        "셀 편집이 저장본에 없음: {cell_text:?}"
    );
}

/// PageDef로부터 본문(텍스트) 폭을 계산한다 (fit 대상 폭).
fn page_content_width(pd: &rhwp::model::page::PageDef) -> u32 {
    let body = rhwp::model::page::PageAreas::from_page_def(pd).body_area;
    (body.right - body.left).max(0) as u32
}

/// 표 JSON에서 (row, col) → cell_idx 매핑을 만든다.
fn cell_index(table_json: &Value, row: u64, col: u64) -> usize {
    table_json["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["row"].as_u64() == Some(row) && c["col"].as_u64() == Some(col))
        .and_then(|c| c["cell_idx"].as_u64())
        .unwrap_or_else(|| panic!("({row},{col}) 셀 없음")) as usize
}

/// 4열 표가 페이지를 넘친 뒤 fit_table_to_page로 본문 폭 안에 들어오는지 검증하고,
/// 데모 산출물 report.hwpx 를 재생성한다. insert_table_column 은 HWP처럼 표를
/// 넓히므로(=페이지 초과 가능), fit 호출이 이를 본문 폭 이내로 되돌려야 한다.
#[test]
fn report_table_fits_page_after_fit() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_table.hwpx"));

    // 표 위치 파악
    let structure = server
        .get_structure(Parameters(DocIdParams {
            doc_id: doc_id.clone(),
        }))
        .unwrap();
    let structure: Value = serde_json::from_str(&structure).unwrap();
    let (section, t_para, t_ctrl) = find_first_table(&structure).expect("픽스처에 표 없음");

    // 3열 → 4열, 2행 → 5행으로 확장
    server
        .insert_table_column(Parameters(InsertTableColumnParams {
            doc_id: doc_id.clone(),
            section,
            para: t_para,
            control: t_ctrl,
            col: 2,
            right: Some(true),
        }))
        .unwrap();
    for _ in 0..3 {
        server
            .insert_table_row(Parameters(InsertTableRowParams {
                doc_id: doc_id.clone(),
                section,
                para: t_para,
                control: t_ctrl,
                row: 1,
                below: Some(true),
            }))
            .unwrap();
    }

    // 셀 채우기
    let data = [
        ["부서", "2분기 목표(억원)", "2분기 실적(억원)", "달성률(%)"],
        ["영업팀", "120", "138", "115"],
        ["마케팅팀", "80", "86", "108"],
        ["개발팀", "60", "57", "95"],
        ["합계", "260", "281", "108"],
    ];
    let table_json = server
        .get_table(Parameters(GetTableParams {
            doc_id: doc_id.clone(),
            section,
            para: t_para,
            control: t_ctrl,
        }))
        .unwrap();
    let table_json: Value = serde_json::from_str(&table_json).unwrap();
    for (r, row) in data.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            let cell = cell_index(&table_json, r as u64, c as u64);
            server
                .set_cell_text(Parameters(SetCellTextParams {
                    doc_id: doc_id.clone(),
                    section,
                    para: t_para,
                    control: t_ctrl,
                    cell,
                    cell_para: None,
                    text: text.to_string(),
                }))
                .unwrap();
        }
    }

    // 표 앞에 8개 문단 삽입 (제목/머리말/소제목), 표는 para 8로 밀림
    for _ in 0..8 {
        server
            .insert_paragraph(Parameters(ParaParams {
                doc_id: doc_id.clone(),
                section,
                para: 0,
            }))
            .unwrap();
    }
    let head = [
        (0usize, "2026년 2분기 사업 실적 보고서"),
        (2, "작성: 경영기획팀   |   작성일: 2026-06-10"),
        (4, "1. 개요"),
        (
            5,
            "본 보고서는 2026년 2분기 주요 부서별 매출 목표와 실적, 달성률을 정리한 것이다.",
        ),
        (7, "2. 부서별 실적 요약"),
    ];
    for (p, text) in head {
        server
            .insert_text(Parameters(InsertTextParams {
                doc_id: doc_id.clone(),
                section,
                para: p,
                char_offset: 0,
                text: text.to_string(),
                cell: None,
            }))
            .unwrap();
    }
    // 표(para 8) 뒤 분석 문단 3개 추가
    for p in 10..=12 {
        server
            .insert_paragraph(Parameters(ParaParams {
                doc_id: doc_id.clone(),
                section,
                para: p,
            }))
            .unwrap();
    }
    let tail = [
        (10usize, "3. 분석 및 향후 계획"),
        (11, "영업팀은 목표 대비 115% 달성하며 전 부서 중 최고 실적을 기록했고, 개발팀은 95%로 소폭 미달했다."),
        (12, "3분기에는 해외 시장 진출과 신제품 출시를 중점 추진하여 전사 목표를 상향 조정한다."),
    ];
    for (p, text) in tail {
        server
            .insert_text(Parameters(InsertTextParams {
                doc_id: doc_id.clone(),
                section,
                para: p,
                char_offset: 0,
                text: text.to_string(),
                cell: None,
            }))
            .unwrap();
    }

    // 표 위치 재파악 후 fit 적용
    let structure = server
        .get_structure(Parameters(DocIdParams {
            doc_id: doc_id.clone(),
        }))
        .unwrap();
    let structure: Value = serde_json::from_str(&structure).unwrap();
    let (section, t_para, t_ctrl) = find_first_table(&structure).expect("재배치 후 표 없음");

    let fit = server
        .fit_table_to_page(Parameters(FitTableToPageParams {
            doc_id: doc_id.clone(),
            section,
            para: t_para,
            control: t_ctrl,
        }))
        .expect("fit_table_to_page");
    let fit: Value = serde_json::from_str(&fit).unwrap();
    assert_eq!(
        fit["changed"].as_bool(),
        Some(true),
        "4열 표는 페이지를 넘쳐 fit이 축소해야 함: {fit}"
    );
    let table_width = fit["tableWidth"].as_u64().unwrap();
    let page_w = fit["pageContentWidth"].as_u64().unwrap();
    assert!(
        table_width <= page_w,
        "fit 후 표 폭({table_width})이 본문 폭({page_w})을 넘음"
    );

    // 데모 산출물 재생성
    let repo_report = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("report.hwpx")
        .display()
        .to_string();
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(repo_report.clone()),
        }))
        .expect("save_document");

    // 저장본 재파싱: 열 폭 합이 본문 폭 이내인지 단언
    let bytes = std::fs::read(&repo_report).unwrap();
    let doc = rhwp::parser::hwpx::parse_hwpx(&bytes).expect("저장본 재파싱");
    let content_w = page_content_width(&doc.sections[section].section_def.page_def);
    let table = doc.sections[section].paragraphs[t_para]
        .controls
        .iter()
        .find_map(|c| match c {
            rhwp::model::control::Control::Table(t) => Some(t),
            _ => None,
        })
        .expect("저장본에서 표 소실");
    let cols_total: u32 = table.get_column_widths().iter().sum();
    assert!(
        cols_total <= content_w,
        "저장본 표 열 폭 합({cols_total})이 본문 폭({content_w})을 넘음"
    );
}

#[test]
fn unknown_doc_id_is_invalid_params() {
    let server = HangulMcp::new();
    let err = server
        .get_structure(Parameters(DocIdParams {
            doc_id: "doc-999".to_string(),
        }))
        .expect_err("없는 doc_id는 에러여야 함");
    assert!(err.message.contains("doc-999"), "에러 메시지: {err:?}");
}

/// list_forms로 지정 종류의 폼 좌표 (section, para, control)를 찾는다.
fn find_form(forms: &Value, form_type: &str) -> (usize, usize, usize) {
    let f = forms["forms"]
        .as_array()
        .expect("forms 배열")
        .iter()
        .find(|f| f["formType"] == form_type)
        .unwrap_or_else(|| panic!("{form_type} 폼 없음: {forms}"));
    (
        f["section"].as_u64().unwrap() as usize,
        f["para"].as_u64().unwrap() as usize,
        f["control"].as_u64().unwrap() as usize,
    )
}

#[test]
fn form_set_value_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("form_fields.hwpx"));

    // 1) 폼 목록 — 5개(모든 타입) 확인, Edit/CheckBox 좌표 확보
    let forms = server
        .list_forms(Parameters(DocIdParams {
            doc_id: doc_id.clone(),
        }))
        .expect("list_forms");
    let forms: Value = serde_json::from_str(&forms).unwrap();
    assert_eq!(
        forms["forms"].as_array().map(|a| a.len()),
        Some(5),
        "폼 5개 기대: {forms}"
    );
    let (e_sec, e_para, e_ctrl) = find_form(&forms, "Edit");
    let (c_sec, c_para, c_ctrl) = find_form(&forms, "CheckBox");

    // 2) Edit 텍스트(문자열 경로) + CheckBox 값(정수 경로) 설정
    let set = server
        .set_form_value(Parameters(SetFormValueParams {
            doc_id: doc_id.clone(),
            section: e_sec,
            para: e_para,
            control: e_ctrl,
            value: None,
            text: Some("MCP에디트값".to_string()),
            caption: None,
            cell: None,
        }))
        .expect("set_form_value (edit)");
    assert_eq!(
        serde_json::from_str::<Value>(&set).unwrap()["ok"].as_bool(),
        Some(true),
        "edit 설정 실패: {set}"
    );
    server
        .set_form_value(Parameters(SetFormValueParams {
            doc_id: doc_id.clone(),
            section: c_sec,
            para: c_para,
            control: c_ctrl,
            value: Some(0), // 원본 CHECKED(1) → 해제(0)
            text: None,
            caption: None,
            cell: None,
        }))
        .expect("set_form_value (checkbox)");

    // 3) 즉시 조회 확인
    let got = server
        .get_form_value(Parameters(FormAtParams {
            doc_id: doc_id.clone(),
            section: e_sec,
            para: e_para,
            control: e_ctrl,
            cell: None,
        }))
        .expect("get_form_value");
    let got: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(
        got["text"].as_str(),
        Some("MCP에디트값"),
        "조회 값 불일치: {got}"
    );

    // 4) 값 누락 시 invalid_params (방어)
    let err = server
        .set_form_value(Parameters(SetFormValueParams {
            doc_id: doc_id.clone(),
            section: e_sec,
            para: e_para,
            control: e_ctrl,
            value: None,
            text: None,
            caption: None,
            cell: None,
        }))
        .expect_err("value/text/caption 모두 없으면 에러여야 함");
    assert!(err.message.contains("최소 하나"), "에러 메시지: {err:?}");

    // 5) 저장 → 재오픈 후 폼 5개 보존 + 값 유지
    let out_path = temp_out("form_roundtrip.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(out_path.clone()),
        }))
        .expect("save_document");

    let (doc_id2, _) = open(&server, &out_path);
    let forms2 = server
        .list_forms(Parameters(DocIdParams {
            doc_id: doc_id2.clone(),
        }))
        .expect("재오픈 list_forms");
    let forms2: Value = serde_json::from_str(&forms2).unwrap();
    assert_eq!(
        forms2["forms"].as_array().map(|a| a.len()),
        Some(5),
        "저장본에서 폼 소실: {forms2}"
    );

    // 저장본에서 좌표 재파악 (인덱스 안정성에 의존하지 않음)
    let (e2_sec, e2_para, e2_ctrl) = find_form(&forms2, "Edit");
    let got2 = server
        .get_form_value(Parameters(FormAtParams {
            doc_id: doc_id2.clone(),
            section: e2_sec,
            para: e2_para,
            control: e2_ctrl,
            cell: None,
        }))
        .expect("재오픈 get_form_value (edit)");
    let got2: Value = serde_json::from_str(&got2).unwrap();
    assert_eq!(
        got2["text"].as_str(),
        Some("MCP에디트값"),
        "저장본에 edit 값 미반영: {got2}"
    );

    let (c2_sec, c2_para, c2_ctrl) = find_form(&forms2, "CheckBox");
    let got_cb = server
        .get_form_value(Parameters(FormAtParams {
            doc_id: doc_id2,
            section: c2_sec,
            para: c2_para,
            control: c2_ctrl,
            cell: None,
        }))
        .expect("재오픈 get_form_value (checkbox)");
    let got_cb: Value = serde_json::from_str(&got_cb).unwrap();
    assert_eq!(
        got_cb["value"].as_i64(),
        Some(0),
        "저장본에 checkbox 값(해제) 미반영: {got_cb}"
    );
}

#[test]
fn field_set_value_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("form_fields.hwpx"));

    // 1) 필드 목록에서 CLICK_HERE 필드 이름 확보
    let fields = server
        .list_fields(Parameters(DocIdParams {
            doc_id: doc_id.clone(),
        }))
        .expect("list_fields");
    let fields: Value = serde_json::from_str(&fields).unwrap();
    let name = fields
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["fieldType"] == "clickhere" && !f["name"].as_str().unwrap_or("").is_empty())
        .and_then(|f| f["name"].as_str())
        .expect("clickhere 필드 없음 — form_fields.hwpx 확인")
        .to_string();

    // 2) 값 설정
    let set = server
        .set_field_value(Parameters(SetFieldValueParams {
            doc_id: doc_id.clone(),
            name: name.clone(),
            value: "MCP필드값".to_string(),
        }))
        .expect("set_field_value");
    let set: Value = serde_json::from_str(&set).unwrap();
    assert_eq!(set["ok"].as_bool(), Some(true), "설정 실패: {set}");

    // 3) 조회로 즉시 확인
    let got = server
        .get_field_value(Parameters(FieldByNameParams {
            doc_id: doc_id.clone(),
            name: name.clone(),
        }))
        .expect("get_field_value");
    let got: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(
        got["value"].as_str(),
        Some("MCP필드값"),
        "조회 값 불일치: {got}"
    );

    // 4) 저장 → 재오픈 후 값이 유지되는지 단언
    let out_path = temp_out("field_roundtrip.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(out_path.clone()),
        }))
        .expect("save_document");

    let (doc_id2, _) = open(&server, &out_path);
    let got2 = server
        .get_field_value(Parameters(FieldByNameParams {
            doc_id: doc_id2,
            name,
        }))
        .expect("재오픈 get_field_value");
    let got2: Value = serde_json::from_str(&got2).unwrap();
    assert_eq!(
        got2["value"].as_str(),
        Some("MCP필드값"),
        "저장본에 필드 값 미반영: {got2}"
    );
}

/// `server_info` 는 빌드에 실제로 쓰인 rhwp 소스를 답해야 한다.
///
/// rhwp 는 git rev 고정 의존성이라 크레이트 버전(0.7.x)만으로는 어떤 커밋인지 알 수 없다.
/// 재빌드 후 서버가 갱신됐는지 이 도구로 확인하므로, 값이 `Cargo.toml` 과 어긋나면
/// 확인 수단 자체가 거짓말을 하게 된다.
#[test]
fn server_info_reports_pinned_rhwp_rev() {
    let server = HangulMcp::new();
    let out = server.server_info().expect("server_info");
    let info: Value = serde_json::from_str(&out).unwrap();

    assert_eq!(info["name"].as_str(), Some("hangul-mcp"));
    assert_eq!(info["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));

    let source = info["rhwp_source"].as_str().expect("rhwp_source");
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .unwrap();
    let rhwp_line = manifest
        .lines()
        .find(|l| l.starts_with("rhwp = "))
        .expect("Cargo.toml 의 rhwp 의존성 줄");

    match rhwp_line.split_once("rev = \"") {
        Some((_, rest)) => {
            let rev = rest.split('"').next().unwrap();
            assert!(
                source.contains(rev),
                "rhwp_source({source}) 가 Cargo.toml 의 rev({rev}) 를 담고 있지 않다"
            );
        }
        None => assert_eq!(
            source, "path",
            "Cargo.toml 이 경로 의존성인데 rhwp_source 가 {source}"
        ),
    }
}

fn section0_text(doc: &rhwp::model::document::Document) -> String {
    doc.sections[0]
        .paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect()
}

/// `.hwp` 경로로 저장하면 HWP 5.0 바이너리가 나와야 하고, 그 뒤 같은 세션을
/// `.hwpx` 로 다시 저장해도 편집 내용이 유지돼야 한다 — HWP 어댑터는 세션 IR 을
/// 제자리에서 바꾸므로 이후 HWPX 저장 경로가 깨지지 않는지가 경계다.
#[test]
fn save_as_hwp_then_hwpx_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 0,
            text: "HWP저장마커".to_string(),
            cell: None,
        }))
        .expect("insert_text");

    let hwp_path = temp_out("save_as.hwp");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(hwp_path.clone()),
        }))
        .expect("save_document .hwp");
    let hwp_bytes = std::fs::read(&hwp_path).unwrap();
    assert!(
        matches!(
            rhwp::parser::detect_format(&hwp_bytes),
            rhwp::parser::FileFormat::Hwp
        ),
        "HWP 바이너리가 아님"
    );
    let hwp_doc = rhwp::parser::parse_hwp(&hwp_bytes).expect("HWP 저장본 재파싱");
    let text = section0_text(&hwp_doc);
    assert!(
        text.contains("HWP저장마커"),
        "HWP 저장본에 마커 없음: {text:?}"
    );

    let hwpx_path = temp_out("save_as_after_hwp.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(hwpx_path.clone()),
        }))
        .expect("save_document .hwpx");
    let hwpx_doc = rhwp::parser::hwpx::parse_hwpx(&std::fs::read(&hwpx_path).unwrap())
        .expect("HWPX 저장본 재파싱");
    let text = section0_text(&hwpx_doc);
    assert!(
        text.contains("HWP저장마커"),
        "HWPX 저장본에 마커 없음: {text:?}"
    );
}

#[test]
fn save_rejects_unknown_extension() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let out_path = temp_out("save_as.docx");
    let err = server
        .save_document(Parameters(SaveDocumentParams {
            doc_id,
            output_path: Some(out_path.clone()),
        }))
        .expect_err(".docx 저장은 거부돼야 함");
    assert!(err.message.contains(".hwp"), "에러 메시지: {err:?}");
    assert!(!PathBuf::from(&out_path).exists(), "거부했는데 파일이 생김");
}

/// 문단 안 char 위치의 글자 모양. char_shapes 는 컨트롤 문자까지 센 UTF-16 위치로
/// 구간을 나누므로, text 의 char 인덱스는 char_offsets 로 옮겨야 한다.
fn char_shape_at<'a>(
    doc: &'a rhwp::model::document::Document,
    para: &rhwp::model::paragraph::Paragraph,
    char_idx: usize,
) -> &'a rhwp::model::style::CharShape {
    let pos = para.char_offsets[char_idx];
    let id = para
        .char_shapes
        .iter()
        .rev()
        .find(|r| r.start_pos <= pos)
        .expect("글자 모양 구간 없음")
        .char_shape_id;
    &doc.doc_info.char_shapes[id as usize]
}

/// HWP ColorRef 는 0x00BBGGRR — 빨강 #FF0000 은 0x0000FF.
const RED_BGR: u32 = 0x0000FF;

/// 본문 범위에 색/진하게/크기/글꼴을 적용하면 HWPX·HWP 저장본 모두에서 그 범위만
/// 바뀌고, 범위 바로 뒤 글자는 원래 서식을 유지해야 한다.
#[test]
fn apply_char_format_body_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 0,
            text: "빨간마커".to_string(),
            cell: None,
        }))
        .expect("insert_text");

    let format = CharFormat {
        bold: Some(true),
        font_size_pt: Some(14.0),
        font_name: Some("hangul-mcp 테스트 글꼴".to_string()),
        text_color: Some("#FF0000".to_string()),
        ..CharFormat::default()
    };
    server
        .apply_char_format(Parameters(ApplyCharFormatParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            start: 0,
            end: 4,
            cell: None,
            format,
        }))
        .expect("apply_char_format");

    let hwpx_path = temp_out("char_format.hwpx");
    let hwp_path = temp_out("char_format.hwp");
    for path in [&hwpx_path, &hwp_path] {
        server
            .save_document(Parameters(SaveDocumentParams {
                doc_id: doc_id.clone(),
                output_path: Some(path.clone()),
            }))
            .expect("save_document");
    }
    let hwpx_doc =
        rhwp::parser::hwpx::parse_hwpx(&std::fs::read(&hwpx_path).unwrap()).expect("HWPX 재파싱");
    let hwp_doc = rhwp::parser::parse_hwp(&std::fs::read(&hwp_path).unwrap()).expect("HWP 재파싱");

    for (label, doc) in [("hwpx", &hwpx_doc), ("hwp", &hwp_doc)] {
        let para = &doc.sections[0].paragraphs[0];
        assert!(
            para.text.starts_with("빨간마커"),
            "{label}: {:?}",
            para.text
        );
        assert!(
            para.text.chars().count() > 4,
            "{label}: 범위 뒤 글자가 없어 경계를 볼 수 없음"
        );
        for i in [0, 3] {
            let cs = char_shape_at(doc, para, i);
            assert_eq!(cs.text_color, RED_BGR, "{label}: [{i}] 색");
            assert!(cs.bold, "{label}: [{i}] 진하게");
            assert_eq!(cs.base_size, 1400, "{label}: [{i}] 크기");
            for lang in 0..7 {
                let font = &doc.doc_info.font_faces[lang][cs.font_ids[lang] as usize];
                assert_eq!(
                    font.name, "hangul-mcp 테스트 글꼴",
                    "{label}: [{i}] 언어 {lang} 글꼴"
                );
            }
        }
        let after = char_shape_at(doc, para, 4);
        assert_ne!(after.text_color, RED_BGR, "{label}: 범위 밖 글자까지 빨강");
        assert_ne!(after.base_size, 1400, "{label}: 범위 밖 글자까지 14pt");
    }
}

#[test]
fn apply_char_format_cell_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_table.hwpx"));
    let structure: Value = serde_json::from_str(
        &server
            .get_structure(Parameters(DocIdParams {
                doc_id: doc_id.clone(),
            }))
            .expect("get_structure"),
    )
    .unwrap();
    let (section, para, control) = find_first_table(&structure).expect("픽스처에 표 없음");
    server
        .set_cell_text(Parameters(SetCellTextParams {
            doc_id: doc_id.clone(),
            section,
            para,
            control,
            cell: 0,
            cell_para: None,
            text: "셀서식테스트".to_string(),
        }))
        .expect("set_cell_text");
    server
        .apply_char_format(Parameters(ApplyCharFormatParams {
            doc_id: doc_id.clone(),
            section,
            para,
            start: 0,
            end: 2,
            cell: Some(CellTarget {
                control,
                cell: 0,
                cell_para: None,
            }),
            format: CharFormat {
                italic: Some(true),
                text_color: Some("#ff0000".to_string()),
                ..CharFormat::default()
            },
        }))
        .expect("apply_char_format (cell)");

    let out_path = temp_out("char_format_cell.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(out_path.clone()),
        }))
        .expect("save_document");
    let doc =
        rhwp::parser::hwpx::parse_hwpx(&std::fs::read(&out_path).unwrap()).expect("저장본 재파싱");
    let table = doc.sections[section].paragraphs[para]
        .controls
        .iter()
        .find_map(|c| match c {
            rhwp::model::control::Control::Table(t) => Some(t),
            _ => None,
        })
        .expect("저장본에서 표 소실");
    let cell_para = &table.cells[0].paragraphs[0];
    assert_eq!(cell_para.text, "셀서식테스트");
    let inside = char_shape_at(&doc, cell_para, 1);
    assert_eq!(inside.text_color, RED_BGR, "셀 범위 색");
    assert!(inside.italic, "셀 범위 기울임");
    let outside = char_shape_at(&doc, cell_para, 2);
    assert_ne!(outside.text_color, RED_BGR, "셀 범위 밖까지 빨강");
}

/// rhwp 는 잘못된 색·빈 범위·범위 밖을 오류 없이 무시하므로 서버가 거부해야 한다.
#[test]
fn apply_char_format_rejects_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let call = |start: usize, end: usize, format: CharFormat| {
        server.apply_char_format(Parameters(ApplyCharFormatParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            start,
            end,
            cell: None,
            format,
        }))
    };
    let red = || CharFormat {
        text_color: Some("#FF0000".to_string()),
        ..CharFormat::default()
    };
    let cases: Vec<(&str, Result<String, rmcp::ErrorData>)> = vec![
        (
            "색 이름",
            call(
                0,
                1,
                CharFormat {
                    text_color: Some("red".to_string()),
                    ..CharFormat::default()
                },
            ),
        ),
        ("빈 서식", call(0, 1, CharFormat::default())),
        ("start == end", call(1, 1, red())),
        ("end 가 문단 길이 초과", call(0, 100_000, red())),
        (
            "장평 범위 밖",
            call(
                0,
                1,
                CharFormat {
                    ratio_percent: Some(10),
                    ..CharFormat::default()
                },
            ),
        ),
        (
            "위/아래첨자 동시",
            call(
                0,
                1,
                CharFormat {
                    superscript: Some(true),
                    subscript: Some(true),
                    ..CharFormat::default()
                },
            ),
        ),
    ];
    for (label, result) in cases {
        let err = result.expect_err(label);
        assert_eq!(
            err.code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "{label}: {err:?}"
        );
    }

    // 거부된 호출은 문서를 바꾸지 않아야 한다.
    let docs: Value = serde_json::from_str(&server.list_documents().unwrap()).unwrap();
    assert_eq!(docs[0]["dirty"].as_bool(), Some(false), "{docs}");
}

fn get_char_runs(server: &HangulMcp, doc_id: &str, para: usize, cell: Option<CellTarget>) -> Value {
    let out = server
        .get_char_format(Parameters(GetCharFormatParams {
            doc_id: doc_id.to_string(),
            section: 0,
            para,
            start: None,
            end: None,
            cell,
        }))
        .expect("get_char_format");
    serde_json::from_str(&out).unwrap()
}

/// get_char_format 은 적용한 범위를 별도 구간으로 돌려주고, 그 구간의 format 은 그대로
/// apply_char_format 입력이 될 수 있어야 한다 (입출력 타입·단위 공유).
#[test]
fn get_char_format_reports_runs_and_roundtrips_into_apply() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 0,
            text: "빨간마커".to_string(),
            cell: None,
        }))
        .expect("insert_text");
    let before = get_char_runs(&server, &doc_id, 0, None);
    let plain_format = before["runs"][0]["format"].clone();

    server
        .apply_char_format(Parameters(ApplyCharFormatParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            start: 0,
            end: 4,
            cell: None,
            format: CharFormat {
                text_color: Some("#FF0000".to_string()),
                font_size_pt: Some(15.5),
                ..CharFormat::default()
            },
        }))
        .expect("apply_char_format");

    let after = get_char_runs(&server, &doc_id, 0, None);
    let runs = after["runs"].as_array().unwrap();
    assert_eq!(runs[0]["start"], 0, "{after}");
    assert_eq!(runs[0]["end"], 4, "{after}");
    assert_eq!(runs[0]["format"]["text_color"], "#ff0000", "{after}");
    assert_eq!(runs[0]["format"]["font_size_pt"], 15.5, "{after}");
    assert_eq!(runs[1]["start"], 4, "{after}");
    assert_eq!(
        runs[1]["format"], plain_format,
        "범위 밖 서식이 바뀜: {after}"
    );
    assert_eq!(
        runs.last().unwrap()["end"],
        after["length"],
        "구간이 문단 끝까지 덮지 않음: {after}"
    );

    // 조회한 원래 서식을 그대로 되돌려 적용하면 범위가 원래 서식으로 돌아와야 한다.
    let restore: CharFormat = serde_json::from_value(plain_format.clone()).unwrap();
    server
        .apply_char_format(Parameters(ApplyCharFormatParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            start: 0,
            end: 4,
            cell: None,
            format: restore,
        }))
        .expect("조회 결과를 apply 입력으로 쓰기");
    let restored = get_char_runs(&server, &doc_id, 0, None);
    assert_eq!(restored["runs"][0]["format"], plain_format, "{restored}");
}

fn get_para(server: &HangulMcp, doc_id: &str, para: usize, cell: Option<CellTarget>) -> Value {
    let out = server
        .get_para_format(Parameters(ParaTargetParams {
            doc_id: doc_id.to_string(),
            section: 0,
            para,
            cell,
        }))
        .expect("get_para_format");
    serde_json::from_str::<Value>(&out).unwrap()["format"].clone()
}

fn sample_para_format() -> ParaFormat {
    ParaFormat {
        alignment: Some(Alignment::Center),
        line_spacing: Some(LineSpacing {
            kind: LineSpacingKind::Percent,
            value: 200.0,
        }),
        margin_left_pt: Some(10.0),
        indent_pt: Some(-5.0),
        spacing_before_pt: Some(6.0),
        keep_with_next: Some(true),
        ..ParaFormat::default()
    }
}

fn assert_sample_para(label: &str, f: &Value) {
    assert_eq!(f["alignment"], "Center", "{label}: {f}");
    assert_eq!(f["line_spacing"]["kind"], "Percent", "{label}: {f}");
    assert_eq!(f["line_spacing"]["value"], 200.0, "{label}: {f}");
    assert_eq!(f["margin_left_pt"], 10.0, "{label}: {f}");
    assert_eq!(f["indent_pt"], -5.0, "{label}: {f}");
    assert_eq!(f["spacing_before_pt"], 6.0, "{label}: {f}");
    assert_eq!(f["keep_with_next"], true, "{label}: {f}");
}

/// 문단 서식은 그 문단에만 적용되고, HWPX·HWP 저장본을 다시 열어도 유지돼야 한다.
#[test]
fn apply_para_format_body_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    // 픽스처는 문단이 하나라, 나눠서 서식이 같은 옆 문단을 만든다.
    server
        .split_paragraph(Parameters(SplitParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 1,
            cell: None,
        }))
        .expect("split_paragraph");
    let neighbour_before = get_para(&server, &doc_id, 1, None);
    server
        .apply_para_format(Parameters(ApplyParaFormatParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            cell: None,
            format: sample_para_format(),
        }))
        .expect("apply_para_format");
    assert_sample_para("메모리", &get_para(&server, &doc_id, 0, None));
    assert_eq!(
        get_para(&server, &doc_id, 1, None),
        neighbour_before,
        "옆 문단 서식이 바뀜"
    );

    for name in ["para_format.hwpx", "para_format.hwp"] {
        let path = temp_out(name);
        server
            .save_document(Parameters(SaveDocumentParams {
                doc_id: doc_id.clone(),
                output_path: Some(path.clone()),
            }))
            .expect("save_document");
        let (reopened, _) = open(&server, &path);
        assert_sample_para(name, &get_para(&server, &reopened, 0, None));
    }
}

#[test]
fn apply_para_format_cell_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_table.hwpx"));
    let structure: Value = serde_json::from_str(
        &server
            .get_structure(Parameters(DocIdParams {
                doc_id: doc_id.clone(),
            }))
            .expect("get_structure"),
    )
    .unwrap();
    let (section, para, control) = find_first_table(&structure).expect("픽스처에 표 없음");
    assert_eq!(section, 0, "테스트 헬퍼는 구역 0 만 본다");
    let cell = || {
        Some(CellTarget {
            control,
            cell: 0,
            cell_para: None,
        })
    };
    server
        .apply_para_format(Parameters(ApplyParaFormatParams {
            doc_id: doc_id.clone(),
            section,
            para,
            cell: cell(),
            format: sample_para_format(),
        }))
        .expect("apply_para_format (cell)");
    assert_sample_para("셀 메모리", &get_para(&server, &doc_id, para, cell()));

    let path = temp_out("para_format_cell.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(path.clone()),
        }))
        .expect("save_document");
    let (reopened, _) = open(&server, &path);
    assert_sample_para("셀 저장본", &get_para(&server, &reopened, para, cell()));
}

#[test]
fn apply_para_format_rejects_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let cases = [
        ("빈 서식", ParaFormat::default()),
        (
            "줄 간격 0%",
            ParaFormat {
                line_spacing: Some(LineSpacing {
                    kind: LineSpacingKind::Percent,
                    value: 0.0,
                }),
                ..ParaFormat::default()
            },
        ),
        (
            "음수 여백",
            ParaFormat {
                margin_left_pt: Some(-1.0),
                ..ParaFormat::default()
            },
        ),
        (
            "NaN 간격",
            ParaFormat {
                spacing_after_pt: Some(f64::NAN),
                ..ParaFormat::default()
            },
        ),
    ];
    for (label, format) in cases {
        let err = server
            .apply_para_format(Parameters(ApplyParaFormatParams {
                doc_id: doc_id.clone(),
                section: 0,
                para: 0,
                cell: None,
                format,
            }))
            .expect_err(label);
        assert_eq!(
            err.code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "{label}: {err:?}"
        );
    }
    let docs: Value = serde_json::from_str(&server.list_documents().unwrap()).unwrap();
    assert_eq!(docs[0]["dirty"].as_bool(), Some(false), "{docs}");
}

/// 글꼴 목록은 언어마다 따로라 같은 이름도 언어마다 ID 가 다를 수 있다. 픽스처는
/// LATIN 목록 앞에 다른 글꼴이 하나 더 있어 "함초롬바탕" 이 HANGUL 1번, LATIN 2번이다.
/// 한 ID 를 7칸에 복사하면 LATIN 이 "함초롬돋움" 을 가리키게 된다.
#[test]
fn apply_char_format_font_name_uses_per_language_ids() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("font_lists_differ.hwpx"));
    server
        .apply_char_format(Parameters(ApplyCharFormatParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            start: 0,
            end: 1,
            cell: None,
            format: CharFormat {
                font_name: Some("함초롬바탕".to_string()),
                ..CharFormat::default()
            },
        }))
        .expect("apply_char_format");

    let out_path = temp_out("font_lists_differ.hwpx");
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.clone(),
            output_path: Some(out_path.clone()),
        }))
        .expect("save_document");
    let doc =
        rhwp::parser::hwpx::parse_hwpx(&std::fs::read(&out_path).unwrap()).expect("저장본 재파싱");
    assert_eq!(
        doc.doc_info.font_faces[1][2].name, "함초롬바탕",
        "픽스처 전제: LATIN 2번이 함초롬바탕"
    );
    let para = &doc.sections[0].paragraphs[0];
    let cs = char_shape_at(&doc, para, 0);
    for lang in 0..7 {
        let name = &doc.doc_info.font_faces[lang][cs.font_ids[lang] as usize].name;
        assert_eq!(
            name, "함초롬바탕",
            "언어 {lang} 글꼴 (ids {:?})",
            cs.font_ids
        );
    }
}

fn para_heading(server: &HangulMcp, doc_id: &str) -> Value {
    para_field(server, doc_id, "heading")
}

fn para_style(server: &HangulMcp, doc_id: &str) -> Value {
    para_field(server, doc_id, "style")
}

fn para_field(server: &HangulMcp, doc_id: &str, key: &str) -> Value {
    let out = server
        .get_para_format(Parameters(ParaTargetParams {
            doc_id: doc_id.to_string(),
            section: 0,
            para: 0,
            cell: None,
        }))
        .expect("get_para_format");
    serde_json::from_str::<Value>(&out).unwrap()[key].clone()
}

/// 이름으로 지정한 스타일이 문단에 걸리고, 저장본에서도 그 스타일의 문단 모양을 쓴다.
#[test]
fn apply_style_by_name_and_id_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let styles: Value = serde_json::from_str(
        &server
            .list_styles(Parameters(DocIdParams {
                doc_id: doc_id.clone(),
            }))
            .expect("list_styles"),
    )
    .unwrap();
    let outline1 = styles["styles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|st| st["name"] == "개요 1")
        .expect("픽스처에 '개요 1' 스타일 없음");
    assert_eq!(outline1["kind"], "para");
    let outline1_id = outline1["id"].as_u64().unwrap() as usize;
    assert_ne!(
        para_style(&server, &doc_id)["id"],
        outline1_id,
        "이미 개요 1"
    );

    let apply = |style: StyleRef| {
        server.apply_style(Parameters(ApplyStyleParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            cell: None,
            style,
        }))
    };
    apply(StyleRef::Name("개요 1".to_string())).expect("apply_style 이름");
    assert_eq!(para_style(&server, &doc_id)["name"], "개요 1");
    // 픽스처 문단은 직접 준 문단 서식이 있어 rhwp 가 문단 모양을 유지한다 — 문단 모양
    // 변화는 단정하지 않고, 적용 직후 상태가 저장 뒤에도 같은지만 본다.
    let format_after = get_para(&server, &doc_id, 0, None);
    let heading = para_heading(&server, &doc_id);

    for name in ["style.hwpx", "style.hwp"] {
        let path = temp_out(name);
        server
            .save_document(Parameters(SaveDocumentParams {
                doc_id: doc_id.clone(),
                output_path: Some(path.clone()),
            }))
            .expect("save_document");
        let (reopened, _) = open(&server, &path);
        assert_eq!(para_style(&server, &reopened)["name"], "개요 1", "{name}");
        assert_eq!(
            get_para(&server, &reopened, 0, None),
            format_after,
            "{name}"
        );
        assert_eq!(para_heading(&server, &reopened), heading, "{name}");
    }

    apply(StyleRef::Id(0)).expect("apply_style id");
    assert_eq!(para_style(&server, &doc_id)["id"], 0);

    for (label, style) in [
        ("없는 이름", StyleRef::Name("없는 스타일".to_string())),
        ("없는 id", StyleRef::Id(10_000)),
    ] {
        let err = apply(style).expect_err(label);
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{label}");
    }
}

/// 직접 서식이 없는 문단(문단 모양 = 스타일 문단 모양)에 "개요 1" 을 걸면 개요 1수준
/// 머리가 붙고, 저장본에서도 유지돼야 한다.
#[test]
fn apply_style_outline_sets_heading() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("form_fields.hwpx"));
    assert_eq!(para_heading(&server, &doc_id)["type"], "None");
    server
        .apply_style(Parameters(ApplyStyleParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            cell: None,
            style: StyleRef::Name("개요 1".to_string()),
        }))
        .expect("apply_style");
    let heading = para_heading(&server, &doc_id);
    assert_eq!(heading["type"], "Outline", "{heading}");
    assert_eq!(heading["level"], 1, "{heading}");

    for name in ["style_outline.hwpx", "style_outline.hwp"] {
        let path = temp_out(name);
        server
            .save_document(Parameters(SaveDocumentParams {
                doc_id: doc_id.clone(),
                output_path: Some(path.clone()),
            }))
            .expect("save_document");
        let (reopened, _) = open(&server, &path);
        assert_eq!(para_heading(&server, &reopened), heading, "{name}");
        assert_eq!(para_style(&server, &reopened)["name"], "개요 1", "{name}");
    }
}

/// 세션을 .hwpx 와 .hwp 로 각각 저장하고 두 저장본을 재파싱해 돌려준다.
fn save_both(
    server: &HangulMcp,
    doc_id: &str,
    stem: &str,
) -> Vec<(&'static str, rhwp::model::document::Document)> {
    let mut out = Vec::new();
    for ext in ["hwpx", "hwp"] {
        let path = temp_out(&format!("{stem}.{ext}"));
        server
            .save_document(Parameters(SaveDocumentParams {
                doc_id: doc_id.to_string(),
                output_path: Some(path.clone()),
            }))
            .unwrap_or_else(|e| panic!("save {ext}: {e:?}"));
        let bytes = std::fs::read(&path).unwrap();
        let doc = if ext == "hwpx" {
            rhwp::parser::hwpx::parse_hwpx(&bytes).expect("HWPX 재파싱")
        } else {
            rhwp::parser::parse_hwp(&bytes).expect("HWP 재파싱")
        };
        out.push((ext, doc));
    }
    out
}

fn table_at(
    doc: &rhwp::model::document::Document,
    para: usize,
    control: usize,
) -> &rhwp::model::table::Table {
    match doc.sections[0].paragraphs[para].controls.get(control) {
        Some(rhwp::model::control::Control::Table(t)) => t,
        other => panic!(
            "문단 {para} 컨트롤 {control} 이 표가 아님: {:?}",
            other.map(|_| ())
        ),
    }
}

fn create_table(server: &HangulMcp, doc_id: &str, rows: u16, cols: u16) -> (usize, usize) {
    let out = server
        .create_table(Parameters(CreateTableParams {
            doc_id: doc_id.to_string(),
            section: 0,
            para: 0,
            char_offset: 0,
            rows,
            cols,
            column_widths_percent: None,
            column_widths: None,
            repeat_header: None,
        }))
        .expect("create_table");
    let v: Value = serde_json::from_str(&out).unwrap();
    (
        v["paraIdx"].as_u64().unwrap() as usize,
        v["controlIdx"].as_u64().unwrap() as usize,
    )
}

/// 새 표 생성 → 셀 텍스트 → 병합까지 HWPX/HWP 저장본 양쪽에 남아야 한다.
#[test]
fn create_table_and_merge_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let (para, control) = create_table(&server, &doc_id, 3, 4);

    let table: Value = serde_json::from_str(
        &server
            .get_table(Parameters(GetTableParams {
                doc_id: doc_id.clone(),
                section: 0,
                para,
                control,
            }))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (table["rows"].as_u64(), table["cols"].as_u64()),
        (Some(3), Some(4))
    );
    server
        .set_cell_text(Parameters(SetCellTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            control,
            cell: cell_index(&table, 2, 3),
            cell_para: None,
            text: "새표마커".to_string(),
        }))
        .expect("set_cell_text");
    server
        .merge_table_cells(Parameters(MergeTableCellsParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            control,
            start_row: 0,
            start_col: 0,
            end_row: 1,
            end_col: 1,
        }))
        .expect("merge_table_cells");

    for (ext, doc) in save_both(&server, &doc_id, "create_merge") {
        let t = table_at(&doc, para, control);
        assert_eq!((t.row_count, t.col_count), (3, 4), "{ext} 표 크기");
        let master = t.cells.iter().find(|c| c.row == 0 && c.col == 0).unwrap();
        assert_eq!(
            (master.row_span, master.col_span),
            (2, 2),
            "{ext} 병합 span"
        );
        assert_eq!(t.cells.len(), 12 - 3, "{ext} 병합 후 셀 수");
        let marked = t.cells.iter().find(|c| c.row == 2 && c.col == 3).unwrap();
        assert_eq!(marked.paragraphs[0].text, "새표마커", "{ext} 셀 텍스트");
    }
}

fn split_cell(
    server: &HangulMcp,
    doc_id: &str,
    (para, control): (usize, usize),
    (row, col): (u16, u16),
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<String, rmcp::ErrorData> {
    server.split_table_cell(Parameters(SplitTableCellParams {
        doc_id: doc_id.to_string(),
        section: 0,
        para,
        control,
        row,
        col,
        rows,
        cols,
    }))
}

/// rows/cols 를 생략하면 병합 해제: 2×2 의 첫 행을 병합했다 풀면 셀 4개가 돌아와야 한다.
#[test]
fn split_table_cell_unmerge_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let at = create_table(&server, &doc_id, 2, 2);
    server
        .merge_table_cells(Parameters(MergeTableCellsParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: at.0,
            control: at.1,
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 1,
        }))
        .expect("merge_table_cells");
    split_cell(&server, &doc_id, at, (0, 0), None, None).expect("병합 해제");

    for (ext, doc) in save_both(&server, &doc_id, "unmerge_cell") {
        let t = table_at(&doc, at.0, at.1);
        assert_eq!((t.row_count, t.col_count), (2, 2), "{ext} 표 크기");
        assert_eq!(t.cells.len(), 4, "{ext} 셀 수");
        assert!(
            t.cells.iter().all(|c| c.row_span == 1 && c.col_span == 1),
            "{ext} 병합이 남아 있음"
        );
    }
}

/// rows/cols 를 지정하면 N×M 나누기: 2×2 의 (1,1) 을 2줄×3칸으로 나누면
/// 행 1개·열 2개가 늘고 그 자리에 1×1 조각 6개가 생겨야 한다.
#[test]
fn split_table_cell_into_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let at = create_table(&server, &doc_id, 2, 2);
    split_cell(&server, &doc_id, at, (1, 1), Some(2), Some(3)).expect("2×3 나누기");

    for (ext, doc) in save_both(&server, &doc_id, "split_cell_into") {
        let t = table_at(&doc, at.0, at.1);
        assert_eq!((t.row_count, t.col_count), (3, 4), "{ext} 나눈 뒤 표 크기");
        let pieces = t
            .cells
            .iter()
            .filter(|c| c.row >= 1 && c.col >= 1 && c.row_span == 1 && c.col_span == 1)
            .count();
        assert_eq!(pieces, 6, "{ext} 나눈 조각 수");
    }
}

#[test]
fn table_tools_reject_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let both_widths = server.create_table(Parameters(CreateTableParams {
        doc_id: doc_id.clone(),
        section: 0,
        para: 0,
        char_offset: 0,
        rows: 2,
        cols: 2,
        column_widths_percent: Some(vec![50.0, 50.0]),
        column_widths: Some(vec![1000, 1000]),
        repeat_header: None,
    }));
    assert!(both_widths.is_err(), "폭 두 종류 동시 지정은 거부돼야 함");
    let bad_pct = server.create_table(Parameters(CreateTableParams {
        doc_id: doc_id.clone(),
        section: 0,
        para: 0,
        char_offset: 0,
        rows: 2,
        cols: 2,
        column_widths_percent: Some(vec![30.0, 30.0]),
        column_widths: None,
        repeat_header: None,
    }));
    assert!(bad_pct.is_err(), "합이 100이 아닌 비율은 거부돼야 함");
    let zero_rows = server.create_table(Parameters(CreateTableParams {
        doc_id: doc_id.clone(),
        section: 0,
        para: 0,
        char_offset: 0,
        rows: 0,
        cols: 2,
        column_widths_percent: None,
        column_widths: None,
        repeat_header: None,
    }));
    assert!(zero_rows.is_err(), "0행 표는 거부돼야 함");

    let (para, control) = create_table(&server, &doc_id, 2, 2);
    let merge_oob = server.merge_table_cells(Parameters(MergeTableCellsParams {
        doc_id: doc_id.clone(),
        section: 0,
        para,
        control,
        start_row: 0,
        start_col: 0,
        end_row: 2,
        end_col: 0,
    }));
    assert!(merge_oob.is_err(), "표 밖 병합 범위는 거부돼야 함");
    for (rows, cols) in [(Some(1), Some(1)), (Some(0), Some(2)), (Some(1), None)] {
        let r = split_cell(&server, &doc_id, (para, control), (0, 0), rows, cols);
        assert!(r.is_err(), "{rows:?}×{cols:?} 나누기는 거부돼야 함");
    }
    let unmerge_plain = split_cell(&server, &doc_id, (para, control), (0, 0), None, None);
    assert!(
        unmerge_plain.is_err(),
        "병합되지 않은 셀의 병합 해제는 거부돼야 함"
    );
}

/// 40×20 px PNG 를 임시 파일로 만든다 (가로:세로 = 2:1).
fn sample_png(name: &str) -> (String, Vec<u8>) {
    let img = image::RgbImage::from_fn(40, 20, |x, _| image::Rgb([(x * 6) as u8, 0, 200]));
    let path = temp_out(name);
    img.save(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

fn picture_params(doc_id: &str, image_path: &str) -> InsertPictureParams {
    InsertPictureParams {
        doc_id: doc_id.to_string(),
        section: 0,
        para: 0,
        char_offset: 0,
        image_path: image_path.to_string(),
        width_mm: None,
        height_mm: None,
        inline: None,
        x_mm: None,
        y_mm: None,
        description: None,
    }
}

fn pictures(doc: &rhwp::model::document::Document) -> Vec<&rhwp::model::image::Picture> {
    doc.sections[0]
        .paragraphs
        .iter()
        .flat_map(|p| &p.controls)
        .filter_map(|c| match c {
            rhwp::model::control::Control::Picture(p) => Some(p.as_ref()),
            _ => None,
        })
        .collect()
}

fn single_picture<'a>(
    ext: &str,
    doc: &'a rhwp::model::document::Document,
) -> &'a rhwp::model::image::Picture {
    let pics = pictures(doc);
    assert_eq!(pics.len(), 1, "{ext} 그림 수");
    pics[0]
}

fn assert_picture_bytes(
    ext: &str,
    doc: &rhwp::model::document::Document,
    pic: &rhwp::model::image::Picture,
    png: &[u8],
) {
    let content = doc
        .bin_data_content
        .iter()
        .find(|c| c.id == pic.image_attr.bin_data_id)
        .unwrap_or_else(|| panic!("{ext} 그림 BinData {} 없음", pic.image_attr.bin_data_id));
    assert_eq!(content.data.load(), png, "{ext} 그림 바이트가 원본과 다름");
}

/// 기본(inline): 폭만 주면 비율이 유지되고 글자처럼 취급돼야 한다.
#[test]
fn insert_picture_inline_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let (png_path, png) = sample_png("inline.png");
    server
        .insert_picture(Parameters(InsertPictureParams {
            width_mm: Some(40.0),
            description: Some("샘플 그림".to_string()),
            ..picture_params(&doc_id, &png_path)
        }))
        .expect("insert_picture");

    let w = (40.0_f64 * 7200.0 / 25.4).round() as u32;
    for (ext, doc) in save_both(&server, &doc_id, "picture_inline") {
        let pic = single_picture(ext, &doc);
        assert!(pic.common.treat_as_char, "{ext} 글자처럼 취급 아님");
        assert_eq!(pic.common.width, w, "{ext} 폭");
        assert!(
            pic.common.height.abs_diff(w / 2) <= 1,
            "{ext} 높이 {} 가 폭의 절반 {} 이 아님",
            pic.common.height,
            w / 2
        );
        assert_eq!(pic.common.description, "샘플 그림", "{ext} 설명");
        assert_picture_bytes(ext, &doc, pic, &png);
    }
}

/// inline=false 이고 위치를 생략하면 본문 영역 왼쪽 위(용지 기준)에 떠 있어야 하고,
/// 크기를 생략하면 96 DPI 원본 크기(px × 75 HWPUNIT)여야 한다.
#[test]
fn insert_picture_floating_defaults_to_body_origin() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let (png_path, png) = sample_png("floating.png");
    server
        .insert_picture(Parameters(InsertPictureParams {
            inline: Some(false),
            ..picture_params(&doc_id, &png_path)
        }))
        .expect("insert_picture");

    for (ext, doc) in save_both(&server, &doc_id, "picture_floating") {
        let body =
            rhwp::model::page::PageAreas::from_page_def(&doc.sections[0].section_def.page_def)
                .body_area;
        let pic = single_picture(ext, &doc);
        assert!(!pic.common.treat_as_char, "{ext} 떠 있는 그림이어야 함");
        assert_eq!(
            (pic.common.horizontal_offset, pic.common.vertical_offset),
            (body.left as u32, body.top as u32),
            "{ext} 위치가 본문 왼쪽 위가 아님"
        );
        assert_eq!(
            (pic.common.width, pic.common.height),
            (40 * 75, 20 * 75),
            "{ext} 원본 크기"
        );
        assert_picture_bytes(ext, &doc, pic, &png);
    }
}

#[test]
fn insert_picture_rejects_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let (png_path, _) = sample_png("reject.png");
    let cases = [
        (
            "그림이 아닌 파일",
            picture_params(&doc_id, &fixture("ref_text.hwpx")),
        ),
        (
            "없는 파일",
            picture_params(&doc_id, &temp_out("no_such.png")),
        ),
        (
            "폭 0",
            InsertPictureParams {
                width_mm: Some(0.0),
                ..picture_params(&doc_id, &png_path)
            },
        ),
        (
            "높이 NaN",
            InsertPictureParams {
                height_mm: Some(f64::NAN),
                ..picture_params(&doc_id, &png_path)
            },
        ),
        (
            "inline 에 위치 지정",
            InsertPictureParams {
                x_mm: Some(10.0),
                ..picture_params(&doc_id, &png_path)
            },
        ),
        (
            "문단 범위 초과",
            InsertPictureParams {
                para: 999,
                ..picture_params(&doc_id, &png_path)
            },
        ),
    ];
    for (label, params) in cases {
        assert!(
            server.insert_picture(Parameters(params)).is_err(),
            "{label}: 거부돼야 함"
        );
    }
    for (ext, doc) in save_both(&server, &doc_id, "picture_rejected") {
        assert!(
            pictures(&doc).is_empty(),
            "{ext}: 거부된 호출이 그림을 남김"
        );
    }
}

/// 본문 위치를 받는 도구는 모두 문단 길이에서 받고 길이+1 에서 거부해야 한다.
/// rhwp 는 범위 밖 위치를 끝으로 당기거나 무시하므로(오류 없음), 거부 쪽 경계가
/// 서버 검사를 직접 증명한다.
#[test]
fn body_offset_tools_reject_past_paragraph_end() {
    let server = HangulMcp::new();
    let (png_path, _) = sample_png("offset_bound.png");
    type Call = fn(&HangulMcp, &str, usize, &str) -> Result<String, rmcp::ErrorData>;
    let tools: [(&str, Call); 6] = [
        ("insert_text", |s, d, off, _| {
            s.insert_text(Parameters(InsertTextParams {
                doc_id: d.to_string(),
                section: 0,
                para: 0,
                char_offset: off,
                text: "X".to_string(),
                cell: None,
            }))
        }),
        ("delete_text", |s, d, off, _| {
            // 끝 위치(off)가 경계가 되도록 마지막 한 글자를 지운다.
            s.delete_text(Parameters(DeleteTextParams {
                doc_id: d.to_string(),
                section: 0,
                para: 0,
                char_offset: off - 1,
                count: 1,
                cell: None,
            }))
        }),
        ("split_paragraph", |s, d, off, _| {
            s.split_paragraph(Parameters(SplitParagraphParams {
                doc_id: d.to_string(),
                section: 0,
                para: 0,
                char_offset: off,
                cell: None,
            }))
        }),
        ("create_table", |s, d, off, _| {
            s.create_table(Parameters(CreateTableParams {
                doc_id: d.to_string(),
                section: 0,
                para: 0,
                char_offset: off,
                rows: 1,
                cols: 1,
                column_widths_percent: None,
                column_widths: None,
                repeat_header: None,
            }))
        }),
        ("insert_picture", |s, d, off, png| {
            s.insert_picture(Parameters(InsertPictureParams {
                char_offset: off,
                ..picture_params(d, png)
            }))
        }),
        ("delete_text(count 0)", |s, d, off, _| {
            s.delete_text(Parameters(DeleteTextParams {
                doc_id: d.to_string(),
                section: 0,
                para: 0,
                char_offset: off.min(1),
                count: 0,
                cell: None,
            }))
        }),
    ];
    for (label, call) in tools {
        let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
        let before = server
            .get_text(Parameters(GetTextParams {
                doc_id: doc_id.clone(),
                section: 0,
                para_start: None,
                para_end: None,
            }))
            .unwrap();
        let len = 12; // ref_text.hwpx 문단 0 의 글자 수
        assert!(
            call(&server, &doc_id, len + 1, &png_path).is_err(),
            "{label}: 길이+1 은 거부돼야 함"
        );
        let after = server
            .get_text(Parameters(GetTextParams {
                doc_id: doc_id.clone(),
                section: 0,
                para_start: None,
                para_end: None,
            }))
            .unwrap();
        assert_eq!(before, after, "{label}: 거부된 호출이 문서를 바꿈");
        if label == "delete_text(count 0)" {
            continue;
        }
        call(&server, &doc_id, len, &png_path)
            .unwrap_or_else(|e| panic!("{label}: 길이 위치는 받아야 함: {e:?}"));
    }
}

/// 문단 0 맨 앞(글자 위치 0)에 글자처럼 취급한 그림을 넣은 세션.
/// 문단 텍스트는 "안녕 Hello 123" 그대로다.
fn doc_with_leading_inline_picture(server: &HangulMcp, name: &str) -> String {
    let (doc_id, _) = open(server, &fixture("ref_text.hwpx"));
    let (png_path, _) = sample_png(name);
    server
        .insert_picture(Parameters(picture_params(&doc_id, &png_path)))
        .expect("insert_picture");
    doc_id
}

fn para_texts(doc: &rhwp::model::document::Document) -> Vec<&str> {
    doc.sections[0]
        .paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect()
}

fn paras_with_picture(doc: &rhwp::model::document::Document) -> Vec<usize> {
    doc.sections[0]
        .paragraphs
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.controls
                .iter()
                .any(|c| matches!(c, rhwp::model::control::Control::Picture(_)))
        })
        .map(|(i, _)| i)
        .collect()
}

/// char_offset 은 글자 위치다: 앞에 인라인 그림이 있어도 split_paragraph(3) 은
/// insert_text(3) 과 같은 자리("안녕 " 뒤)에서 나눠야 한다. rhwp split 은 개체를
/// 한 칸 더 센 논리 위치를 받으므로 변환 없이 넘기면 "안녕" | " Hello 123" 이 된다.
#[test]
fn split_paragraph_offset_counts_text_chars_after_inline_object() {
    let server = HangulMcp::new();
    let doc_id = doc_with_leading_inline_picture(&server, "split_after_pic.png");
    server
        .split_paragraph(Parameters(SplitParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 3,
            cell: None,
        }))
        .expect("split_paragraph");
    for (ext, doc) in save_both(&server, &doc_id, "split_after_pic") {
        assert_eq!(para_texts(&doc), ["안녕 ", "Hello 123"], "{ext}");
        assert_eq!(
            paras_with_picture(&doc),
            [0],
            "{ext} 그림은 앞 문단에 남아야 함"
        );
    }
}

/// 같은 글자 위치(0)의 개체는 분할 뒤쪽으로 간다 — insert_text 가 개체 앞에 넣는 것과 같은 규칙.
#[test]
fn split_paragraph_at_object_position_moves_object_to_second_paragraph() {
    let server = HangulMcp::new();
    let doc_id = doc_with_leading_inline_picture(&server, "split_at_pic.png");
    server
        .split_paragraph(Parameters(SplitParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 0,
            cell: None,
        }))
        .expect("split_paragraph");
    for (ext, doc) in save_both(&server, &doc_id, "split_at_pic") {
        assert_eq!(para_texts(&doc), ["", "안녕 Hello 123"], "{ext}");
        assert_eq!(
            paras_with_picture(&doc),
            [1],
            "{ext} 그림은 뒤 문단으로 가야 함"
        );
    }
}

#[test]
fn create_table_offset_counts_text_chars_after_inline_object() {
    let server = HangulMcp::new();
    let doc_id = doc_with_leading_inline_picture(&server, "table_after_pic.png");
    let out = server
        .create_table(Parameters(CreateTableParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 0,
            char_offset: 3,
            rows: 1,
            cols: 1,
            column_widths_percent: None,
            column_widths: None,
            repeat_header: None,
        }))
        .expect("create_table");
    let v: Value = serde_json::from_str(&out).unwrap();
    let table_para = v["paraIdx"].as_u64().unwrap() as usize;
    for (ext, doc) in save_both(&server, &doc_id, "table_after_pic") {
        let texts = para_texts(&doc);
        // rhwp 는 [앞 글자 | 표 문단 | 빈 이웃 문단 | 뒤 글자] 순서로 넣는다.
        assert_eq!(texts, ["안녕 ", "", "", "Hello 123"], "{ext}");
        assert_eq!(table_para, 1, "{ext} 표 문단 위치");
        table_at(&doc, table_para, 0);
    }
}

fn insert_break(
    server: &HangulMcp,
    doc_id: &str,
    char_offset: usize,
    kind: Option<BreakKind>,
) -> Result<String, rmcp::ErrorData> {
    server.insert_break(Parameters(InsertBreakParams {
        doc_id: doc_id.to_string(),
        section: 0,
        para: 0,
        char_offset,
        kind,
    }))
}

/// 쪽 나누기 뒤 문단은 Page 나누기 종류를 갖고 저장본을 다시 열면 쪽이 하나 늘어야 한다.
#[test]
fn insert_page_break_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, info) = open(&server, &fixture("ref_text.hwpx"));
    let pages_before = info["page_count"].as_u64().unwrap();
    insert_break(&server, &doc_id, 3, None).expect("insert_break");
    for (ext, doc) in save_both(&server, &doc_id, "page_break") {
        assert_eq!(para_texts(&doc), ["안녕 ", "Hello 123"], "{ext}");
        assert_eq!(
            doc.sections[0].paragraphs[1].column_type,
            rhwp::model::paragraph::ColumnBreakType::Page,
            "{ext} 나누기 종류"
        );
        let (_, reopened) = open(&server, &temp_out(&format!("page_break.{ext}")));
        assert_eq!(
            reopened["page_count"].as_u64().unwrap(),
            pages_before + 1,
            "{ext} 쪽 수"
        );
    }
}

#[test]
fn insert_column_break_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    insert_break(&server, &doc_id, 3, Some(BreakKind::Column)).expect("insert_break");
    for (ext, doc) in save_both(&server, &doc_id, "column_break") {
        assert_eq!(para_texts(&doc), ["안녕 ", "Hello 123"], "{ext}");
        assert_eq!(
            doc.sections[0].paragraphs[1].column_type,
            rhwp::model::paragraph::ColumnBreakType::Column,
            "{ext} 나누기 종류"
        );
    }
}

/// 쪽 나누기도 split 계열이라 인라인 개체 뒤 char_offset 을 글자 위치로 받고,
/// 문단 길이를 넘는 위치는 거부해야 한다.
#[test]
fn insert_break_offset_is_text_chars_and_bounded() {
    let server = HangulMcp::new();
    let doc_id = doc_with_leading_inline_picture(&server, "break_after_pic.png");
    assert!(
        insert_break(&server, &doc_id, 13, None).is_err(),
        "문단 길이(12)+1 은 거부돼야 함"
    );
    insert_break(&server, &doc_id, 3, None).expect("insert_break");
    for (ext, doc) in save_both(&server, &doc_id, "break_after_pic") {
        assert_eq!(para_texts(&doc), ["안녕 ", "Hello 123"], "{ext}");
        assert_eq!(paras_with_picture(&doc), [0], "{ext}");
    }
}

fn note_params(
    doc_id: &str,
    char_offset: usize,
    text: &str,
    kind: Option<NoteKind>,
) -> InsertNoteParams {
    InsertNoteParams {
        doc_id: doc_id.to_string(),
        section: 0,
        para: 0,
        char_offset,
        text: text.to_string(),
        kind,
    }
}

/// 문단 0 의 주석 컨트롤을 (종류, 번호, 첫 문단 텍스트) 로, 문단 내 순서대로.
fn notes(doc: &rhwp::model::document::Document) -> Vec<(&'static str, u16, String)> {
    doc.sections[0].paragraphs[0]
        .controls
        .iter()
        .filter_map(|c| match c {
            rhwp::model::control::Control::Footnote(f) => {
                Some(("footnote", f.number, f.paragraphs[0].text.clone()))
            }
            rhwp::model::control::Control::Endnote(e) => {
                Some(("endnote", e.number, e.paragraphs[0].text.clone()))
            }
            _ => None,
        })
        .collect()
}

/// 각주 두 개(뒤쪽을 먼저 넣음)와 미주 하나: 번호는 문서 순서로 다시 매겨지고,
/// 내용은 번호 자리표시("  ") 뒤에 들어가야 한다.
#[test]
fn insert_note_footnote_and_endnote_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    server
        .insert_note(Parameters(note_params(&doc_id, 8, "뒤 각주", None)))
        .expect("footnote 1");
    server
        .insert_note(Parameters(note_params(
            &doc_id,
            2,
            "앞 각주",
            Some(NoteKind::Footnote),
        )))
        .expect("footnote 2");
    server
        .insert_note(Parameters(note_params(
            &doc_id,
            12,
            "미주 내용",
            Some(NoteKind::Endnote),
        )))
        .expect("endnote");

    for (ext, doc) in save_both(&server, &doc_id, "notes") {
        assert_eq!(
            notes(&doc),
            [
                ("footnote", 1, "  앞 각주".to_string()),
                ("footnote", 2, "  뒤 각주".to_string()),
                ("endnote", 1, "  미주 내용".to_string()),
            ],
            "{ext}"
        );
        assert_eq!(
            doc.sections[0].paragraphs[0].text, "안녕 Hello 123",
            "{ext} 본문 텍스트"
        );
    }
}

#[test]
fn insert_note_rejects_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    assert!(server
        .insert_note(Parameters(note_params(&doc_id, 13, "x", None)))
        .is_err());
    assert!(server
        .insert_note(Parameters(note_params(&doc_id, 0, "두\n줄", None)))
        .is_err());
    for (ext, doc) in save_both(&server, &doc_id, "notes_rejected") {
        assert!(notes(&doc).is_empty(), "{ext}: 거부된 호출이 주석을 남김");
    }
}

fn hf_params(
    doc_id: &str,
    kind: HeaderFooterKind,
    pages: Option<HeaderFooterPages>,
) -> HeaderFooterParams {
    HeaderFooterParams {
        doc_id: doc_id.to_string(),
        section: 0,
        kind,
        pages,
    }
}

fn set_hf(
    server: &HangulMcp,
    doc_id: &str,
    kind: HeaderFooterKind,
    pages: Option<HeaderFooterPages>,
    text: &str,
) -> Result<String, rmcp::ErrorData> {
    server.set_header_footer(Parameters(SetHeaderFooterParams {
        doc_id: doc_id.to_string(),
        section: 0,
        kind,
        pages,
        text: text.to_string(),
    }))
}

fn get_hf(
    server: &HangulMcp,
    doc_id: &str,
    kind: HeaderFooterKind,
    pages: Option<HeaderFooterPages>,
) -> Value {
    serde_json::from_str(
        &server
            .get_header_footer(Parameters(hf_params(doc_id, kind, pages)))
            .unwrap(),
    )
    .unwrap()
}

/// 저장본의 (머리말 여부, 적용 쪽, 필드 컨트롤 종류) 목록.
fn header_footers(doc: &rhwp::model::document::Document) -> Vec<(bool, String, Vec<String>)> {
    use rhwp::model::control::Control;
    let fields = |paras: &[rhwp::model::paragraph::Paragraph]| -> Vec<String> {
        paras
            .iter()
            .flat_map(|p| &p.controls)
            .filter_map(|c| match c {
                Control::AutoNumber(a) => Some(format!("{:?}", a.number_type)),
                Control::Field(f) => Some(format!("{:?}", f.field_type)),
                _ => None,
            })
            .collect()
    };
    doc.sections[0]
        .paragraphs
        .iter()
        .flat_map(|p| &p.controls)
        .filter_map(|c| match c {
            Control::Header(h) => Some((true, format!("{:?}", h.apply_to), fields(&h.paragraphs))),
            Control::Footer(f) => Some((false, format!("{:?}", f.apply_to), fields(&f.paragraphs))),
            _ => None,
        })
        .collect()
}

const FOOTER_TEXT: &str = "- {page} / {total_pages} - {file_name}";

/// 머리말(양쪽) + 꼬리말(홀수 쪽, 필드 셋). 꼬리말은 두 번 설정해 교체를 확인한다.
/// 필드는 편집 중엔 마커, 저장본에선 자동 번호/파일 경로 컨트롤인데, 어느 쪽이든
/// get_header_footer 는 같은 자리표시자 표기를 돌려줘야 한다.
#[test]
fn set_header_footer_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let odd = Some(HeaderFooterPages::Odd);
    set_hf(
        &server,
        &doc_id,
        HeaderFooterKind::Header,
        None,
        "보고서 머리말",
    )
    .expect("header");
    set_hf(&server, &doc_id, HeaderFooterKind::Footer, odd, "옛 꼬리말").expect("footer 1");
    set_hf(&server, &doc_id, HeaderFooterKind::Footer, odd, FOOTER_TEXT).expect("footer 2");

    let footer = get_hf(&server, &doc_id, HeaderFooterKind::Footer, odd);
    assert_eq!(footer["text"], FOOTER_TEXT, "편집 중 get: {footer}");
    let both_footer = get_hf(&server, &doc_id, HeaderFooterKind::Footer, None);
    assert_eq!(
        both_footer["exists"], false,
        "양쪽 꼬리말은 없어야 함: {both_footer}"
    );

    for (ext, doc) in save_both(&server, &doc_id, "header_footer") {
        let mut hfs = header_footers(&doc);
        hfs.sort();
        assert_eq!(
            hfs,
            [
                (
                    false,
                    "Odd".to_string(),
                    vec![
                        "Page".to_string(),
                        "TotalPage".to_string(),
                        "Path".to_string()
                    ]
                ),
                (true, "Both".to_string(), vec![]),
            ],
            "{ext}"
        );
        let (reopened, _) = open(&server, &temp_out(&format!("header_footer.{ext}")));
        let header = get_hf(&server, &reopened, HeaderFooterKind::Header, None);
        assert_eq!(
            header["text"], "보고서 머리말",
            "{ext} 다시 연 머리말: {header}"
        );
        let footer = get_hf(&server, &reopened, HeaderFooterKind::Footer, odd);
        assert_eq!(
            footer["text"], FOOTER_TEXT,
            "{ext} 다시 연 꼬리말: {footer}"
        );
    }

    server
        .delete_header_footer(Parameters(hf_params(
            &doc_id,
            HeaderFooterKind::Header,
            None,
        )))
        .expect("delete header");
    let header = get_hf(&server, &doc_id, HeaderFooterKind::Header, None);
    assert_eq!(header["exists"], false, "삭제 후: {header}");
}

#[test]
fn set_header_footer_rejects_multiline() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    assert!(set_hf(
        &server,
        &doc_id,
        HeaderFooterKind::Header,
        None,
        "한\n줄 아님"
    )
    .is_err());
    let header = get_hf(&server, &doc_id, HeaderFooterKind::Header, None);
    assert_eq!(
        header["exists"], false,
        "거부된 호출이 머리말을 만듦: {header}"
    );
}

fn render(
    server: &HangulMcp,
    doc_id: &str,
    page: u32,
    out: &str,
) -> Result<String, rmcp::ErrorData> {
    server.render_page(Parameters(RenderPageParams {
        doc_id: doc_id.to_string(),
        page,
        output_path: out.to_string(),
    }))
}

/// 편집 중(미저장) 상태가 렌더링에 반영돼야 한다: 쪽 나누기 뒤 둘째 쪽을 그릴 수 있고,
/// 넣은 글자가 SVG 글자 요소로 나와야 한다.
#[test]
fn render_page_svg_and_pdf_reflect_unsaved_edits() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    insert_break(&server, &doc_id, 3, None).expect("insert_break");
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para: 1,
            char_offset: 0,
            text: "쪽".to_string(),
            cell: None,
        }))
        .expect("insert_text");

    let svg_path = temp_out("page1.svg");
    let out: Value =
        serde_json::from_str(&render(&server, &doc_id, 1, &svg_path).expect("svg")).unwrap();
    assert_eq!(out["page_count"], 2, "{out}");
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    assert!(
        svg.starts_with("<svg"),
        "SVG 아님: {}",
        &svg[..80.min(svg.len())]
    );
    assert!(svg.contains(">쪽<"), "둘째 쪽 SVG 에 넣은 글자가 없음");

    let pdf_path = temp_out("page0.pdf");
    render(&server, &doc_id, 0, &pdf_path).expect("pdf");
    assert!(
        std::fs::read(&pdf_path).unwrap().starts_with(b"%PDF-"),
        "PDF 아님"
    );

    let docs: Value = serde_json::from_str(&server.list_documents().unwrap()).unwrap();
    let entry = docs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["doc_id"] == doc_id.as_str())
        .unwrap();
    assert_eq!(entry["dirty"], true, "렌더링이 미저장 표시를 지움: {entry}");
}

#[test]
fn render_page_rejects_bad_page_and_extension() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let png = temp_out("page.png");
    assert!(
        render(&server, &doc_id, 0, &png).is_err(),
        ".png 는 거부돼야 함"
    );
    assert!(!PathBuf::from(&png).exists());
    let svg = temp_out("page_oob.svg");
    assert!(
        render(&server, &doc_id, 1, &svg).is_err(),
        "1쪽 문서의 page 1 은 거부돼야 함"
    );
    assert!(!PathBuf::from(&svg).exists());
}

fn para0_text(server: &HangulMcp, doc_id: &str) -> String {
    let out: Value = serde_json::from_str(
        &server
            .get_text(Parameters(GetTextParams {
                doc_id: doc_id.to_string(),
                section: 0,
                para_start: Some(0),
                para_end: Some(0),
            }))
            .unwrap(),
    )
    .unwrap();
    out.to_string()
}

fn insert_at0(server: &HangulMcp, doc_id: &str, text: &str) {
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.to_string(),
            section: 0,
            para: 0,
            char_offset: 0,
            text: text.to_string(),
            cell: None,
        }))
        .expect("insert_text");
}

fn history(out: Result<String, rmcp::ErrorData>) -> (u64, u64, bool) {
    let v: Value = serde_json::from_str(&out.expect("undo/redo")).unwrap();
    (
        v["undo_available"].as_u64().unwrap(),
        v["redo_available"].as_u64().unwrap(),
        v["dirty"].as_bool().unwrap(),
    )
}

fn undo(server: &HangulMcp, doc_id: &str) -> Result<String, rmcp::ErrorData> {
    server.undo(Parameters(DocIdParams {
        doc_id: doc_id.to_string(),
    }))
}

fn redo(server: &HangulMcp, doc_id: &str) -> Result<String, rmcp::ErrorData> {
    server.redo(Parameters(DocIdParams {
        doc_id: doc_id.to_string(),
    }))
}

fn save_to(server: &HangulMcp, doc_id: &str, name: &str) {
    server
        .save_document(Parameters(SaveDocumentParams {
            doc_id: doc_id.to_string(),
            output_path: Some(temp_out(name)),
        }))
        .expect("save");
}

/// 미저장 표시는 "지금 상태 == 저장한 상태" 로 정해진다: 저장 시점으로 undo/redo 해
/// 돌아오면 깨끗해지고, 거기서 벗어나면 다시 dirty 다.
#[test]
fn undo_redo_restore_text_and_track_saved_state() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let original = para0_text(&server, &doc_id);
    insert_at0(&server, &doc_id, "가");
    let edited = para0_text(&server, &doc_id);

    assert_eq!(
        history(undo(&server, &doc_id)),
        (0, 1, false),
        "열기 상태로 undo"
    );
    assert_eq!(para0_text(&server, &doc_id), original);
    assert_eq!(history(redo(&server, &doc_id)), (1, 0, true), "redo");
    assert_eq!(para0_text(&server, &doc_id), edited);

    save_to(&server, &doc_id, "undo_saved.hwpx");
    assert_eq!(
        history(undo(&server, &doc_id)),
        (0, 1, true),
        "저장 뒤 undo 는 dirty"
    );
    assert_eq!(
        history(redo(&server, &doc_id)),
        (1, 0, false),
        "저장 시점으로 redo"
    );
}

/// 되돌린 뒤 새 편집: 다시 할 기록은 사라지고, 저장 때와 같은 깊이여도 다른 상태라 dirty.
#[test]
fn edit_after_undo_drops_redo_and_is_dirty_at_saved_depth() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    insert_at0(&server, &doc_id, "가");
    save_to(&server, &doc_id, "undo_branch.hwpx");
    undo(&server, &doc_id).expect("undo");
    insert_at0(&server, &doc_id, "나");
    assert!(
        redo(&server, &doc_id).is_err(),
        "새 편집 뒤 redo 는 없어야 함"
    );
    let docs: Value = serde_json::from_str(&server.list_documents().unwrap()).unwrap();
    let entry = docs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["doc_id"] == doc_id.as_str())
        .unwrap();
    assert_eq!(
        entry["dirty"], true,
        "저장 깊이와 같지만 다른 상태: {entry}"
    );
}

#[test]
fn undo_redo_on_empty_history_is_rejected() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    assert!(undo(&server, &doc_id).is_err());
    assert!(redo(&server, &doc_id).is_err());
}

/// 기록 상한(20): 21번 편집하면 가장 오래된 한 단계는 되돌릴 수 없다.
#[test]
fn undo_history_is_capped() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    insert_at0(&server, &doc_id, "0");
    let after_first = para0_text(&server, &doc_id);
    for i in 1..=hangul_mcp::store::MAX_UNDO {
        insert_at0(&server, &doc_id, &i.to_string());
    }
    for _ in 0..hangul_mcp::store::MAX_UNDO {
        undo(&server, &doc_id).expect("상한 안의 undo");
    }
    assert!(
        undo(&server, &doc_id).is_err(),
        "상한을 넘는 undo 는 없어야 함"
    );
    assert_eq!(para0_text(&server, &doc_id), after_first);
}

/// 여러 rhwp 호출로 이뤄진 도구(그림 삽입 + 글자처럼 취급, 머리말 삭제 + 생성 + 입력)도
/// undo 한 번에 통째로 되돌아가야 한다.
#[test]
fn undo_reverts_multi_step_tools_as_one_unit() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    set_hf(
        &server,
        &doc_id,
        HeaderFooterKind::Footer,
        None,
        "옛 꼬리말",
    )
    .expect("footer 1");
    set_hf(
        &server,
        &doc_id,
        HeaderFooterKind::Footer,
        None,
        "새 꼬리말 {page}",
    )
    .expect("footer 2");
    undo(&server, &doc_id).expect("undo footer");
    let footer = get_hf(&server, &doc_id, HeaderFooterKind::Footer, None);
    assert_eq!(footer["text"], "옛 꼬리말", "{footer}");

    let (png_path, _) = sample_png("undo_pic.png");
    server
        .insert_picture(Parameters(picture_params(&doc_id, &png_path)))
        .expect("insert_picture");
    undo(&server, &doc_id).expect("undo picture");
    for (ext, doc) in save_both(&server, &doc_id, "undo_pic") {
        assert!(pictures(&doc).is_empty(), "{ext}: undo 뒤 그림이 남음");
    }
}

/// 첫 표의 (0,0) 셀과 표 바로 뒤 본문 문단에 같은 글자를 둔 세션.
/// 반환: (doc_id, 표 문단, 표 컨트롤, 셀 인덱스). 본문 쪽 문단은 표 문단 + 1.
fn doc_with_cell_and_body_marker(server: &HangulMcp) -> (String, usize, usize, usize) {
    let (doc_id, _) = open(server, &fixture("ref_table.hwpx"));
    let structure: Value = serde_json::from_str(
        &server
            .get_structure(Parameters(DocIdParams {
                doc_id: doc_id.clone(),
            }))
            .unwrap(),
    )
    .unwrap();
    let (section, para, control) = find_first_table(&structure).expect("픽스처에 표 없음");
    assert_eq!(section, 0, "테스트 헬퍼는 구역 0 만 본다");
    let table: Value = serde_json::from_str(
        &server
            .get_table(Parameters(GetTableParams {
                doc_id: doc_id.clone(),
                section,
                para,
                control,
            }))
            .unwrap(),
    )
    .unwrap();
    let cell = cell_index(&table, 0, 0);
    server
        .set_cell_text(Parameters(SetCellTextParams {
            doc_id: doc_id.clone(),
            section,
            para,
            control,
            cell,
            cell_para: None,
            text: "마커A".to_string(),
        }))
        .unwrap();
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section,
            para: para + 1,
            char_offset: 0,
            text: "마커A".to_string(),
            cell: None,
        }))
        .unwrap();
    (doc_id, para, control, cell)
}

fn replace(
    server: &HangulMcp,
    doc_id: &str,
    query: &str,
    all: bool,
) -> Result<String, rmcp::ErrorData> {
    server.replace_text(Parameters(ReplaceTextParams {
        doc_id: doc_id.to_string(),
        query: query.to_string(),
        replacement: "바뀜".to_string(),
        all: Some(all),
        case_sensitive: None,
    }))
}

fn cell_and_body(
    doc: &rhwp::model::document::Document,
    (para, control, cell): (usize, usize, usize),
) -> (String, String) {
    (
        table_at(doc, para, control).cells[cell].paragraphs[0]
            .text
            .clone(),
        doc.sections[0].paragraphs[para + 1].text.clone(),
    )
}

/// all=false 는 search_text 와 같은 문서 순서의 첫 매치(표 셀)를 바꿔야 한다 —
/// rhwp replace_one_native 는 본문만 찾아 셀을 건너뛰고 본문 쪽을 바꾼다.
#[test]
fn replace_text_first_match_includes_table_cells() {
    let server = HangulMcp::new();
    let (doc_id, para, control, cell) = doc_with_cell_and_body_marker(&server);
    replace(&server, &doc_id, "마커A", false).expect("replace first");
    for (ext, doc) in save_both(&server, &doc_id, "replace_first") {
        assert_eq!(
            cell_and_body(&doc, (para, control, cell)),
            ("바뀜".to_string(), "마커A".to_string()),
            "{ext}"
        );
    }
}

#[test]
fn replace_text_all_includes_table_cells() {
    let server = HangulMcp::new();
    let (doc_id, para, control, cell) = doc_with_cell_and_body_marker(&server);
    let out: Value =
        serde_json::from_str(&replace(&server, &doc_id, "마커A", true).unwrap()).unwrap();
    assert_eq!(out["count"], 2, "{out}");
    for (ext, doc) in save_both(&server, &doc_id, "replace_all") {
        assert_eq!(
            cell_and_body(&doc, (para, control, cell)),
            ("바뀜".to_string(), "바뀜".to_string()),
            "{ext}"
        );
    }
}

/// 일치가 없거나 빈 검색어면 오류이고, 되돌리기 기록을 남기지 않는다:
/// 바로 다음 undo 는 실패한 치환이 아니라 그 전의 본문 insert_text 를 되돌린다.
#[test]
fn replace_text_without_match_is_rejected_and_not_recorded() {
    let server = HangulMcp::new();
    let (doc_id, para, control, cell) = doc_with_cell_and_body_marker(&server);
    for (query, all) in [("없는글자", true), ("없는글자", false), ("", true)] {
        assert!(
            replace(&server, &doc_id, query, all).is_err(),
            "{query:?} all={all}"
        );
    }
    undo(&server, &doc_id).expect("undo");
    for (ext, doc) in save_both(&server, &doc_id, "replace_none") {
        assert_eq!(
            cell_and_body(&doc, (para, control, cell)),
            ("마커A".to_string(), String::new()),
            "{ext}"
        );
    }
}

/// 첫 표의 (0,0) 셀 좌표: (doc_id, 표 문단, CellTarget 만드는 함수 재료 (control, cell)).
fn first_table_cell(server: &HangulMcp) -> (String, usize, usize, usize) {
    let (doc_id, _) = open(server, &fixture("ref_table.hwpx"));
    let structure: Value = serde_json::from_str(
        &server
            .get_structure(Parameters(DocIdParams {
                doc_id: doc_id.clone(),
            }))
            .unwrap(),
    )
    .unwrap();
    let (section, para, control) = find_first_table(&structure).expect("픽스처에 표 없음");
    assert_eq!(section, 0, "테스트 헬퍼는 구역 0 만 본다");
    let table: Value = serde_json::from_str(
        &server
            .get_table(Parameters(GetTableParams {
                doc_id: doc_id.clone(),
                section,
                para,
                control,
            }))
            .unwrap(),
    )
    .unwrap();
    (doc_id, para, control, cell_index(&table, 0, 0))
}

fn cell_paras(
    doc: &rhwp::model::document::Document,
    para: usize,
    control: usize,
    cell: usize,
) -> Vec<String> {
    table_at(doc, para, control).cells[cell]
        .paragraphs
        .iter()
        .map(|p| p.text.clone())
        .collect()
}

/// insert_text/delete_text/split_paragraph/merge_paragraph 의 cell 대상은 표 셀 문단을
/// 고치고 본문 문단은 건드리지 않는다.
#[test]
fn text_editing_in_table_cell_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, para, control, cell) = first_table_cell(&server);
    let target = |cell_para| {
        Some(CellTarget {
            control,
            cell,
            cell_para,
        })
    };
    let (_, before) = save_both(&server, &doc_id, "cell_edit_before").remove(0);
    let body_before = para_texts(&before);
    let insert = |offset, text: &str| {
        server.insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: offset,
            text: text.to_string(),
            cell: target(None),
        }))
    };
    insert(0, "가나다").expect("insert in cell");
    insert(1, "X").expect("insert in cell middle");
    server
        .delete_text(Parameters(DeleteTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: 2,
            count: 1,
            cell: target(None),
        }))
        .expect("delete in cell");
    server
        .split_paragraph(Parameters(SplitParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: 2,
            cell: target(None),
        }))
        .expect("split in cell");
    for (ext, doc) in save_both(&server, &doc_id, "cell_edit_split") {
        assert_eq!(
            cell_paras(&doc, para, control, cell),
            ["가X", "다"],
            "{ext}"
        );
        assert_eq!(para_texts(&doc), body_before, "{ext}: 본문이 바뀜");
    }
    server
        .merge_paragraph(Parameters(MergeParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            cell: target(Some(1)),
        }))
        .expect("merge in cell");
    for (ext, doc) in save_both(&server, &doc_id, "cell_edit_merge") {
        assert_eq!(cell_paras(&doc, para, control, cell), ["가X다"], "{ext}");
    }
}

/// 셀 문단 길이를 넘는 위치는 rhwp 가 조용히 당기지 않도록 오류여야 하고, 아무것도 바꾸지 않는다.
#[test]
fn text_editing_in_table_cell_rejects_past_cell_paragraph_end() {
    let server = HangulMcp::new();
    let (doc_id, para, control, cell) = first_table_cell(&server);
    let target = || {
        Some(CellTarget {
            control,
            cell,
            cell_para: None,
        })
    };
    server
        .insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: 0,
            text: "가나".to_string(),
            cell: target(),
        }))
        .unwrap();
    let errors = [
        server.insert_text(Parameters(InsertTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: 3,
            text: "X".to_string(),
            cell: target(),
        })),
        server.delete_text(Parameters(DeleteTextParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: 1,
            count: 2,
            cell: target(),
        })),
        server.split_paragraph(Parameters(SplitParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            char_offset: 3,
            cell: target(),
        })),
        server.merge_paragraph(Parameters(MergeParagraphParams {
            doc_id: doc_id.clone(),
            section: 0,
            para,
            cell: target(),
        })),
    ];
    for (i, r) in errors.iter().enumerate() {
        assert!(r.is_err(), "#{i} 가 성공함: {r:?}");
    }
    for (ext, doc) in save_both(&server, &doc_id, "cell_edit_reject") {
        assert_eq!(cell_paras(&doc, para, control, cell), ["가나"], "{ext}");
    }
}

fn structure(server: &HangulMcp, doc_id: &str) -> Value {
    serde_json::from_str(
        &server
            .get_structure(Parameters(DocIdParams {
                doc_id: doc_id.to_string(),
            }))
            .unwrap(),
    )
    .unwrap()
}

/// get_structure 문단 para 의 objects 를 (control, kind, char_offset) 로.
fn objects(server: &HangulMcp, doc_id: &str, para: usize) -> Vec<(usize, String, usize)> {
    structure(server, doc_id)["sections"][0]["paragraphs"][para]["objects"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|o| {
                    (
                        o["control"].as_u64().unwrap() as usize,
                        o["kind"].as_str().unwrap().to_string(),
                        o["char_offset"].as_u64().unwrap() as usize,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn delete_object(
    server: &HangulMcp,
    doc_id: &str,
    para: usize,
    control: usize,
) -> Result<String, rmcp::ErrorData> {
    server.delete_object(Parameters(ObjectAtParams {
        doc_id: doc_id.to_string(),
        section: 0,
        para,
        control,
    }))
}

/// 각주 둘과 글자처럼 취급한 그림: get_structure 가 문서 위치와 함께 알리고,
/// delete_object 는 각주 번호를 다시 매기며 본문 텍스트를 건드리지 않는다.
#[test]
fn delete_object_footnote_and_picture_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    for (offset, text) in [(2, "앞"), (8, "뒤")] {
        server
            .insert_note(Parameters(note_params(&doc_id, offset, text, None)))
            .unwrap();
    }
    let (png, _) = sample_png("delete_object.png");
    let mut pic = picture_params(&doc_id, &png);
    pic.char_offset = 5;
    server.insert_picture(Parameters(pic)).unwrap();

    let objs = objects(&server, &doc_id, 0);
    let kinds: Vec<_> = objs.iter().map(|(_, k, o)| (k.as_str(), *o)).collect();
    assert_eq!(
        kinds,
        [("footnote", 2), ("picture", 5), ("footnote", 8)],
        "{objs:?}"
    );

    delete_object(&server, &doc_id, 0, objs[0].0).expect("delete footnote");
    let picture = objects(&server, &doc_id, 0)
        .into_iter()
        .find(|(_, k, _)| k == "picture")
        .unwrap();
    delete_object(&server, &doc_id, 0, picture.0).expect("delete picture");

    for (ext, doc) in save_both(&server, &doc_id, "delete_object") {
        assert_eq!(notes(&doc), [("footnote", 1, "  뒤".to_string())], "{ext}");
        assert!(pictures(&doc).is_empty(), "{ext}: 그림이 남음");
        assert_eq!(
            doc.sections[0].paragraphs[0].text, "안녕 Hello 123",
            "{ext} 본문 텍스트"
        );
    }
}

#[test]
fn delete_object_table_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let (para, control) = create_table(&server, &doc_id, 2, 2);
    assert_eq!(
        objects(&server, &doc_id, para)
            .iter()
            .map(|(c, k, _)| (*c, k.as_str()))
            .collect::<Vec<_>>(),
        [(control, "table")]
    );
    delete_object(&server, &doc_id, para, control).expect("delete table");
    for (ext, doc) in save_both(&server, &doc_id, "delete_table") {
        let tables = doc.sections[0]
            .paragraphs
            .iter()
            .flat_map(|p| &p.controls)
            .filter(|c| matches!(c, rhwp::model::control::Control::Table(_)))
            .count();
        assert_eq!(tables, 0, "{ext}: 표가 남음");
    }
}

/// 구역 정의 같은 비개체 컨트롤이나 없는 컨트롤은 지우지 않는다.
#[test]
fn delete_object_rejects_non_objects() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let controls_total = structure(&server, &doc_id)["sections"][0]["paragraphs"][0]
        ["controls_total"]
        .as_u64()
        .unwrap_or(0) as usize;
    assert!(
        controls_total > 0,
        "문단 0 에 구역 정의 컨트롤이 있어야 한다"
    );
    assert!(objects(&server, &doc_id, 0).is_empty());
    for control in 0..=controls_total {
        assert!(
            delete_object(&server, &doc_id, 0, control).is_err(),
            "컨트롤 {control} 이 지워짐"
        );
    }
    assert!(delete_object(&server, &doc_id, 99, 0).is_err());
}

fn page_setup(server: &HangulMcp, doc_id: &str) -> PageSetup {
    serde_json::from_str(
        &server
            .get_page_setup(Parameters(SectionParams {
                doc_id: doc_id.to_string(),
                section: 0,
            }))
            .unwrap(),
    )
    .unwrap()
}

fn set_page(server: &HangulMcp, doc_id: &str, setup: PageSetup) -> Result<String, rmcp::ErrorData> {
    server.set_page_setup(Parameters(SetPageSetupParams {
        doc_id: doc_id.to_string(),
        section: 0,
        setup,
    }))
}

/// SVG 루트의 width/height 속성 (px).
fn svg_size(path: &str) -> (f64, f64) {
    let svg = std::fs::read_to_string(path).unwrap();
    let root = &svg[svg.find("<svg").unwrap()..];
    let root = &root[..root.find('>').unwrap()];
    let attr = |name: &str| -> f64 {
        let key = format!(" {name}=\"");
        let start = root.find(&key).unwrap() + key.len();
        let end = start + root[start..].find('"').unwrap();
        root[start..end].trim_end_matches("px").parse().unwrap()
    };
    (attr("width"), attr("height"))
}

/// 가로 방향·여백 변경이 저장 뒤에도 남고, 지정하지 않은 항목은 그대로이며,
/// 미저장 상태 렌더링도 가로 쪽으로 그린다.
#[test]
fn page_setup_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let before = page_setup(&server, &doc_id);
    assert_eq!(
        before.orientation,
        Some(Orientation::Portrait),
        "{before:?}"
    );

    let after = PageSetup {
        orientation: Some(Orientation::Landscape),
        margin_left_mm: Some(20.0),
        margin_top_mm: Some(12.5),
        binding: Some(Binding::Duplex),
        ..before.clone()
    };
    let out: PageSetup = serde_json::from_str(
        &set_page(
            &server,
            &doc_id,
            PageSetup {
                orientation: after.orientation,
                margin_left_mm: after.margin_left_mm,
                margin_top_mm: after.margin_top_mm,
                binding: after.binding,
                ..Default::default()
            },
        )
        .expect("set_page_setup"),
    )
    .unwrap();
    assert_eq!(out, after);
    assert_eq!(page_setup(&server, &doc_id), after);

    let svg = temp_out("page_setup.svg");
    render(&server, &doc_id, 0, &svg).expect("render");
    let (w, h) = svg_size(&svg);
    assert!(w > h, "가로 방향인데 SVG 가 {w}×{h}");

    for (ext, doc) in save_both(&server, &doc_id, "page_setup") {
        let reopened = PageSetup::from_page_def(&doc.sections[0].section_def.page_def);
        assert_eq!(reopened, after, "{ext}");
    }
}

/// 본문 영역이 사라지는 여백, 빈 설정, 범위 밖 값, 없는 구역은 오류이고 아무것도 바꾸지 않는다.
#[test]
fn set_page_setup_rejects_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let before = page_setup(&server, &doc_id);
    let bad = [
        PageSetup {
            margin_left_mm: Some(150.0),
            margin_right_mm: Some(100.0),
            ..Default::default()
        },
        PageSetup {
            margin_header_mm: Some(150.0),
            margin_footer_mm: Some(150.0),
            ..Default::default()
        },
        PageSetup::default(),
        PageSetup {
            paper_width_mm: Some(5.0),
            ..Default::default()
        },
        PageSetup {
            margin_gutter_mm: Some(-1.0),
            ..Default::default()
        },
    ];
    for setup in bad {
        assert!(
            set_page(&server, &doc_id, setup.clone()).is_err(),
            "{setup:?} 가 적용됨"
        );
    }
    assert!(server
        .set_page_setup(Parameters(SetPageSetupParams {
            doc_id: doc_id.clone(),
            section: 9,
            setup: PageSetup {
                margin_left_mm: Some(10.0),
                ..Default::default()
            },
        }))
        .is_err());
    assert_eq!(page_setup(&server, &doc_id), before);
    assert!(
        undo(&server, &doc_id).is_err(),
        "실패한 설정이 되돌리기 기록을 남김"
    );
}

fn list_forms(server: &HangulMcp, doc_id: &str) -> Vec<Value> {
    let v: Value = serde_json::from_str(
        &server
            .list_forms(Parameters(DocIdParams {
                doc_id: doc_id.to_string(),
            }))
            .unwrap(),
    )
    .unwrap();
    v["forms"].as_array().unwrap().clone()
}

fn cell_target_of(entry: &Value) -> CellTarget {
    let c = &entry["cell"];
    CellTarget {
        control: c["control"].as_u64().unwrap() as usize,
        cell: c["cell"].as_u64().unwrap() as usize,
        cell_para: Some(c["cell_para"].as_u64().unwrap() as usize),
    }
}

fn form_at_params(doc_id: &str, entry: &Value, cell: Option<CellTarget>) -> FormAtParams {
    FormAtParams {
        doc_id: doc_id.to_string(),
        section: entry["section"].as_u64().unwrap() as usize,
        para: entry["para"].as_u64().unwrap() as usize,
        control: entry["control"].as_u64().unwrap() as usize,
        cell,
    }
}

/// 재파싱한 문서에서 list_forms 항목 위치의 셀 양식 값.
fn cell_form_value(doc: &rhwp::model::document::Document, entry: &Value) -> i32 {
    let c = cell_target_of(entry);
    let para = &doc.sections[entry["section"].as_u64().unwrap() as usize].paragraphs
        [entry["para"].as_u64().unwrap() as usize];
    let rhwp::model::control::Control::Table(t) = &para.controls[c.control] else {
        panic!("표가 아님");
    };
    match &t.cells[c.cell].paragraphs[c.cell_para()].controls
        [entry["control"].as_u64().unwrap() as usize]
    {
        rhwp::model::control::Control::Form(f) => f.value,
        _ => panic!("양식이 아님"),
    }
}

/// 표 셀 안 체크박스: list_forms 가 cell 좌표로 알리고, get_form_value/get_form_info 가
/// 읽고, set_form_value 로 바꾼 값이 HWPX/HWP 저장 뒤에도 남는다.
#[test]
fn form_in_table_cell_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("form_in_cells.hwpx"));
    let forms = list_forms(&server, &doc_id);
    let in_cells: Vec<_> = forms.iter().filter(|f| !f["cell"].is_null()).collect();
    assert!(!in_cells.is_empty(), "셀 안 양식이 목록에 없음");
    let entry = in_cells[0].clone();
    assert_eq!(entry["formType"], "CheckBox", "{entry}");
    let cell = || Some(cell_target_of(&entry));

    let value: Value = serde_json::from_str(
        &server
            .get_form_value(Parameters(form_at_params(&doc_id, &entry, cell())))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["value"], entry["value"], "{value}");
    let info: Value = serde_json::from_str(
        &server
            .get_form_info(Parameters(form_at_params(&doc_id, &entry, cell())))
            .unwrap(),
    )
    .unwrap();
    assert!(info["width"].as_u64().unwrap() > 0, "{info}");

    let new_value = 1 - entry["value"].as_i64().unwrap() as i32;
    let at = form_at_params(&doc_id, &entry, cell());
    server
        .set_form_value(Parameters(SetFormValueParams {
            doc_id: doc_id.clone(),
            section: at.section,
            para: at.para,
            control: at.control,
            cell: cell(),
            value: Some(new_value),
            text: None,
            caption: None,
        }))
        .expect("set_form_value in cell");
    assert_eq!(
        list_forms(&server, &doc_id)
            .iter()
            .find(|f| f["cell"] == entry["cell"] && f["control"] == entry["control"])
            .unwrap()["value"],
        new_value
    );
    for (ext, doc) in save_both(&server, &doc_id, "form_in_cell") {
        assert_eq!(cell_form_value(&doc, &entry), new_value, "{ext}");
    }
}

/// 셀 좌표 없이 셀 양식 위치를 주거나, 양식이 아닌 컨트롤을 가리키면 오류이고 기록을 남기지 않는다.
#[test]
fn form_in_table_cell_rejects_wrong_target() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("form_in_cells.hwpx"));
    let entry = list_forms(&server, &doc_id)
        .into_iter()
        .find(|f| !f["cell"].is_null())
        .unwrap();
    let mut wrong_cell = cell_target_of(&entry);
    wrong_cell.cell_para = Some(999);
    for cell in [None, Some(wrong_cell)] {
        let at = form_at_params(&doc_id, &entry, None);
        let desc = format!("{:?}", cell.as_ref().map(|c| c.cell_para));
        let r = server.set_form_value(Parameters(SetFormValueParams {
            doc_id: doc_id.clone(),
            section: at.section,
            para: at.para,
            control: at.control,
            cell,
            value: Some(1),
            text: None,
            caption: None,
        }));
        assert!(r.is_err(), "{desc}: {r:?}");
    }
    assert!(server
        .get_form_value(Parameters(form_at_params(&doc_id, &entry, None)))
        .is_err());
    assert!(
        undo(&server, &doc_id).is_err(),
        "실패한 설정이 되돌리기 기록을 남김"
    );
}

fn get_cell_format(
    server: &HangulMcp,
    doc_id: &str,
    (para, control): (usize, usize),
    row: u16,
    col: u16,
) -> CellFormat {
    serde_json::from_str(
        &server
            .get_cell_format(Parameters(GetCellFormatParams {
                doc_id: doc_id.to_string(),
                section: 0,
                para,
                control,
                row,
                col,
            }))
            .unwrap(),
    )
    .unwrap()
}

fn apply_cell_format(
    server: &HangulMcp,
    doc_id: &str,
    (para, control): (usize, usize),
    (start_row, start_col, end_row, end_col): (u16, u16, Option<u16>, Option<u16>),
    format: CellFormat,
) -> Result<String, rmcp::ErrorData> {
    server.apply_cell_format(Parameters(ApplyCellFormatParams {
        doc_id: doc_id.to_string(),
        section: 0,
        para,
        control,
        start_row,
        start_col,
        end_row,
        end_col,
        format,
    }))
}

fn side(style: BorderStyle, width_mm: f64, color: &str) -> BorderSide {
    BorderSide {
        style: Some(style),
        width_mm: Some(width_mm),
        color: Some(color.to_string()),
    }
}

/// 범위 적용·면별 부분 덮어쓰기·안 여백이 조회와 HWPX/HWP 저장 뒤에 같은 값으로 남고,
/// 범위 밖 셀은 맞닿은 테두리 면만 따라 바뀐다.
#[test]
fn cell_format_roundtrip() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let table = create_table(&server, &doc_id, 2, 3);
    let untouched = get_cell_format(&server, &doc_id, table, 1, 0);

    let red = side(BorderStyle::Solid, 0.4, "#ff0000");
    let out: Value = serde_json::from_str(
        &apply_cell_format(
            &server,
            &doc_id,
            table,
            (0, 0, None, Some(2)),
            CellFormat {
                background: Some("#ffcc00".to_string()),
                border: Some(red.clone()),
                vertical_align: Some(VerticalAlign::Center),
                header: Some(true),
                ..Default::default()
            },
        )
        .expect("apply row 0"),
    )
    .unwrap();
    assert_eq!(out["cells"], 3, "{out}");
    // 한 면의 선 종류만 바꾸면 굵기·색은 그 면의 현재 값, 배경은 그대로여야 한다.
    apply_cell_format(
        &server,
        &doc_id,
        table,
        (0, 1, None, None),
        CellFormat {
            border_bottom: Some(BorderSide {
                style: Some(BorderStyle::Double),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .expect("apply bottom");
    apply_cell_format(
        &server,
        &doc_id,
        table,
        (1, 1, None, None),
        CellFormat {
            padding_left_mm: Some(5.0),
            ..Default::default()
        },
    )
    .expect("apply padding");

    let f01 = get_cell_format(&server, &doc_id, table, 0, 1);
    assert_eq!(f01.background.as_deref(), Some("#ffcc00"));
    assert_eq!(f01.border_top, Some(red.clone()));
    assert_eq!(
        f01.border_bottom,
        Some(side(BorderStyle::Double, 0.4, "#ff0000"))
    );
    assert_eq!(f01.vertical_align, Some(VerticalAlign::Center));
    assert_eq!(f01.header, Some(true));
    let f11 = get_cell_format(&server, &doc_id, table, 1, 1);
    assert_eq!(f11.padding_left_mm, Some(5.0));
    assert_eq!(
        (f11.padding_right_mm, f11.padding_top_mm),
        (untouched.padding_right_mm, untouched.padding_top_mm),
        "지정하지 않은 안 여백이 바뀜"
    );
    // 맞닿은 면은 한글처럼 한 줄이다: 0행 아래 테두리가 1행 위 테두리로도 보인다.
    assert_eq!(
        get_cell_format(&server, &doc_id, table, 1, 0),
        CellFormat {
            border_top: Some(red.clone()),
            ..untouched.clone()
        }
    );
    assert_eq!(
        f11.border_top,
        Some(side(BorderStyle::Double, 0.4, "#ff0000")),
        "(0,1) 아래 면을 바꾸면 (1,1) 위 면도 같아야 한다"
    );

    let expected: Vec<CellFormat> = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]
        .iter()
        .map(|&(r, c)| get_cell_format(&server, &doc_id, table, r, c))
        .collect();
    for (ext, doc) in save_both(&server, &doc_id, "cell_format") {
        let t = table_at(&doc, table.0, table.1);
        let got: Vec<CellFormat> = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]
            .iter()
            .map(|&(r, c)| {
                let cell = t.cells.iter().find(|x| x.row == r && x.col == c).unwrap();
                CellFormat::from_cell(&doc, t, cell)
            })
            .collect();
        assert_eq!(got, expected, "{ext}");
    }
}

#[test]
fn apply_cell_format_rejects_invalid_input() {
    let server = HangulMcp::new();
    let (doc_id, _) = open(&server, &fixture("ref_text.hwpx"));
    let table = create_table(&server, &doc_id, 2, 2);
    let ok_format = || CellFormat {
        background: Some("#00ff00".to_string()),
        ..Default::default()
    };
    let bad_formats = [
        CellFormat::default(),
        CellFormat {
            background: Some("blue".to_string()),
            ..Default::default()
        },
        CellFormat {
            border: Some(side(BorderStyle::Solid, 0.33, "#000000")),
            ..Default::default()
        },
        CellFormat {
            border_left: Some(side(BorderStyle::Solid, 0.4, "red")),
            ..Default::default()
        },
        CellFormat {
            padding_top_mm: Some(-1.0),
            ..Default::default()
        },
    ];
    for f in bad_formats {
        let desc = format!("{f:?}");
        assert!(
            apply_cell_format(&server, &doc_id, table, (0, 0, None, None), f).is_err(),
            "{desc}"
        );
    }
    for range in [
        (1, 0, Some(0), None),
        (0, 0, Some(2), None),
        (0, 0, None, Some(5)),
    ] {
        assert!(
            apply_cell_format(&server, &doc_id, table, range, ok_format()).is_err(),
            "{range:?}"
        );
    }
    assert!(apply_cell_format(
        &server,
        &doc_id,
        (table.0, table.1 + 9),
        (0, 0, None, None),
        ok_format()
    )
    .is_err());
    // create_table 한 단계만 남아 있어야 한다.
    undo(&server, &doc_id).expect("undo create_table");
    assert!(
        undo(&server, &doc_id).is_err(),
        "실패한 서식이 되돌리기 기록을 남김"
    );
}
