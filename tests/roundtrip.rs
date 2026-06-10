//! 라운드트립 통합 테스트: open → edit → save → 재파싱 단언.
//!
//! MCP 도구 메서드를 직접 호출한다 (stdio 전송 계층 제외 전 구간 검증).

use std::path::PathBuf;

use hangul_mcp::server::{
    DocIdParams, FieldByNameParams, FitTableToPageParams, FormAtParams, GetTableParams, HangulMcp,
    InsertTableColumnParams, InsertTableRowParams, InsertTextParams, OpenDocumentParams,
    ParaParams, ReplaceTextParams, SaveDocumentParams, SetCellTextParams, SetFieldValueParams,
    SetFormValueParams,
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
        }))
        .expect("set_form_value (checkbox)");

    // 3) 즉시 조회 확인
    let got = server
        .get_form_value(Parameters(FormAtParams {
            doc_id: doc_id.clone(),
            section: e_sec,
            para: e_para,
            control: e_ctrl,
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
