#!/usr/bin/env bash
# 一键迭代部署：本机打包 → 上传 → 服务器构建/重启/自检。
#
# 用法：DEPLOY_PW='服务器密码' [DEPLOY_HOST=HOST] [DEPLOY_PORT=22] [DEPLOY_USER=root]
#       [DMS_RUNTIME_ROOT=/opt/dms-ai] bash tools/deploy_update.sh
#
# RUNTIME_ROOT 持久化 settings/.secret_key/kbdata；APP_ROOT 只放每次可替换的源码。
set -euo pipefail
cd "$(dirname "$0")/.."

# 🔴 Windows/Git-Bash（MSYS）会把**看起来像 POSIX 路径**的参数改写成 Windows 路径：
# `/opt/dms-ai/src.tar.gz` → `D:/Program Files/Git/opt/dms-ai/src.tar.gz`。
# 于是远端 `base64 -d > "$part"` 写不进那个不存在的目录、命令当场退出，
# 客户端看到的却是一句莫名其妙的 `OSError: Socket is closed`（2026-08-14 实测，
# 排查掉了半小时：远端磁盘、inode、权限、工具链全查了一遍才发现路径被本地改写）。
# 这两个变量对 Linux/macOS 无副作用（不存在这个转换），所以无条件导出。
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

HOST="${DEPLOY_HOST:?DEPLOY_HOST 未设置}"
export DEPLOY_PW="${DEPLOY_PW:?DEPLOY_PW 未设置}" DEPLOY_HOST="$HOST"
export DEPLOY_USER="${DEPLOY_USER:-root}" DEPLOY_PORT="${DEPLOY_PORT:-22}"
RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
APP_ROOT="$RUNTIME_ROOT/app"
RELEASES_ROOT="$RUNTIME_ROOT/releases"
RELEASE_ID="$(date -u '+%Y%m%dT%H%M%SZ')-$$"
RELEASE_ROOT="$RELEASES_ROOT/$RELEASE_ID"
HEALTH_URL="http://172.17.0.1:8100/api/health"

