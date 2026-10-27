//! 쪽 설정(용지 크기·방향·여백·제본) — MCP 입출력 타입과 rhwp `PageDef` 사이의 변환.
//!
//! 조회(`get_page_setup`)와 적용(`set_page_setup`)이 한 타입·한 단위(mm)를 쓴다.
//! 용지 너비/높이는 한글 "용지 종류"의 크기(A4 = 210×297)이고, 가로 방향은 둘을 바꿔 그린다.
//! rhwp 는 여백 합이 용지를 넘으면 오류 없이 5% 여백으로 폴백하므로 본문 영역이 남는지
//! 여기서 먼저 확인한다.

use rhwp::model::page::{BindingMethod, PageDef};
use rmcp::ErrorData;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::format::check_range;

/// 용지·여백 한 변의 상한 (mm). 그림 크기 상한과 같은 값.
pub(crate) const MAX_PAPER_MM: f64 = 2000.0;
/// 용지 한 변의 하한 (mm).
const MIN_PAPER_MM: f64 = 10.0;

pub(crate) fn mm_to_hu(mm: f64) -> f64 {
    mm * 7200.0 / 25.4
}

fn hu_to_mm(hu: u32) -> f64 {
    (hu as f64 * 25.4 / 7200.0 * 100.0).round() / 100.0
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// 세로 (좁게)
    Portrait,
    /// 가로 (넓게): 용지 너비/높이를 바꿔 그린다
    Landscape,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    /// 한쪽 편집
    Single,
    /// 맞쪽 편집: 짝수 쪽은 좌우 여백을 바꾼다
    Duplex,
    /// 위로 넘기기
    TopFlip,
}

/// 쪽 설정. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct PageSetup {
    /// 용지 너비 (mm, 세로 방향 기준. A4 = 210)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_width_mm: Option<f64>,
    /// 용지 높이 (mm, 세로 방향 기준. A4 = 297)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_height_mm: Option<f64>,
    /// 용지 방향
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<Orientation>,
    /// 왼쪽 여백 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_left_mm: Option<f64>,
    /// 오른쪽 여백 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_right_mm: Option<f64>,
    /// 위쪽 여백 (mm, 머리말 영역 아래부터)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_top_mm: Option<f64>,
    /// 아래쪽 여백 (mm, 꼬리말 영역 위까지)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_bottom_mm: Option<f64>,
    /// 머리말 여백 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_header_mm: Option<f64>,
    /// 꼬리말 여백 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_footer_mm: Option<f64>,
    /// 제본 여백 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_gutter_mm: Option<f64>,
    /// 제본 방법
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
}

impl PageSetup {
    pub fn from_page_def(pd: &PageDef) -> Self {
        Self {
            paper_width_mm: Some(hu_to_mm(pd.width)),
            paper_height_mm: Some(hu_to_mm(pd.height)),
            orientation: Some(if pd.landscape {
                Orientation::Landscape
            } else {
                Orientation::Portrait
            }),
            margin_left_mm: Some(hu_to_mm(pd.margin_left)),
            margin_right_mm: Some(hu_to_mm(pd.margin_right)),
            margin_top_mm: Some(hu_to_mm(pd.margin_top)),
            margin_bottom_mm: Some(hu_to_mm(pd.margin_bottom)),
            margin_header_mm: Some(hu_to_mm(pd.margin_header)),
            margin_footer_mm: Some(hu_to_mm(pd.margin_footer)),
            margin_gutter_mm: Some(hu_to_mm(pd.margin_gutter)),
            binding: Some(match pd.binding {
                BindingMethod::SingleSided => Binding::Single,
                BindingMethod::DuplexSided => Binding::Duplex,
                BindingMethod::TopFlip => Binding::TopFlip,
            }),
        }
    }

