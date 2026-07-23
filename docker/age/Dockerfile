# dms-ai 元数据库：Apache AGE（图）+ pgvector（向量）
FROM apache/age:latest

# pgvector：按镜像内 PG 主版本装 PGDG 包（apache/age 基于 Debian + PGDG）
RUN apt-get update \
    && PGMAJOR=$(pg_config --version | grep -oE '[0-9]+' | head -1) \
    && apt-get install -y --no-install-recommends postgresql-${PGMAJOR}-pgvector \
    && rm -rf /var/lib/apt/lists/*
