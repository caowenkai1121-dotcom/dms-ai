# 解析服务（容器内）：build / up / down / probe。
#
#   .\scripts\parser.ps1 build
#   .\scripts\parser.ps1 up        # 起容器，宿主机 8078 → 容器 8077
#   .\scripts\parser.ps1 probe     # 造 5 种真夹具 + 逐格式解析，打印实际解析出的文字
#   .\scripts\parser.ps1 down
#
# 为什么要这个容器：宿主机的 lxml 编译扩展被 Smart App Control 拦死（`DLL load failed while
# importing etree`），实测宿主机 :8077/health → parse_ok.docx=false、pptx=false，
# 即业主点名的 word/ppt 在这台机器上恒不可用。容器里没有 SAC。理由全文见 docker/parser/Dockerfile。
#
# 端口为什么默认 8078 而不是 8077：宿主机 8077 上的 embed 服务**正在跑**（主线程在用它测量），
# 抢端口会打断它。8078 是 8077 的**完全替身**：/parse 与 /chunk 本地做（全格式），
# /embed 与 /health 透传给宿主机 8077。切过去只改 settings.docker.json 的 `service_url`，
# Rust 侧一行不改（`service_url` 是单一键，embed 与文档服务同址 —— 裁决 V1）。
#
# 🔴 凭据：不传任何凭据给容器，也不挂 settings.json —— 本服务只解析文件、不连库。
# 将来真要连 PG，照 serve.ps1 挂 `settings.docker.json:/app/settings.json:ro`（运行时挂载，不进层）。
#
# 反向验证（打坏某个解析器 → 探针必须明说不支持，而不是静默返空）：
#   docker exec dms-ai-parser pip uninstall -y python-docx
#   docker restart dms-ai-parser; .\scripts\parser.ps1 probe     # docx 应报 unsupported
#   docker exec dms-ai-parser pip install python-docx==1.1.2 ; docker restart dms-ai-parser
param(
    [ValidateSet('build', 'up', 'down', 'probe')]
    [string]$Action = 'up',
    [ValidateRange(1,65535)]
    [int]$Port = 8078
)
$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\.."
$repo = (Get-Location).Path
$name = 'dms-ai-parser'
$base = "http://127.0.0.1:$Port"

# 🔴 kb_data 目录必须是「容器与宿主机看到同一个路径字符串」的目录（serve.ps1 有同一段理由）：
# `DocService::parse(path)` 传的是**路径不是字节流**，服务端与写文件那侧必须指向同一个文件。
# D:\kbdata → /kbdata，`kb_root=/kbdata`；Windows 上 Python 把 `/kbdata/x` 解到当前驱动器。
if (-not (Test-Path 'D:\kbdata')) { New-Item -ItemType Directory -Force 'D:\kbdata' | Out-Null }
$env:MSYS_NO_PATHCONV = '1'
$mounts = @(
    # tools/ 必挂：解析/分块/能力上报的真相源全是 tools/embed_service.py（含它的 `CAPS` 表：
    # 旧 Office 走 soffice、图片走 pytesseract）。本镜像只提供它需要的依赖 + 绑 0.0.0.0，
    # 不 fork 它一行。`:ro` —— 解析服务不该写仓库。
    # 凭据红线：挂的是 tools/ 子目录，不是仓库根（settings*.json 在仓库根，不会被带进来）。
    '-v', "${repo}\tools:/app/tools:ro",
    '-v', 'D:\kbdata:/kbdata',
    '--add-host', 'host.docker.internal:host-gateway',
    # /embed 透传给宿主机 embed 服务：fastembed + 95MB 模型没必要再进本镜像一份。
    # ⚠️ 宿主机 embed_service.serve 默认绑 127.0.0.1 —— 从容器连不上；serve 有 host 参数，
    # 建议绑 172.17.0.1（docker 网桥，只放本机容器进），不要绑 0.0.0.0（会把解析/向量面
    # 暴露给整个网段，与 embed_service.py 自己的警告口径一致）。
    # 没配通也不会静默：/embed 会回 503 明说，不是回空向量。
    '-e', 'EMBED_UPSTREAM=http://host.docker.internal:8077'
)

# 🔴 探针的失败必须进**退出码**，不能只是彩色文字。
# 这一条是评审实测抓出来的：原来 `Probe-Format` 遇 HTTP≠200 只 `Write-Host` 黄字然后 `return`，
# 「解析成功但零文本」只 Write-Host 红字然后 `return` —— 两条都不置退出码。
# 于是**五种格式全部 422 时 `probe` 仍 exit 0**，而交给业主的验收话术正是「非 0 退出即红」。
# 人眼看到红、退出码是绿，业主看的是退出码 —— 这就是又一条恒真判据。
$script:bad = @()

