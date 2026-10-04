//! 项目状态（02 §2、04 F-004）。
//!
//! 「读得懂」与「写得进」是两条不同的规则：
//!
//! - `parse` 覆盖 schema 的**整个取值域**（`active`/`archived`/`done`）。读路径
//!   必须用它——库里那行 `done` 是合法数据，读成错误才是 bug；
//! - `is_writable_in_v01` 是本版的**写范围**：UI 只提供创建（`active`）与
//!   归档（`archived`），`done` 留给后续版本。schema 与 02 都保留它，
//!   届时不必改已发布的 CHECK。
//!
//! 两件事分开写，是因为把它们压成一条规则总要牺牲一头：要么读不了 `done`，
//! 要么悄悄接受了一个规格里没有的状态写入。

use super::error::{DomainError, DomainResult};

/// 项目状态。取值与 `project.status` 的 CHECK 逐字一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectStatus {
    Active,
    Archived,
    Done,
}

impl ProjectStatus {
    pub const ALL: [ProjectStatus; 3] = [Self::Active, Self::Archived, Self::Done];

    /// 落库用的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Done => "done",
        }
    }

    /// 从库里的值解析。
    ///
    /// `None` 表示不在取值域里——正常路径上 CHECK 挡得住，出现它只可能是库被绕过
    /// CHECK 写坏过，或更新版本写入的取值被旧版本读到。两种都必须**报错并说清
    /// 是哪一列**（见 `storage::task_repo::enum_error` 的先例），不得回落默认值。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// V0.1 接受写入的状态：创建写 `active`，归档写 `archived`。
    pub fn is_writable_in_v01(self) -> bool {
        matches!(self, Self::Active | Self::Archived)
    }
}

/// IPC/JSON 形状：**就是落库用的那套小写字符串**（[`ProjectStatus::as_str`]）。
///
/// 手写而不是派生：派生序列化变体名（`Active`/`Archived`/`Done`），而库里、schema 的
/// CHECK 与前端约定的是 `as_str()` 那一份。理由与 [`crate::domain::task::TaskStatus`]
/// 的实现逐字相同。
impl serde::Serialize for ProjectStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// 项目名的规范化：只去首尾空白，全空白视为空输入。
///
/// 与 `domain::tag::normalize_name` 同一口径：写入与比较都用**这个函数**的产出，
/// 预检与存储层（`project.name` 的 `length(trim(name)) > 0` 约束）才给出一致答案。
/// 项目名**不要求唯一**——schema 没有唯一索引，02 §2 与 F-004 都没要求，
/// 重名是两条独立的项目。
pub fn normalize_name(raw: &str) -> DomainResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::EmptyText { field: "项目名" });
    }
    Ok(trimmed.to_string())
}
