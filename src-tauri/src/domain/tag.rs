//! 标签的类型与命名规则（02 §2、04 F-005）。
//!
//! V0.1 的四类标签是**平铺**的：`Knowledge`（层级标签）与任何非空 `parent_id`
//! 都属 V0.2。schema 用 `ck_tag_parent_kind CHECK (parent_id IS NULL)` 兜底，
//! 这里负责在写入前给出**用户读得懂**的拒绝理由，而不是让用户看到一条存储错误。

use super::error::{DomainError, DomainResult};

/// 标签类型。取值与 `tag.kind` 的 CHECK 逐字一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Domain,
    Activity,
    Context,
    Report,
}

impl TagKind {
    pub const ALL: [TagKind; 4] = [Self::Domain, Self::Activity, Self::Context, Self::Report];

    /// 落库用的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Domain => "Domain",
            Self::Activity => "Activity",
            Self::Context => "Context",
            Self::Report => "Report",
        }
    }

    /// 从库里的值解析。`None` 的含义与 `ProjectStatus::parse` 相同：
    /// 库被写坏，或版本超前，两种情况都必须报错而不是回落。
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// 标签名的规范化：只去首尾空白。
///
/// 唯一性口径是「同 kind + 规范化后的名字 + **大小写敏感**」：
/// `写作` 与 `写作 ` 是同一个标签，`Abc` 与 `abc` 是两个。
/// 存储层的 `uq_tag_root(kind, name)`（默认 BINARY 排序规则）执行的就是这一条，
/// 所以服务层的预检必须调用**这个函数**，不能自己写一份 trim + 比较——
/// 预检与兜底给出不同答案时，用户会看到一条自相矛盾的报错。
pub fn normalize_name(raw: &str) -> DomainResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DomainError::EmptyText { field: "标签名" });
    }
    Ok(trimmed.to_string())
}

/// V0.1 没有标签层级：任何非空 `parent_id` 都拒绝。
///
/// 空白串按「未提供」处理——空 ID 指向不了任何标签，而前端把「不选」
/// 序列化成空串是常见形状，为此报错等于让用户看一个他没做错的事。
pub fn ensure_no_parent(parent_id: Option<&str>) -> DomainResult<()> {
    match parent_id {
        Some(id) if !id.trim().is_empty() => Err(DomainError::NotInThisVersion {
            what: "标签层级",
        }),
        _ => Ok(()),
    }
}