case "$RUNTIME_ROOT" in
  /*) ;;
  *) echo "DMS_RUNTIME_ROOT 必须是绝对路径：$RUNTIME_ROOT" >&2; exit 2 ;;
esac
case "$RUNTIME_ROOT" in
  *[!A-Za-z0-9_./-]*) echo "DMS_RUNTIME_ROOT 含不支持的字符：$RUNTIME_ROOT" >&2; exit 2 ;;
esac

echo "== 1/5 本机校验 + 构建 + 打包（当前工作区 + fresh web/dist）"
# 🔴 `-U` 不能省，这条闸此前在 Windows 上**恒绿**：msys 的 grep 以文本模式读文件，
# 匹配前先把 CR 吃掉 —— 对一个逐行 \r\n 的脚本，`grep -c $'\r'` 返 0/rc=1（放行），
# 加 -U 才返 6/rc=0（开火）。而 msys 的 bash 能正常跑 CRLF 脚本，所以 `bash -n` 也一起放行。
# 代价是本机 1/5 全绿、CRLF 随 src.tar.gz 上服务器，Linux 侧 `bash scripts/server-build.sh`
# 报 `$'\r': command not found`，部署停在 3/5。部署包场景尤其要防：OneDrive 目录里
# .gitattributes 的 `*.sh text eol=lf` 不再起作用（那是 git 检出期机制）。
# 🔴 名单换成正判据：凡是会被 bash 执行的 .sh 全查，不再逐个点名 —— 点名的写法漏掉了
# web-update.sh 与 server-cleanup.sh，而这两个都在服务器上真跑。
for script in tools/*.sh scripts/*.sh; do
  [ -f "$script" ] || continue
  bash -n "$script" || { echo "shell 语法错误：$script" >&2; exit 2; }
  if LC_ALL=C grep -Uq $'\r' "$script"; then
    echo "shell 脚本必须使用 LF：$script" >&2
    exit 2
  fi
done
# 部署包模式：产物已在包里（那台机器没有 .git，也未必装了 npm），直接用现成 tar。
# 🔴 两个变量必须同时给：只给 src 会把**旧前端**留在现网，而部署照样打印成功 ——
# 这类「哑掉的降级」比当场报错难查得多，所以宁可在这里当场退出。
if [ -n "${DEPLOY_SRC_TAR:-}" ] || [ -n "${DEPLOY_WEB_TAR:-}" ]; then
  [ -n "${DEPLOY_SRC_TAR:-}" ] && [ -n "${DEPLOY_WEB_TAR:-}" ] || {
    echo "DEPLOY_SRC_TAR 与 DEPLOY_WEB_TAR 必须同时提供（只给一个会发布半套）" >&2; exit 2; }
  [ -s "$DEPLOY_SRC_TAR" ] || { echo "源码包不存在或为空：$DEPLOY_SRC_TAR" >&2; exit 2; }
  [ -s "$DEPLOY_WEB_TAR" ] || { echo "前端包不存在或为空：$DEPLOY_WEB_TAR" >&2; exit 2; }
  SRC_TAR="$DEPLOY_SRC_TAR"
  WEB_TAR="$DEPLOY_WEB_TAR"
  echo "部署包模式：跳过 git 打包与 npm 构建，直接用包内产物"
else
  mkdir -p target/tmp
  # 不能 archive HEAD：部署脚本的正常用法就是先在当前工作区验收、再发布当前工作区；只打
  # HEAD 会把尚未提交但已通过测试的修复静默丢掉，形成“本机已修、服务器仍是旧代码”。
  git ls-files -co --exclude-standard -z |
    while IFS= read -r -d '' path; do
      [ -e "$path" ] || [ -L "$path" ] || continue
      printf '%s\0' "$path"
    done | tar --null -czf target/tmp/src.tar.gz --files-from=-
  (cd web && npm ci && npm test && npm run build && tar -czf ../target/tmp/web-dist.tar.gz -C dist .)
  SRC_TAR=target/tmp/src.tar.gz
  WEB_TAR=target/tmp/web-dist.tar.gz
fi
ls -la "$SRC_TAR" "$WEB_TAR"

echo "== 2/5 上传（bput：base64 走 exec 通道，SFTP 坏也能传）"
python tools/_deploy.py bput "$SRC_TAR" "$RUNTIME_ROOT/src.tar.gz"
python tools/_deploy.py bput "$WEB_TAR" "$RUNTIME_ROOT/web-dist.tar.gz"

echo "== 3/5 新 release 解包 + Docker 构建（数分钟，耐心）"
python tools/_deploy.py run "set -e; test ! -e '$RELEASE_ROOT'; mkdir -p '$RELEASE_ROOT'; tar -xzf '$RUNTIME_ROOT/src.tar.gz' -C '$RELEASE_ROOT'; echo extracted:$RELEASE_ROOT" 120
# 若服务器拉 crates.io 慢：先在服务器放 /root/.cargo/config.toml（rsproxy 镜像）再构建，
# 纯净 Dockerfile 不内嵌镜像源（纪律见 docker/server/Dockerfile 头注）。
# 🔴 构建**不能挂在一条长活 SSH 连接上**（2026-08-14 实测连挂两次）：
# 服务器 Docker 构建要 5-10 分钟，这条链路上的连接撑不住，一断 `_deploy.py` 抛
# `Socket exception (10054)`、脚本退出，而远端构建进程收到 SIGHUP 一起死 ——
# 表现是「部署跑了十分钟，镜像还是旧的」，且退出码还可能是 0（管道吞掉了）。
# 改成 nohup 后台跑 + 客户端**短连接轮询** rc 文件：连接断多少次都不影响构建。
BUILD_LOG="$RUNTIME_ROOT/build-$RELEASE_ID.log"
python tools/_deploy.py run "cd '$RELEASE_ROOT' && rm -f '$BUILD_LOG.rc' && nohup sh -c 'bash scripts/server-build.sh > \"$BUILD_LOG\" 2>&1; echo \$? > \"$BUILD_LOG.rc\"' >/dev/null 2>&1 & echo build-started" 120
# 轮询间隔 30s 而不是 15s：一次构建最多开 ~20 条连接而不是 120 条 ——
# sshd 的 MaxStartups 默认 10:30:100，120 条短连接实测会被拒（`No existing session`）。
# `_deploy.py::client` 那侧也加了退避重试，两条一起才拦得住。
for _ in $(seq 1 60); do
  if python tools/_deploy.py run "test -f '$BUILD_LOG.rc' && cat '$BUILD_LOG.rc' || echo RUNNING" 60 2>/dev/null | grep -qv RUNNING; then
    break
  fi
  sleep 30
done
BUILD_RC="$(python tools/_deploy.py run "cat '$BUILD_LOG.rc' 2>/dev/null || echo 99" 60 | tr -d '[:space:]')"
python tools/_deploy.py run "tail -5 '$BUILD_LOG'" 60 || true
if [ "$BUILD_RC" != "0" ]; then
  echo "服务器构建失败（rc=$BUILD_RC），未切换 app，生产仍是旧版本" >&2
  exit 1
fi

echo "== 3.6/5 让宿主机 embed 服务跟上新 release（在切 app 之前，失败则生产原样不动）"
# 🔴 顺序有意：跑的是**新 release 里**的 embed-sync.sh，而 app 链接此刻还指向旧版 ——
# 向量/解析层先换、先自证健康，再切 API。这样新 embed_service.py 有问题时，
# 脚本自己回滚那份文件并非零退出，而生产的 API 一步都没动过。
# 反过来（先切 API 再同步）失败会留下「API 新、向量层半新不旧」的中间态。
python tools/_deploy.py run "DMS_RUNTIME_ROOT='$RUNTIME_ROOT' bash '$RELEASE_ROOT/scripts/embed-sync.sh'" 180

echo "== 4/5 原子切换 app → 新 release，重启失败则恢复旧 release + 更新 web 产物"
python tools/_deploy.py run "set -e; previous=''; if [ -L '$APP_ROOT' ]; then previous=\$(readlink -f '$APP_ROOT'); elif [ -d '$APP_ROOT' ]; then previous='$RELEASES_ROOT/pre-release-$RELEASE_ID'; mv '$APP_ROOT' \"\$previous\"; elif [ -e '$APP_ROOT' ]; then echo 'ERROR: $APP_ROOT 不是目录或符号链接' >&2; exit 2; fi; ln -sfn '$RELEASE_ROOT' '$RUNTIME_ROOT/app.next'; mv -Tf '$RUNTIME_ROOT/app.next' '$APP_ROOT'; if ! DMS_RUNTIME_ROOT='$RUNTIME_ROOT' DMS_SERVER_HOST=172.17.0.1 bash '$APP_ROOT/scripts/server-restart.sh'; then rm -f '$APP_ROOT'; if [ -n \"\$previous\" ]; then ln -sfn \"\$previous\" '$RUNTIME_ROOT/app.next'; mv -Tf '$RUNTIME_ROOT/app.next' '$APP_ROOT'; fi; exit 1; fi" 900
# 生产 Web 根目录是只读 bind mount；在宿主原子换目录并重启，失败自动恢复旧目录。
# 🔴 nginx.conf 同理是宿主机上的一份独立拷贝（容器 bind 的是 $RUNTIME_ROOT/docker/web/nginx.conf，
# 不是 release 里那份），改了配置不同步等于没改，而 nginx 照常服务旧规则 —— 又一处哑掉的降级。
# 用 `cp` 而不是 `mv`：bind mount 绑的是 inode，mv 换了 inode 容器还看着旧文件。
python tools/_deploy.py run "set -e; dst='$RUNTIME_ROOT/docker/web/nginx.conf'; src='$APP_ROOT/docker/web/nginx.conf'; if [ -f \"\$dst\" ]; then cmp -s \"\$src\" \"\$dst\" && echo 'nginx.conf 无变化' || { cp \"\$src\" \"\$dst\"; echo 'nginx.conf 已从 release 同步'; }; else echo \"跳过：\$dst 不存在（web 容器可能直接 bind release）\"; fi" 60
python tools/_deploy.py run "bash '$APP_ROOT/scripts/web-update.sh' '$RUNTIME_ROOT/web-dist.tar.gz' dms-ai-web" 180

echo "== 5/5 健康检查（HTTP 2xx 且 JSON ok=true）"
python tools/_deploy.py run "curl -fsS -m 5 '$HEALTH_URL' | python3 -c 'import json,sys; data=json.load(sys.stdin); print(json.dumps(data,ensure_ascii=False)); raise SystemExit(0 if data.get(\"ok\") is True else 1)'" 30

# 🔴 健康检查通过**之后**才清理（失败时旧镜像与旧 release 还是回滚材料，不能先删）。
# 不加这一步的代价：每次部署留下一个悬空 `<none>` 镜像（builder 阶段带整个 cargo target，
# 每个几 GB）+ 一份 BuildKit 缓存 + 一个 release 目录，纯增不减 —— 部署几次磁盘就满
# （2026-08-13 现网实测空间不足）。releases 保留最近 3 个当回滚位。
echo "== 5.5/5 清理历史产物（悬空镜像 / 构建缓存 / 旧 release，保留 3 个回滚位）"
python tools/_deploy.py run "DMS_RUNTIME_ROOT='$RUNTIME_ROOT' KEEP_RELEASES=3 bash '$APP_ROOT/scripts/server-cleanup.sh' --apply 2>&1 | tail -12" 600
echo
echo "部署完成。人工冒烟三题：本月销售额 / 现在总库存量是多少 / 市场费用的报销政策是什么"
