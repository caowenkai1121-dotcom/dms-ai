//! 方言层。**五个方法**（ARCHITECTURE §8）：`name/quote/parser/table_probe/column_probe`。
//! `time_fn/classify_column/limit_clause` 因零消费者或零方言差异删掉
//! （MySQL 与 PG 的 `LIMIT n` 写法相同）；`time_fn` 等到 K3 做 PG 规则时间解析时再加。
//!
//! `quote` 是**被实测拽回来的**那一个：它曾以「零消费者」之名被删（当时 prompt 里
//! 「结果列别名用反引号包裹」是硬编码的 MySQL 写法）。K4 首次对上传的 PG 源问数，
//! LLM 照着那句写出 `AS \`人数\``，PG 当场 `syntax error at or near "\`"` ——
//! 也就是说那条硬编码让**任何非 MySQL 源的问数恒失败**，而它一直没有消费者是因为
//! 那条通道从来没被实测过。

/// 一个只读源方言需要告诉内核的全部：怎么 parse、怎么采 schema、标识符怎么引。
pub trait Dialect: Send + Sync + 'static {
    /// 方言名（prompt 里的方言段用它插值，也是 `by_name` 的 key）
    fn name(&self) -> &'static str;
    /// 标识符引号（prompt 告诉 LLM 中文别名该拿什么包）。MySQL 反引号，PG 双引号。
    fn quote(&self) -> &'static str;
    /// sqlparser 侧的方言实例（`Parser::parse_sql` 的入参）
    fn parser(&self) -> &'static (dyn sqlparser::dialect::Dialect + Send + Sync);
    /// 表探针：`(表名, 表注释, 估算行数)` 三列，按当前库/schema 过滤，只取基表
    fn table_probe(&self) -> &'static str;
    /// 列探针：`(表名, 列名, 类型, 列注释, 序号)` 五列，按当前库/schema 过滤
    fn column_probe(&self) -> &'static str;
}

pub struct MysqlDialect;
pub struct PostgresDialect;

static MYSQL_PARSER: sqlparser::dialect::MySqlDialect = sqlparser::dialect::MySqlDialect {};
static PG_PARSER: sqlparser::dialect::PostgreSqlDialect = sqlparser::dialect::PostgreSqlDialect {};
static MYSQL: MysqlDialect = MysqlDialect;
static POSTGRES: PostgresDialect = PostgresDialect;

impl Dialect for MysqlDialect {
    fn name(&self) -> &'static str {
        "MySQL"
    }
    fn quote(&self) -> &'static str {
        "`"
    }
    fn parser(&self) -> &'static (dyn sqlparser::dialect::Dialect + Send + Sync) {
        &MYSQL_PARSER
    }
    /// 逐字取自 `meta.rs:215-218`（连库验证过的形态）
    fn table_probe(&self) -> &'static str {
        "SELECT CAST(TABLE_NAME AS CHAR), CAST(TABLE_COMMENT AS CHAR), CAST(TABLE_ROWS AS CHAR)
         FROM information_schema.TABLES
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'"
    }
    /// 逐字取自 `meta.rs:222-226`。**`CAST(... AS CHAR)` 一个都不能省**：
    /// information_schema 的注释列是 LONGBLOB，不 CAST 会被 sqlx 解成 `Vec<u8>` 直接类型不匹配报错。
    fn column_probe(&self) -> &'static str {
        "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), CAST(DATA_TYPE AS CHAR),
                CAST(COLUMN_COMMENT AS CHAR), CAST(ORDINAL_POSITION AS CHAR)
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE()"
    }
}

impl Dialect for PostgresDialect {
    fn name(&self) -> &'static str {
        "PostgreSQL"
    }
    fn quote(&self) -> &'static str {
        "\""
    }
    fn parser(&self) -> &'static (dyn sqlparser::dialect::Dialect + Send + Sync) {
        &PG_PARSER
    }
    /// PG 侧两条探针**已连库实测**（2026-07-28，上传源首次问数；见 `connector/src/postgres.rs` 头注）。
    /// `reltuples` 是 ANALYZE 的估算值（未 ANALYZE 的新表为 -1），与 MySQL 的 `TABLE_ROWS` 同为估算。
    ///
    /// `n.nspname = current_schema()` 是**上传源必须带 `search_path` 的原因**：多份上传共用一条
    /// `pg_ro_url`、schema 一份一个，不置 search_path 则这里恒查 `public`、一张表都采不到。
    fn table_probe(&self) -> &'static str {
        "SELECT c.relname, coalesce(obj_description(c.oid, 'pg_class'), ''), c.reltuples::bigint
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind = 'r' AND n.nspname = current_schema()"
    }
    /// 同上已实测。走 pg_catalog 而非 information_schema：后者拿列注释要把
    /// `table_schema.table_name` 拼成 regclass 再取 oid，pg_attribute 直接有 attrelid/attnum。
    fn column_probe(&self) -> &'static str {
        "SELECT c.relname, a.attname, format_type(a.atttypid, a.atttypmod),
                coalesce(col_description(c.oid, a.attnum), ''), a.attnum
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_attribute a ON a.attrelid = c.oid
         WHERE c.relkind = 'r' AND n.nspname = current_schema()
           AND a.attnum > 0 AND NOT a.attisdropped"
    }
}

/// 配置里的方言名 → 方言实例。认不出返回 `None`（调用方 fail-closed，不猜默认值）。
pub fn by_name(name: &str) -> Option<&'static dyn Dialect> {
    match name.to_ascii_lowercase().as_str() {
        "mysql" => Some(&MYSQL),
        "postgres" | "postgresql" | "pg" => Some(&POSTGRES),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_is_case_insensitive_and_closed() {
        assert_eq!(by_name("MySQL").map(|d| d.name()), Some("MySQL"));
        assert_eq!(by_name("pg").map(|d| d.name()), Some("PostgreSQL"));
        assert_eq!(by_name("postgresql").map(|d| d.name()), Some("PostgreSQL"));
        assert!(by_name("oracle").is_none());
    }

    /// 🔴 两个方言的引号**必须不同**：相同就说明有人把 PG 那支复制成了反引号，
    /// 而那正是「非 MySQL 源问数恒 syntax error」那个缺陷的形态。
    #[test]
    fn quote_differs_per_dialect() {
        assert_eq!(MysqlDialect.quote(), "`");
        assert_eq!(PostgresDialect.quote(), "\"");
        assert_ne!(MysqlDialect.quote(), PostgresDialect.quote());
    }
}
