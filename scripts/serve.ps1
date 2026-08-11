# 在容器内跑后端（Windows 侧 SAC 强制态，本机 exe 起不来 —— 裁决 二·E）。
#
#   .\scripts\serve.ps1              # 重建容器并启动
#   .\scripts\serve.ps1 -Build       # 先 docker build 再启动
#   .\scripts\serve.ps1 -Cmd 'meta sync'   # 一次性子命令（不留容器）
#   .\scripts\serve.ps1 -Cmd 'why-not-compose --csv tools/why_gates.csv'   # 全量诊断→基线 CSV
#
# ⚠️ `-Cmd` 是按**空格**切参的（下面 `$Cmd -split ' '`），所以带空格的问句用这条路走不通；
# 单问请直接 `docker exec dms-ai-server /app/dms-ai-server why-not-compose "本月各品牌销售额"`。
# 好消息是它不再静默出错：多余位置参数现在由 `why-not-compose` 自己报错退出（见 main.rs）。
#
# settings.docker.json **运行时挂载**，不进镜像层：镜像可 `docker save`、可推仓库，
# 明文 DSN 与 LLM key 一旦进层就删不掉（F8）。
# kb_data 挂 volume：容器重建不能丢文件，否则 DB 里的 doc 行指向不存在的文件。
param(
    [switch]$Build,
    [string]$Cmd = ''
)
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\.."
$repo = (Get-Location).Path

# 本机容器部署使用 host.docker.internal:8078。先保证向量/解析链就绪，避免后端首次
# 上传碰到连接失败后进入 300 秒熔断；外部部署地址不由这个脚本托管。
# 缺文件时给友好文案再 throw，别抛 Get-Content 的裸错（run.ps1 有 settings.json 回退，
# 但下面 mounts 挂死 settings.docker.json，这里不能回退 —— 只统一友好文案这一半）
if (-not (Test-Path "$repo\settings.docker.json")) {
    throw '缺少 settings.docker.json，请从 settings.example.json 创建'
}
$runtimeSettings = Get-Content "$repo\settings.docker.json" -Raw | ConvertFrom-Json
# Trim 后再匹配：service_url 带尾随空格/换行会静默跳过 ensure-services，首次上传就撞 300s 熔断
if ("$($runtimeSettings.service_url)".Trim() -match '^http://host\.docker\.internal:8078/?$') {
    & "$PSScriptRoot\ensure-services.ps1" -Parser
} else {
    Write-Host 'service_url 非本机解析链（host.docker.internal:8078），跳过依赖检查'
}

if ($Build) {
    docker build -f docker/server/Dockerfile -t dms-ai-server .
    if ($LASTEXITCODE -ne 0) { throw 'build 失败' }
}