function Probe-Format($path, $label) {
    $body = @{ path = $path } | ConvertTo-Json -Compress
    $r = Invoke-WebRequest "$base/parse" -Method POST -Body $body -ContentType 'application/json' `
        -TimeoutSec 300 -SkipHttpErrorCheck
    $j = $r.Content | ConvertFrom-Json
    if ($r.StatusCode -ne 200) {
        Write-Host ("[{0,-6}] HTTP {1}  {2}" -f $label, $r.StatusCode, $r.Content) -ForegroundColor Yellow
        $script:bad += $label
        return
    }
    $txt = (($j.blocks | ForEach-Object { $_.text }) -join ' / ') -replace '\s+', ' '
    if (-not $txt) { $txt = ($j.sheets | ConvertTo-Json -Compress -Depth 4) }
    if (-not $txt) {
        # 静默返空 = 文档「已入库 0 块」，用户以为传上去了。探针必须把它判红。
        Write-Host ("[{0,-6}] 解析成功但零文本 —— 这就是静默返空，判红" -f $label) -ForegroundColor Red
        $script:bad += $label
        return
    }
    $head = $txt.Substring(0, [Math]::Min(100, $txt.Length))
    Write-Host ("[{0,-6}] blocks={1} pages={2} sheets={3}  {4}" -f `
            $label, $j.blocks.Count, $j.page_count, $j.sheets.Count, $head) -ForegroundColor Green
}

