//! 라운드트립 통합 테스트: open → edit → save → 재파싱 단언.
//!
//! MCP 도구 메서드를 직접 호출한다 (stdio 전송 계층 제외 전 구간 검증).

use std::path::PathBuf;

use hangul_mcp::format::{
    Alignment, CellTarget, CharFormat, LineSpacing, LineSpacingKind, ParaFormat,
};
use hangul_mcp::server::{
    ApplyCharFormatParams, ApplyParaFormatParams, ApplyStyleParams, DocIdParams, FieldByNameParams,
    FitTableToPageParams, FormAtParams, GetCharFormatParams, GetTableParams, HangulMcp,
    InsertTableColumnParams, InsertTableRowParams, InsertTextParams, OpenDocumentParams,
    ParaParams, ParaTargetParams, ReplaceTextParams, SaveDocumentParams, SetCellTextParams,
    SetFieldValueParams, SetFormValueParams, SplitParagraphParams, StyleRef,
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
