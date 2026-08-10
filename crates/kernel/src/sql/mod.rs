//! SQL 层：词法（纯文本扫描）/ AST 遍历 / 只读红线判定 / 口径校验 / 方言。
//! 切分依据是「是否需要 sqlparser」：`lex` 纯字符扫描，`ast` 走 sqlparser。
//! `gate`（三段 newtype 闸门）属 T3，届时在此加 `pub mod gate;`。

pub mod ast;
pub mod caliber;
pub mod dialect;
pub mod dms_lookup;
pub mod gate;
pub mod guard;
pub mod lex;
