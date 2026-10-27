//! 쪽 설정(용지 크기·방향·여백·제본)과 다단 — MCP 입출력 타입과 rhwp `PageDef`/`ColumnDef`
//! 사이의 변환.
//!
//! 조회(`get_page_setup`)와 적용(`set_page_setup`)이 한 타입·한 단위(mm)를 쓴다.
//! 용지 너비/높이는 한글 "용지 종류"의 크기(A4 = 210×297)이고, 가로 방향은 둘을 바꿔 그린다.
//! rhwp 는 여백 합이 용지를 넘으면 오류 없이 5% 여백으로 폴백하므로 본문 영역이 남는지
//! 여기서 먼저 확인한다.

use rhwp::model::page::{BindingMethod, ColumnDef, ColumnType, PageAreas, PageDef};
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

/// 단 간격 상한 (mm). HWPUNIT16(i16, 약 115mm) 안.
const MAX_COLUMN_SPACING_MM: f64 = 100.0;
/// 단 수 상한 (한글 단 설정과 같다).
const MAX_COLUMNS: u16 = 255;

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    /// 일반 다단: 앞 단을 채운 뒤 다음 단으로
    Normal,
    /// 배분 다단: 단마다 높이를 고르게
    Distribute,
    /// 평행 다단: 단을 따로 채운다
    Parallel,
}

/// 구역 첫 단 설정. 적용 시 지정한 항목만 바뀐다.
#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Default, PartialEq, Debug)]
pub struct Columns {
    /// 단 수 (1 = 다단 아님, 최대 255)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u16>,
    /// 단 종류
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ColumnKind>,
    /// 단 사이 간격 (mm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spacing_mm: Option<f64>,
}

impl Columns {
    pub fn from_column_def(cd: &ColumnDef) -> Self {
        Self {
            count: Some(cd.column_count),
            kind: Some(match cd.column_type {
                ColumnType::Normal => ColumnKind::Normal,
                ColumnType::Distribute => ColumnKind::Distribute,
                ColumnType::Parallel => ColumnKind::Parallel,
            }),
            spacing_mm: Some(hu_to_mm(cd.spacing.max(0) as u32)),
        }
    }

    /// 현재 `cd` 에 지정 항목을 덮어쓴 rhwp `set_column_def_native` 인자
    /// (단 수, 종류 코드, 너비 같게, 간격 HWPUNIT). 단마다 본문 폭이 남는지 먼저 확인한다.
    /// rhwp 는 단 수가 바뀌면 단별 너비를 지우므로 그때는 너비를 같게 한다.
    pub fn to_rhwp_args(
        &self,
        cd: &ColumnDef,
        pd: &PageDef,
    ) -> Result<(u16, u8, bool, i16), ErrorData> {
        if *self == Self::default() {
            return Err(ErrorData::invalid_params(
                "columns 에 바꿀 항목이 하나도 없다",
                None,
            ));
        }
        let count = self.count.unwrap_or(cd.column_count);
        check_range("count", f64::from(count), 1.0, f64::from(MAX_COLUMNS))?;
        let spacing = match self.spacing_mm {
            Some(v) => {
                check_range("spacing_mm", v, 0.0, MAX_COLUMN_SPACING_MM)?;
                mm_to_hu(v).round() as i16
            }
            None => cd.spacing,
        };
        let body = PageAreas::from_page_def(pd).body_area.width() as i64;
        let gaps = i64::from(spacing.max(0)) * (i64::from(count) - 1);
        if gaps >= body {
            return Err(ErrorData::invalid_params(
                format!(
                    "단 간격 합 {}mm 가 본문 폭 {}mm 를 넘어 단 너비가 남지 않는다",
                    hu_to_mm(gaps as u32),
                    hu_to_mm(body as u32)
                ),
                None,
            ));
        }
        let kind = match self
            .kind
            .unwrap_or(Self::from_column_def(cd).kind.expect("kind"))
        {
            ColumnKind::Normal => 0,
            ColumnKind::Distribute => 1,
            ColumnKind::Parallel => 2,
        };
        let same_width = cd.same_width || count != cd.column_count;
        Ok((count, kind, same_width, spacing))
    }
}
