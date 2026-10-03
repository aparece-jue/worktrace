//! 领域层：实体、跃迁表与校验。
//!
//! 禁止 IO：不得引用 `rusqlite`、`std::fs`、`std::time` 或 `platform::`。
