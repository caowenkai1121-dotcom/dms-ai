# dms-ai

DMS 智能取数助手（彻底重构版）。方案与项目计划见仓库外 `../REBUILD-PLAN.md`。

- 后端：Rust + axum（`crates/server`），直连 DMS 生产 MySQL（**只读**，会话级 READ ONLY）+ 自有 PG 元数据库
- 前端：Vue3 + Vite + TS + Ant Design Vue + ECharts（`web/`）
- 元数据库：PG + Apache AGE（图）+ pgvector（向量）+ pg_trgm（`docker/age`，端口 15433）
- 架构参考 SuperSonic：语义层 / 词典+向量双召回 / LLM 只产 S2SQL / 确定性校正与翻译 / parse-execute 两段协议

## 起步

```powershell
# 1. 元数据库
cd docker/age; docker compose up -d --build
# 2. 后端（需 settings.json，参考 settings.example.json）
./scripts/run.ps1
# 3. 前端
cd web; npm install; npm run dev
```

红线：DMS 数据库只读，任何写操作禁止；xh-dms / xh-dms-fornt / xh-xcx 三份源码只读。
