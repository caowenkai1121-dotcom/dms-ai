# dms-ai 元数据库：Apache AGE（图）+ pgvector（向量）
FROM apache/age:latest
# 为什么敢用 :latest：开发期接受漂移，换省心（重建即跟最新构建）。
# 已知风险：PG 主版本随 latest 漂，跨主版本数据目录不兼容 —— 对着既有 named volume rebuild
# 可能直接起不来；生产化之前应先钉主版本 tag/digest，并与 docs/DEPLOY.md:9 的「postgres16」口径对齐。

# pgvector：按镜像内 PG 主版本装 PGDG 包（apache/age 基于 Debian + PGDG）
# PGMAJOR 取版本串首个数字：PG 官方版本串（"PostgreSQL 16.x ..."）没问题；基底若换成版本串
# 带前缀数字的 fork 会静默得到错包名 —— 换基底时复查这行。
# pgvector 版本不钉（随 PGDG 浮动）：hnsw 参数/默认值跨版本可能变；要钉就
# postgresql-${PGMAJOR}-pgvector=<ver>（可用版本需实机 apt-cache 查）。
RUN apt-get update \
    && PGMAJOR=$(pg_config --version | grep -oE '[0-9]+' | head -1) \
    && apt-get install -y --no-install-recommends postgresql-${PGMAJOR}-pgvector \
    && rm -rf /var/lib/apt/lists/* \
    && ls "$(pg_config --pkglibdir)/vector.so"
# 收尾的 ls 是构建期断言：包名错了 apt 也会「成功」（装错包），别等 initdb 的 CREATE EXTENSION 才炸。
# HEALTHCHECK 不在这里写，在 docker-compose.yml 里（pg_isready -U postgres -d dms_ai）。