    /// 현재 `pd` 에 지정 항목을 덮어쓴 rhwp `set_page_def_native` 속성 JSON.
    /// 값 범위와, 덮어쓴 결과에 본문 영역이 남는지를 문서를 건드리기 전에 확인한다.
    pub fn to_rhwp_json(&self, pd: &PageDef) -> Result<String, ErrorData> {
        let mut obj = serde_json::Map::new();
        let mut next = pd.clone();
        let paper = [
            (
                "paper_width_mm",
                "width",
                self.paper_width_mm,
                &mut next.width,
            ),
            (
                "paper_height_mm",
                "height",
                self.paper_height_mm,
                &mut next.height,
            ),
        ];
        for (name, key, v, slot) in paper {
            if let Some(v) = v {
                check_range(name, v, MIN_PAPER_MM, MAX_PAPER_MM)?;
                *slot = mm_to_hu(v).round() as u32;
                obj.insert(key.into(), json!(*slot));
            }
        }
        let margins = [
            (
                "margin_left_mm",
                "marginLeft",
                self.margin_left_mm,
                &mut next.margin_left,
            ),
            (
                "margin_right_mm",
                "marginRight",
                self.margin_right_mm,
                &mut next.margin_right,
            ),
            (
                "margin_top_mm",
                "marginTop",
                self.margin_top_mm,
                &mut next.margin_top,
            ),
            (
                "margin_bottom_mm",
                "marginBottom",
                self.margin_bottom_mm,
                &mut next.margin_bottom,
            ),
            (
                "margin_header_mm",
                "marginHeader",
                self.margin_header_mm,
                &mut next.margin_header,
            ),
            (
                "margin_footer_mm",
                "marginFooter",
                self.margin_footer_mm,
                &mut next.margin_footer,
            ),
            (
                "margin_gutter_mm",
                "marginGutter",
                self.margin_gutter_mm,
                &mut next.margin_gutter,
            ),
        ];
        for (name, key, v, slot) in margins {
            if let Some(v) = v {
                check_range(name, v, 0.0, MAX_PAPER_MM)?;
                *slot = mm_to_hu(v).round() as u32;
                obj.insert(key.into(), json!(*slot));
            }
        }
        if let Some(o) = self.orientation {
            next.landscape = o == Orientation::Landscape;
            obj.insert("landscape".into(), json!(next.landscape));
        }
        if let Some(b) = self.binding {
            let code = match b {
                Binding::Single => 0,
                Binding::Duplex => 1,
                Binding::TopFlip => 2,
            };
            obj.insert("binding".into(), json!(code));
        }
        if obj.is_empty() {
            return Err(ErrorData::invalid_params(
                "setup 에 바꿀 항목이 하나도 없다",
                None,
            ));
        }
        check_body_area(&next)?;
        Ok(serde_json::Value::Object(obj).to_string())
    }
}

/// 여백을 뺀 본문 너비·높이가 양수여야 한다 (rhwp `PageAreas` 의 본문 계산과 같은 합).
fn check_body_area(pd: &PageDef) -> Result<(), ErrorData> {
    let (w, h) = if pd.landscape {
        (pd.height, pd.width)
    } else {
        (pd.width, pd.height)
    };
    let horizontal = pd.margin_left as u64 + pd.margin_right as u64 + pd.margin_gutter as u64;
    let vertical = pd.margin_top as u64
        + pd.margin_bottom as u64
        + pd.margin_header as u64
        + pd.margin_footer as u64;
    if horizontal >= w as u64 || vertical >= h as u64 {
        return Err(ErrorData::invalid_params(
            format!(
                "여백이 용지를 넘어 본문 영역이 없다: 좌우+제본 {}mm / 쪽 너비 {}mm, 위아래+머리말·꼬리말 {}mm / 쪽 높이 {}mm",
                hu_to_mm(horizontal as u32),
                hu_to_mm(w),
                hu_to_mm(vertical as u32),
                hu_to_mm(h)
            ),
            None,
        ));
    }
    Ok(())
}