# 「某个只出现在第 N 帧/第 N 页的唯一 token 必须进块里」。
# 这是「静默丢内容」唯一可判的形状：HTTP 200、blocks 非空、肉眼看着像成功，而内容少了。
function Probe-Token($path, $token, $label) {
    $body = @{ path = $path } | ConvertTo-Json -Compress
    $r = Invoke-WebRequest "$base/parse" -Method POST -Body $body -ContentType 'application/json' `
        -TimeoutSec 600 -SkipHttpErrorCheck
    if ($r.StatusCode -ne 200) {
        Write-Host ("[{0,-8}] HTTP {1}  {2}" -f $label, $r.StatusCode, $r.Content) -ForegroundColor Yellow
        $script:bad += $label
        return
    }
    $j = $r.Content | ConvertFrom-Json
    $all = ($j.blocks | ForEach-Object { $_.text }) -join ' '
    if ($all -notmatch [regex]::Escape($token)) {
        Write-Host ("[{0,-8}] token [{1}] 不在块里 —— 静默丢内容（blocks={2} pages={3}）" -f `
                $label, $token, $j.blocks.Count, $j.page_count) -ForegroundColor Red
        $script:bad += $label
        return
    }
    Write-Host ("[{0,-8}] blocks={1} pages={2} token[{3}] ✅  notes=[{4}]" -f `
            $label, $j.blocks.Count, $j.page_count, $token, ($j.notes -join '; ')) -ForegroundColor Green
}

switch ($Action) {
    'build' {
        # build context 只有 docker/parser/（仓库根有 target/ 与 settings.json，没必要送进 daemon）
        docker build -f docker/parser/Dockerfile -t $name docker/parser
        if ($LASTEXITCODE -ne 0) { throw 'build 失败' }
        docker images $name
    }
    'down' { docker rm -f $name 2>$null | Out-Null; Write-Host "$name 已停" }
    'up' {
        docker rm -f $name 2>$null | Out-Null
        # 🔴 **必须绑 `127.0.0.1`，不许写成 `-p "8078:8077"`（那等于 0.0.0.0）。**
        #
        # 实测过的暴露面：`/parse` 是**无鉴权的任意路径读文件**接口 ——
        # `POST :8078/parse {"path":"/etc/passwd","mime":"text/plain"}` → **HTTP 200 原样返回全文**，
        # `{"path":"/app/tools/settings.py"}` 同样返回源码。而这个容器挂着 `D:\kbdata`
        # （全部客户文档，RW）。发布到 0.0.0.0 = 同网段任何机器都能把知识库逐份读走。
        #
        # 宿主机上同一份 `serve()` 绑的是 `127.0.0.1`（只有本机能打）；容器里为了让 docker 网卡
        # 收到包必须绑 0.0.0.0 —— 但那只覆盖**容器网卡**，覆盖不了 `-p` 的**发布面**。
        # 「容器本身就是沙箱边界」这句话在有 `-p` 的时候不成立。
        #
        # 另有一道纵深防御在 `docker/parser/parse_service.py::guard_path`（path 必须落在
        # `PARSE_ROOTS` 之内）：绑回环只是收容，真正的修法是那一道 ——
        # 因为下一个人还会再写一次 `-p`。
        & docker run -d --name $name -p "127.0.0.1:${Port}:8077" @mounts $name
        if ($LASTEXITCODE -ne 0) { throw 'run 失败' }
        for ($i = 0; $i -lt 30; $i++) {
            Start-Sleep -Milliseconds 700
            try {
                Write-Host ((Invoke-RestMethod "$base/health" -TimeoutSec 3) | ConvertTo-Json -Depth 5)
                exit 0
            } catch {}
        }
        Write-Host '[FAIL] health 30 次未通，容器日志：' -ForegroundColor Red
        docker logs --tail 40 $name
        exit 1
    }
    'probe' {
        Write-Host '--- health ---'
        Write-Host ((Invoke-RestMethod "$base/health" -TimeoutSec 5) | ConvertTo-Json -Depth 5)
        Write-Host '--- 造夹具（容器内；宿主机造不出来，lxml 被 SAC 拦）---'
        # 字体只在造 png/pdf 夹具时要（要真中文字形才能测 OCR），故只在这一步挂宿主机字体目录
        & docker run --rm @mounts -v 'C:\Windows\Fonts:/hostfonts:ro' $name `
            python /app/make_fixtures.py /kbdata/_probe /hostfonts/simhei.ttf
        if ($LASTEXITCODE -ne 0) { throw '造夹具失败' }
        Write-Host '--- 逐格式解析（前 100 字）---'
        Probe-Format '/kbdata/_probe/fixture.docx' 'docx'
        Probe-Format '/kbdata/_probe/fixture.pptx' 'pptx'
        Probe-Format '/kbdata/_probe/fixture.pdf'  'pdf'
        Probe-Format '/kbdata/_probe/fixture.png'  'png'
        Probe-Format '/kbdata/_probe/fixture.doc'  'doc'
        # 🔴 「静默丢内容」三条：HTTP 200 + 少了内容，肉眼看回答**看不出来** ——
        # 判据必须是「只出现在第 2 帧/第 2 页的那个唯一 token 有没有进块里」。
        # 三条各自的原缺陷：多帧 TIFF 只 OCR 第 0 帧；混合 PDF 静默丢扫描页；
        # 整份扫描件靠 `-----` 冒充成「已入库 1 块」。
        Write-Host '--- 静默丢内容三条（唯一 token 必须进块）---'
        & docker run --rm @mounts -v 'C:\Windows\Fonts:/hostfonts:ro' `
            -v "${repo}\docker\parser:/mk:ro" $name `
            python /mk/make_silent_fixtures.py /kbdata/_silent /hostfonts/simhei.ttf | Out-Null
        if ($LASTEXITCODE -ne 0) { throw '造静默夹具失败' }
        Probe-Token '/kbdata/_silent/multiframe.tif' 'TIFFPAGE2-7788' '多帧tif'
        Probe-Token '/kbdata/_silent/mixed.pdf' 'PDFOCR2-9911' '混合pdf'
        Probe-Token '/kbdata/_silent/scanned.pdf' 'SCANONLY-3344' '扫描pdf'
        Write-Host '--- 上游自带判据 tools/parse_probe.py（xlsx/pdf/空 sheet 三条契约）---'
        # 用一次性容器跑，两个原因：
        # ① 它要往 tools/kb_fixtures/ 写夹具，而服务容器的 tools/ 是 `:ro`（解析服务不该写仓库）；
        # ② 它的 BASE 写死 127.0.0.1:8077 —— `--network container:` 借服务容器的网络栈就打得中，
        #    不必改它一个字。注意这条**不能**splat $mounts：`--add-host` 与 `--network container:`
        #    互斥（docker 直接报 conflicting options）。
        # 夹具输出目录要同时满足三件事，缺一条就红（三条都实测踩过）：
        # ① 落在解析服务的 `PARSE_ROOTS`（默认 `/kbdata:/tmp`）之内 —— 否则 `guard_path` 403。
        #    那道守卫是对的（`/parse` 曾是无鉴权任意文件读），**不许为了让探针跑通去放宽它**：
        #    把 `/app/tools` 加进允许根就等于把源码与 settings 又读回来了。
        # ② 在**两个容器都挂到的同一个宿主机目录**上。`--network container:` 只共享网络栈，
        #    **不共享文件系统** —— 写探针容器自己的 `/tmp`，服务容器一侧是 `404 not_found`（实测）。
        # ③ 可写（服务容器的 `tools/` 是 `:ro`，写不进去）。
        # `/kbdata` 三条全中。
        & docker run --rm --network "container:$name" -v "${repo}\tools:/app/tools" `
            -v 'D:\kbdata:/kbdata' -e 'PARSE_PROBE_OUT=/kbdata/_probe_upstream' `
            $name python -u /app/tools/parse_probe.py
        if ($LASTEXITCODE -ne 0) { Write-Host '[FAIL] 上游判据非 0 退出' -ForegroundColor Red; exit 1 }
        # 逐格式的失败在这里才结算（见 $script:bad 上方那段注释）
        if ($script:bad.Count) {
            Write-Host ("[FAIL] {0} 种格式没通过：{1}" -f $script:bad.Count, ($script:bad -join ', ')) `
                -ForegroundColor Red
            exit 1
        }
        Write-Host '[ok] 5 种格式全部解析出文本，上游判据也绿' -ForegroundColor Green
    }
}
