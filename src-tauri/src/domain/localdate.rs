//! 本地日期（02 §2 的 `daily_plan.local_date`）。
//!
//! 「本地」指**某个用户时区里的一天**。这个类型只管一件事：一个日期是不是
//! 真实存在的公历日。时区换算不在这里——它在 `services::daily_plan`，
//! 因为那里才拿得到时区库。
//!
//! 为什么不靠 schema 的 GLOB：`daily_plan.local_date` 上的
//! `GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'` 只管**形状**，
//! `2023-02-30` 完全写得进去（`tests/catalog_validation.rs` 有用例钉住这一点）。
//! 闰年与月长只有日历算得出来。

use std::fmt;

use super::error::{DomainError, DomainResult};

/// 进用户可见文案的字段名（见 `DomainError` 的 Display 文档）。
const FIELD: &str = "本地日期";

/// 一个真实存在的公历日期。
///
/// 内部用 `jiff::civil::Date` 判定日历：它是纯计算库，不做 IO，所以不违反
/// 「domain 只放纯规则、不碰存储与平台」这条分层约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDate {
    date: jiff::civil::Date,
}

impl LocalDate {
    /// 严格解析 `YYYY-MM-DD`。
    ///
    /// - 形状固定 10 个字符：4 位年、`-`、2 位月、`-`、2 位日，且都是 ASCII 数字；
    /// - 必须是真实日历日（含闰年与月长）；
    /// - **不**去首尾空白，也不接受 `2024-2-29` 这类「差一点」的写法。日期是
    ///   机器生成的形状，宽松解析只会把「前端拼错了」变成「库里多了一个
    ///   看不出来的日期」。
    pub fn parse(raw: &str) -> DomainResult<Self> {
        if raw.trim().is_empty() {
            return Err(DomainError::EmptyText { field: FIELD });
        }
        if !has_iso_shape(raw) {
            return Err(malformed(raw));
        }
        // 形状检查已保证这 10 个字节全是 ASCII 数字或 `-`，
        // 所以下面的按字节切片不会落在字符中间。
        let year = raw[0..4].parse::<i16>().map_err(|_| malformed(raw))?;
        let month = raw[5..7].parse::<i8>().map_err(|_| malformed(raw))?;
        let day = raw[8..10].parse::<i8>().map_err(|_| malformed(raw))?;
        Self::new(year, month, day)
    }

    /// 由年月日构造，与 `parse` 共用同一套规则（同一条规则不留第二份实现）。
    ///
    /// 年份只接受 `0..=9999`：落库形状是 4 位年，负年与 5 位年拼不回 `YYYY-MM-DD`，
    /// 放进来会让「`LocalDate` 与列里的字符串一一对应」这条不变量破掉。
    pub fn new(year: i16, month: i8, day: i8) -> DomainResult<Self> {
        let shown = format!("{year:04}-{month:02}-{day:02}");
        if !(0..=9999).contains(&year) {
            return Err(malformed(&shown));
        }
        jiff::civil::Date::new(year, month, day)
            .map(|date| Self { date })
            .map_err(|_| malformed(&shown))
    }

    pub fn year(self) -> i16 {
        self.date.year()
    }

    pub fn month(self) -> i8 {
        self.date.month()
    }

    pub fn day(self) -> i8 {
        self.date.day()
    }

    /// 由一个已经算好的日历日构造。
    ///
    /// 只给 `services::daily_plan` 用：那条路径上的日期是**换算**出来的
    /// （时刻 + 时区 → 本地日期），不是解析出来的。它仍然走 `new`，
    /// 所以范围与日历规则与 `parse` 完全一致。
    pub(crate) fn from_jiff(date: jiff::civil::Date) -> DomainResult<Self> {
        Self::new(date.year(), date.month(), date.day())
    }
}

impl fmt::Display for LocalDate {
    /// 落库形状 `YYYY-MM-DD`，与 [`LocalDate::parse`] 的输入逐字一致（可往返）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.date)
    }
}

/// 固定宽度形状检查：10 个字节，第 5、8 个是 `-`，其余位置是 ASCII 数字。
///
/// 先查字节再切片：多字节字符也能凑出 10 个字节（例如 `2024-02-2九`），
/// 直接按字节下标切片会切在字符中间而 panic。
fn has_iso_shape(raw: &str) -> bool {
    let b = raw.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && [0usize, 1, 2, 3, 5, 6, 8, 9]
            .iter()
            .all(|i| b[*i].is_ascii_digit())
}

/// 「这不是一个合法的本地日期」。
///
/// 复用 `UnknownEnumValue` 而不是新增变体：`domain/error.rs` 是 P1 已发布的形状，
/// 而这条规则的语义正是「这个值不在取值域里」。`field` 写成面向用户的中文。
fn malformed(value: &str) -> DomainError {
    DomainError::UnknownEnumValue {
        field: FIELD,
        value: value.to_string(),
    }
}
