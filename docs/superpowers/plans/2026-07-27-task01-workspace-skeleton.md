# Task 1：6-crate 工作区骨架（零风险）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不动任何业务代码的前提下，把单 crate workspace 扩成 6 个 crate，让 cargo 认下依赖方向，为后续纯算法下沉做准备。

**Architecture:** 新增 kernel/connector/policy/semantic/agent 五个空库 crate（各仅 `lib.rs`），server 作为二进制 crate 增加对这五个的 path 依赖但**一个符号都不 use**。依赖方向照 spec：`kernel ← connector ← {policy, semantic} ← agent ← server`，`semantic 不依赖 policy`。

**Tech Stack:** Rust workspace、cargo。

## Global Constraints

- **零行为变化**：不删/不改任何现有 `crates/server/src/**` 业务逻辑；`cargo build` 产物与今天完全等价。
- **依赖红线**：五个新 crate 的 `Cargo.toml` 只允许出现 spec 已列依赖（kernel: serde/serde_json/sqlparser/chrono；connector: + sqlx/reqwest/futures/tokio/tracing；其余: 暂空），**不得新增任何 spec 之外第三方 crate**。
- **禁止 `pub use` 业务符号**：本步只建空壳，不做任何 re-export 或代码搬移（那是 Task 2+）。
- **版本/锚定**：`sqlparser = "0.53"`（features=["visitor"]）、`sqlx = "0.8"`、`tokio = "1"`、`axum = "0.8"`、`chrono = "0.4"`——与 server 现 Cargo.toml 完全一致。
- **统一 crate 命名**：`dms-kernel` / `dms-connector` / `dms-policy` / `dms-semantic` / `dms-agent` / `dms-ai-server`(不变)。
- Windows 构建须前缀 MinGW bin 路径（见 Task 0 备注），cargo 命令走 PowerShell 不走 Bash。

---

### Task 1.1: 声明 workspace members

**Files:**
- Modify: `Cargo.toml`（workspace 根）

**Interfaces:**
- Consumes: 无（现状 `members = ["crates/server"]`）
- Produces: `members` 含 6 个 crate 路径，供后续任务解析

- [ ] **Step 1: 备份并编辑根 Cargo.toml**

把：
```toml
members = ["crates/server"]
```
改为：
```toml
members = [
    "crates/kernel",
    "crates/connector",
    "crates/policy",
    "crates/semantic",
    "crates/agent",
    "crates/server",
]
```

- [ ] **Step 2: 创建五个空 crate 目录与最小文件**

逐个创建（内容见下），否则 cargo 因找不到 member 报错：
```
crates/kernel/Cargo.toml
crates/kernel/src/lib.rs
crates/connector/Cargo.toml
crates/connector/src/lib.rs
crates/policy/Cargo.toml
crates/policy/src/lib.rs
crates/semantic/Cargo.toml
crates/semantic/src/lib.rs
crates/agent/Cargo.toml
crates/agent/src/lib.rs
```

`crates/kernel/Cargo.toml`:
```toml
[package]
name = "dms-kernel"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = { workspace = true }   # 迁移期过渡：搬移的纯函数现返回 anyhow::Result，Task 3+ 再细分为强类型错误
serde = { workspace = true }
serde_json = { workspace = true }
sqlparser = { version = "0.53", features = ["visitor"] }
chrono = "0.4"
```
`crates/kernel/src/lib.rs`:
```rust
//! dms-kernel：纯契约 + 纯算法底座（零 IO，禁 sqlx/reqwest/axum，零 DMS 字符串）。
```

`crates/connector/Cargo.toml`:
```toml
[package]
name = "dms-connector"
version = "0.1.0"
edition = "2021"

[dependencies]
dms-kernel = { path = "../kernel" }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
futures = "0.3"
rust_decimal = "1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
sqlx = { version = "0.8", default-features = false, features = [
    "runtime-tokio", "tls-rustls", "mysql", "postgres", "chrono", "rust_decimal", "json",
] }
```
`crates/connector/src/lib.rs`:
```rust
//! dms-connector：全部对外 IO 唯一出口（MySQL/PG/LLM/embed/AGE）。全仓唯一能造 MySQL 池。
```

