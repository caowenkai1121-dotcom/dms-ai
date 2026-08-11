-- dms-ai 元数据库初始化：图 + 向量 + 检索扩展
-- ⚠️ 本脚本只在数据目录为空的首次启动执行一次（官方 entrypoint 语义）：改此文件对既有 volume
-- 永不生效，要生效得清空 volume 重建。
-- 扩展只建在默认库（POSTGRES_DB=dms_ai）；另建的库要手工补，见 docs/DEPLOY.md「起依赖」段（DEPLOY.md:36）。
CREATE EXTENSION IF NOT EXISTS age;
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;