$env:MSYS_NO_PATHCONV = '1'
# 🔴 知识库落盘目录必须是「容器与宿主机看到同一个路径字符串」的目录，不能用 docker volume。
#
# 原因：文档解析的契约是 `DocService::parse(path)` —— **传路径不传字节流**（`connector/src/doc.rs`
# 那行注释写着「同机部署」）。真实部署里服务端与 Python 文档服务同机，这没问题；
# 但本机为绕 SAC 把**服务端塞进了容器、文档服务留在宿主机**，于是容器写
# `/app/kb_data/<id>.md`（volume 内），宿主机 Python 去读同一个字符串 → 读不到 → 404，
# **知识库上传在容器部署下必然失败**。这是开发环境断裂，不是产品缺陷。
#
# 解法零代码：挂 `D:\kbdata` → `/kbdata` 并令 `kb_root=/kbdata`。
# Windows 上 Python 把 `/kbdata/x` 解析到**当前驱动器**（embed 服务的 CWD 在 D:）→ `D:\kbdata\x`，
# 两侧于是指向同一个文件。**必须是 D 盘**：换成别的盘符或从别的盘启动 embed 服务，这个巧合就断。
if (-not (Test-Path 'D:\kbdata')) { New-Item -ItemType Directory -Force 'D:\kbdata' | Out-Null }
$mounts = @(
    # settings 运行时挂载不进镜像层（F8）。**可写**：`settings_api`（页面编辑配置）
    # 原地写它 —— 单文件挂载点 inode 固定，原地写宿主机立即可见；改回 :ro 那天
    # 页面保存会变成 500「写 settings.json 失败」。
    '-v', "${repo}\settings.docker.json:/app/settings.json",
    '-v', 'D:\kbdata:/kbdata',
    # 🔴 tools/ 必挂，否则容器里的全量诊断**必然**跑不起来：`why-not-compose` 无参模式读
    # **相对路径** `tools/eval_cases.json`，而容器 cwd 是 Dockerfile 的 `WORKDIR /app`
    # → 解析成 `/app/tools/eval_cases.json`。上一轮为了跑这条诊断是手工
    # `docker exec mkdir` + `docker cp` 才凑出输入的 —— 判据的输入不在容器里就不是判据。
    #
    # **可写**（无 `:ro`）：`why-not-compose --csv <path>` 要把逐题门分布写回宿主机当基线文件
    #（连跑两次逐列全等才算这把尺子无抖动）。
    #
    # 凭据红线：挂的是**仓库根下的 tools/ 子目录，不是仓库根** ——
    # `settings.json` / `settings.docker.json` 都在仓库根，不在 tools/ 里，不会被这行带进容器；
    # 且这是**运行时挂载**，一个字节都不进镜像层（同 settings 那行的理由，F8）。
    #
    # `tools/` 曾有两处自有 PG 的明文口令（`embed_service.py` / `cleanup_autodiscover.py`），
    # 已挪进 `settings.json` 由 `tools/settings.py` 统一读取；那个文件的自检里有一条
    # **纪律断言**（`tools/*.py` 不许出现字面量口令赋值）钉着，别再往回写。
    '-v', "${repo}\tools:/app/tools",
    # PG 在宿主机的 15433，LLM/embed 也在宿主机 —— 容器内 127.0.0.1 是容器自己
    '--add-host', 'host.docker.internal:host-gateway'
)

if ($Cmd) {
    # 一次性任务：stdout 是子命令的 JSON（判官脚本要解析），日志走 stderr
        # `Where-Object { $_ }` 滤掉空 token：`-split ' '` 遇到双空格或尾空格会产出空元素
    # （实测 `'why-not-compose ' -split ' '` → [why-not-compose][""]），而空串曾被
    # `why-not-compose` 当成合法问句 —— 全量 38 题诊断会**静默降级成「问一句空话」**。
    # Rust 侧已加守卫（空位置参数即 Err），这里再滤一道：两头都堵，别指望只有一处对。
    & docker run --rm @mounts dms-ai-server /app/dms-ai-server @($Cmd -split ' ' | Where-Object { $_ })
    exit $LASTEXITCODE
}

docker rm -f dms-ai-server 2>$null | Out-Null
# 🔴 `127.0.0.1:8100:8100` 而不是 `8100:8100`：裸写端口 docker 会绑 0.0.0.0，
# 实测 `docker ps` 显示 `0.0.0.0:8100->8100/tcp, [::]:8100->8100/tcp` —— **对整个局域网开放**。
# 而服务侧的认证在 `insecure_login_fallback=true`（本机判官脚本要它）时等于没有认证：
# 局域网内任何人 `curl -d '{"question":"…","login_name":"admin"}'` 就是管理员。
# 容器**内部**仍监听 0.0.0.0（`settings.docker.json` 的 listen），否则宿主连不进去 ——
# 收窄的是宿主这一侧的发布面。要给别的机器用就上反向代理 + 真登录，别把这一行改回去。
& docker run -d --name dms-ai-server -p 127.0.0.1:8100:8100 @mounts dms-ai-server
if ($LASTEXITCODE -ne 0) { throw 'run 失败' }

# 冷启动会迁移/灌入元数据与权限档案，实测约 28s；原 21s 窗口会把正常启动误判失败。
for ($i = 0; $i -lt 90; $i++) {
    Start-Sleep -Milliseconds 700
    try {
        $r = Invoke-RestMethod http://127.0.0.1:8100/api/health -TimeoutSec 2
        Write-Host ($r | ConvertTo-Json -Depth 6)
        exit 0
    } catch {}
}
Write-Host '[FAIL] health 90 次未通，容器日志：' -ForegroundColor Red
docker logs --tail 40 dms-ai-server
exit 1