`crates/policy/Cargo.toml`:
```toml
[package]
name = "dms-policy"
version = "0.1.0"
edition = "2021"

[dependencies]
dms-kernel = { path = "../kernel" }
dms-connector = { path = "../connector" }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
sqlx = { version = "0.8", default-features = false, features = [
    "runtime-tokio", "tls-rustls", "mysql", "postgres", "chrono", "rust_decimal", "json",
] }
```
`crates/policy/src/lib.rs`:
```rust
//! dms-policy：行级数据权限 IO 侧（语义 1:1 复刻 Java DMS，唯一「改错=越权」模块）。
```

`crates/semantic/Cargo.toml`（**故意不含 dms-policy**）:
```toml
[package]
name = "dms-semantic"
version = "0.1.0"
edition = "2021"

[dependencies]
dms-kernel = { path = "../kernel" }
dms-connector = { path = "../connector" }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
futures = "0.3"
sqlx = { version = "0.8", default-features = false, features = [
    "runtime-tokio", "tls-rustls", "postgres", "chrono", "rust_decimal", "json",
] }
```
`crates/semantic/src/lib.rs`:
```rust
//! dms-semantic：业务知识全部落点（注册表/召回/组合器/校正器/列标注）。不依赖 policy。
```

`crates/agent/Cargo.toml`:
```toml
[package]
name = "dms-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
dms-kernel = { path = "../kernel" }
dms-connector = { path = "../connector" }
dms-policy = { path = "../policy" }
dms-semantic = { path = "../semantic" }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
futures = "0.3"
uuid = { version = "1", features = ["v4"] }
```
`crates/agent/src/lib.rs`:
```rust
//! dms-agent：唯一持有循环语义与路由分诊（Answerer 有序表 + AskRun 循环）。不依赖 axum。
```

- [ ] **Step 3: 让 server 依赖五个新 crate（不 use 任何符号）**

Modify `crates/server/Cargo.toml`，在 `[dependencies]` 末尾追加：
```toml
dms-kernel = { path = "../kernel" }
dms-connector = { path = "../connector" }
dms-policy = { path = "../policy" }
dms-semantic = { path = "../semantic" }
dms-agent = { path = "../agent" }
```
> 注意：会因「未使用的 crate 依赖」产生 5 条 warning，属预期，Task 2 起逐个消除。若想静默，可在 `main.rs` 顶部临时加 `#[allow(unused_crate_dependencies)]`，但**不要**加任何 `use`。

- [ ] **Step 4: 验证可编译且二进制行为不变**

Run（PowerShell，前缀 MinGW）:
```
cargo build 2>&1 | Select-Object -Last 5
```
Expected: `Finished dev profile`，仅 5 条 unused-crate warning，无 error。`target\debug\dms-ai-server.exe` 正常产出。

- [ ] **Step 5: 验证依赖方向无环且正确**

Run: `cargo tree -p dms-agent --prefix none 2>&1 | Select-String "dms-" | Select-Object -First 10`
Expected: agent 树下出现 kernel/connector/policy/semantic，**不出现** server/axum。
Run: `cargo tree -p dms-semantic --prefix none 2>&1 | Select-String "dms-policy"`
Expected: **空**（semantic 不依赖 policy，这是 spec 硬规则）。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml crates/kernel crates/connector crates/policy crates/semantic crates/agent crates/server/Cargo.toml
git commit -m "骨架: 6-crate workspace（kernel/connector/policy/semantic/agent/server），空壳+依赖方向，零行为变化"
```

---

## 自检（已执行）
- **spec 覆盖**：对应迁移步 1。✓
- **占位符扫描**：无 TBD/TODO；五个 Cargo.toml 与 lib.rs 均给出完整内容。✓
- **类型一致**：crate 名 `dms-*` 与 spec 第 1 节一致；`members` 列表与 Step 2/3 路径一致。✓
- **后续任务依赖**：Task 2 起将 use 这些 crate 的模块，本步已把依赖方向钉死。✓

## 备注（Windows 构建）
cargo 命令统一前缀：
`$env:PATH = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT.LLVM_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin;" + $env:PATH`
（Bash 的 mingw 与 cargo 链接冲突，必须走 PowerShell。）
