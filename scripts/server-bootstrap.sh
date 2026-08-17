#!/usr/bin/env bash
# 全新机器从零到「可以跑 server-build.sh + server-restart.sh」的前置铺设。**幂等**：每一步
# 先看现状，已就位就打印 KEEP/SKIP 并跳过，重复跑不会推翻已有环境。
#
# 为什么单独一个脚本：tools/deploy_update.sh 只做「更新已有环境」，它**假设**下面这些已经存在
# —— PG 容器、venv、systemd 单元 dms-ai-embed、settings/.secret_key、web 容器。这些假设在现网
# 是人手一次性搭起来的，仓库里此前一份记录都没有（`git grep systemctl` / `*.service` 全空），
# 于是「换台机器重来一遍」这件事只活在某个人的记忆里。
#
# 用法（在服务器上，源码已解到某个 release 目录）：
#   DMS_RUNTIME_ROOT=/opt/dms-ai bash <release>/scripts/server-bootstrap.sh
#
# 种子文件（部署包上传到 $DMS_RUNTIME_ROOT/seed/，本脚本只读不改）：
#   settings.docker.json          必需——含 enc:v1 密文的 DSN 与 LLM key
#   secret.key                    必需——解 enc:v1 的主钥，与上面**成对**，缺一即全废
#   requirements-embed.lock.txt   可选——现网 pip freeze，优先于仓库里的 requirements
#   registry_snapshot.json        可选——业务字典种子，由部署脚本在 API 起来之后导入
set -euo pipefail

die() { echo "ERROR: $*" >&2; exit 1; }
step() { echo; echo "── $*"; }

APP_ROOT="$(cd "$(dirname "$0")/.." && pwd -P)"
RUNTIME_ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
RUNTIME_ROOT="${RUNTIME_ROOT%/}"
SEED_DIR="${DMS_SEED_DIR:-$RUNTIME_ROOT/seed}"
SETTINGS_FILE="$RUNTIME_ROOT/settings.docker.json"
SECRET_FILE="$RUNTIME_ROOT/.secret_key"
VENV="$RUNTIME_ROOT/venv"
UNIT=/etc/systemd/system/dms-ai-embed.service

case "$RUNTIME_ROOT" in
  /*) ;;
  *) die "DMS_RUNTIME_ROOT 必须是绝对路径：$RUNTIME_ROOT" ;;
esac

step "0/9 前置工具"
for bin in docker python3 curl; do
  command -v "$bin" >/dev/null || die "缺少 $bin，请先安装"
done
docker compose version >/dev/null 2>&1 || die "缺少 docker compose 插件（v2）"
command -v systemctl >/dev/null || die "缺少 systemd：本脚本用 systemd 单元托管 embed 服务"
echo "OK docker=$(docker --version | awk '{print $3}' | tr -d ,) python3=$(python3 -V | awk '{print $2}')"

step "1/9 运行时目录"
mkdir -p "$RUNTIME_ROOT/kbdata" "$RUNTIME_ROOT/releases"
echo "OK $RUNTIME_ROOT/{kbdata,releases}"

step "2/9 配置与密钥（已存在则保留现网那份，绝不覆盖）"
[ -d "$SEED_DIR" ] || die "缺少种子目录：$SEED_DIR"
if [ -e "$SETTINGS_FILE" ]; then
  echo "KEEP $SETTINGS_FILE（已存在）"
else
  [ -s "$SEED_DIR/settings.docker.json" ] || die "缺少 $SEED_DIR/settings.docker.json"
  install -m 600 "$SEED_DIR/settings.docker.json" "$SETTINGS_FILE"
  echo "OK  装入 $SETTINGS_FILE"
fi
if [ -e "$SECRET_FILE" ]; then
  echo "KEEP $SECRET_FILE（已存在）"
else
  [ -s "$SEED_DIR/secret.key" ] || die "缺少 $SEED_DIR/secret.key"
  install -m 600 "$SEED_DIR/secret.key" "$SECRET_FILE"
  echo "OK  装入 $SECRET_FILE"
fi
[ "$(wc -c < "$SECRET_FILE")" -ge 32 ] || die "运行时密钥必须至少 32 字节：$SECRET_FILE"

step "3/9 app 链接（systemd 单元与后续构建都按它找源码）"
if [ -L "$RUNTIME_ROOT/app" ]; then
  echo "KEEP app -> $(readlink -f "$RUNTIME_ROOT/app")"
elif [ -e "$RUNTIME_ROOT/app" ]; then
  die "$RUNTIME_ROOT/app 已存在且不是符号链接，请人工处理"
else
  ln -sfn "$APP_ROOT" "$RUNTIME_ROOT/app"
  echo "OK  app -> $APP_ROOT"
fi

step "4/9 元数据 PG（docker/age：postgres16 + AGE + pgvector + pg_trgm）"
# 密码与绑定地址都从 settings 的 pg_url 反解 —— 不让运维再手输一遍，也就不会输错。
# 🔴 只打印用户名/主机/端口，密码从不落到 stdout。
read -r PG_USER PG_HOST PG_PORT PG_DB PG_PW <<EOF
$(DMS_SECRET_KEY="$(cat "$SECRET_FILE")" DMSAI_SETTINGS="$SETTINGS_FILE" \
  python3 - "$APP_ROOT" <<'PY'
import sys, os
sys.path.insert(0, os.path.join(sys.argv[1], "tools"))
from urllib.parse import urlsplit, unquote
import settings as st
u = urlsplit(st.load()["pg_url"])
print(u.username or "postgres", u.hostname or "", u.port or 5432,
      (u.path or "/dms_ai").lstrip("/"), unquote(u.password or ""))
PY
)
EOF
[ -n "$PG_PW" ] || die "无法从 settings 的 pg_url 反解 PG 密码（凭据能解开吗？）"
# 容器里的 API 用 host.docker.internal 连 PG（= Docker 网桥网关），PG 必须绑到网关地址上；
# 绑 127.0.0.1 的话容器侧永远连不上，而 API 只会报「数据库不可达」这类下游症状。
PG_BIND=172.17.0.1
if [ "$PG_HOST" != "host.docker.internal" ] && [ "$PG_HOST" != "172.17.0.1" ]; then
  PG_BIND="$PG_HOST"
fi
echo "settings 里的 PG：$PG_USER@$PG_HOST:$PG_PORT/$PG_DB（容器绑定地址取 $PG_BIND）"
if docker inspect --type container dms-ai-pg >/dev/null 2>&1; then
  echo "KEEP 容器 dms-ai-pg 已存在"
else
  ( cd "$APP_ROOT/docker/age" \
    && DMS_AI_PG_PASSWORD="$PG_PW" DMS_AI_PG_BIND="$PG_BIND" docker compose up -d --build )
  echo "OK  dms-ai-pg 已拉起，等待 initdb 完成…"
fi
for _ in $(seq 1 60); do
  docker exec dms-ai-pg pg_isready -U "$PG_USER" -d "$PG_DB" >/dev/null 2>&1 && break
  sleep 2
done
docker exec dms-ai-pg pg_isready -U "$PG_USER" -d "$PG_DB" >/dev/null 2>&1 \
  || die "PG 未在 120 秒内就绪，看 docker logs dms-ai-pg"
# 扩展只由 init 脚本建在默认库上；库名与 compose 的 POSTGRES_DB 不一致时这里会当场红。
MISSING="$(docker exec dms-ai-pg psql -U "$PG_USER" -d "$PG_DB" -tAc \
  "SELECT string_agg(x, ',') FROM unnest(ARRAY['age','vector','pg_trgm']) AS x \
   WHERE x NOT IN (SELECT extname FROM pg_extension)")"
[ -z "$MISSING" ] \
  || die "库 $PG_DB 缺扩展：$MISSING（init 脚本只在数据目录为空的首次启动跑，见 docker/age/init/01-extensions.sql）"
echo "OK  PG 就绪，age/vector/pg_trgm 三个扩展都在"

step "5/9 Python venv（解析/向量服务与快照导入都跑在它上面）"
if [ -x "$VENV/bin/python3" ]; then
  echo "KEEP $VENV（已存在）"
else
  python3 -m venv "$VENV" || die "建 venv 失败（需要 python3-venv）"
  echo "OK  建好 $VENV"
fi
REQ="$SEED_DIR/requirements-embed.lock.txt"
[ -s "$REQ" ] || REQ="$APP_ROOT/tools/requirements-embed.txt"
"$VENV/bin/python3" -m pip install --quiet --upgrade pip >/dev/null
"$VENV/bin/python3" -m pip install --quiet -r "$REQ" || die "pip 装依赖失败：$REQ"
# 判据：enc:v1 解得开才算装对（cryptography 缺了正是这里红）。只打印布尔，不打印明文。
DMS_SECRET_KEY="$(cat "$SECRET_FILE")" DMSAI_SETTINGS="$SETTINGS_FILE" \
  "$VENV/bin/python3" - "$APP_ROOT" <<'PY' || die "venv 读不通配置：凭据解不开或依赖不全"
import sys, os
sys.path.insert(0, os.path.join(sys.argv[1], "tools"))
import settings as st
cfg = st.load()
bad = [k for k in ("pg_url", "mysql_url", "llm_api_key") if str(cfg.get(k, "")).startswith("enc:v1:")]
raise SystemExit(f"仍是密文，解密失败：{bad}" if bad else 0)
PY
echo "OK  依赖装好（$REQ），enc:v1 凭据可解密"

step "6/9 宿主机解析依赖（扫描件 OCR / Office 预览）"
# 缺了不致命：/health 的 parse_ok 会把对应格式报 false，server-restart.sh 只要求
# text=true 且 xlsx/pdf/docx 至少一种。所以这里失败只告警，不中断。
if command -v tesseract >/dev/null; then
  echo "KEEP tesseract $(tesseract --version 2>&1 | head -1 | awk '{print $2}')"
elif command -v apt-get >/dev/null; then
  apt-get update -qq && apt-get install -y -qq tesseract-ocr tesseract-ocr-chi-sim \
    && echo "OK  装好 tesseract + 中文简体语言包" \
    || echo "WARN tesseract 安装失败：扫描件走不了 OCR 档，其余格式不受影响"
else
  echo "WARN 无 apt-get 且无 tesseract：扫描件走不了 OCR 档"
fi

step "7/9 向量·精排·解析服务（容器形态）"
# 🔴 2026-08-17 起这套服务是**容器**，不再是宿主机 venv + systemd 单元。
# 换形态的原因是一天之内撞到的两笔账：
#   ① 第二台生产机上压根没装单元，8078 上是个手工起的裸 python —— 重启即失、
#      部署换代码也不跟着变，而 `/api/health` 全绿（哑掉的降级）；
#   ② 单元跑的 `$RUNTIME_ROOT/tools/embed_service.py` 与 release 里那份是两份拷贝，
#      靠人手同步（`scripts/embed-sync.sh` 就是给它写的补丁）。
# 装进镜像后：依赖、代码、启动方式一起进版本库，`--restart unless-stopped` 天然开机自启，
# 部署换代码＝重建镜像换容器，两份拷贝的问题从根上消失。
# venv 仍然要有 —— `registry_snapshot.py` 的快照导入跑在它上面（第 5 步），那是另一件事。
DMS_RUNTIME_ROOT="$RUNTIME_ROOT" DMS_EMBED_TAKEOVER=1 bash "$APP_ROOT/scripts/embed-install.sh" \
  || die "向量/解析服务安装失败"

step "8/9 Web 容器 dms-ai-web（nginx 托管前端产物）"
# web-update.sh 只更新**已存在**的容器，容器不存在时它打印 SKIP 就退出 0 ——
# 全新机器上那正是「部署全绿但网站根本没有」的来源，所以这里必须先把容器建出来。
mkdir -p "$RUNTIME_ROOT/web/dist" "$RUNTIME_ROOT/docker/web"
cp "$APP_ROOT/docker/web/nginx.conf" "$RUNTIME_ROOT/docker/web/nginx.conf"
[ -s "$RUNTIME_ROOT/web/dist/index.html" ] \
  || echo '<!doctype html><meta charset="utf-8"><title>dms-ai</title>前端产物待部署' \
     > "$RUNTIME_ROOT/web/dist/index.html"
if docker inspect --type container dms-ai-web >/dev/null 2>&1; then
  echo "KEEP 容器 dms-ai-web 已存在"
else
  # 🔴 --add-host 不能省：nginx.conf 里 proxy_pass 写的是 host.docker.internal，
  # 它在 nginx **启动期**解析，Linux 上没有这个映射会 emerg 拒启 —— 全站宕，不只 /api 挂。
  docker run -d --name dms-ai-web --restart unless-stopped \
    --add-host host.docker.internal:host-gateway \
    -p "${DMS_WEB_PORT:-5180}:80" \
    --mount "type=bind,source=$RUNTIME_ROOT/web/dist,target=/usr/share/nginx/html,readonly" \
    --mount "type=bind,source=$RUNTIME_ROOT/docker/web/nginx.conf,target=/etc/nginx/conf.d/default.conf,readonly" \
    nginx:1.27-alpine >/dev/null \
    || die "dms-ai-web 启动失败"
  echo "OK  dms-ai-web 已拉起（宿主端口 ${DMS_WEB_PORT:-5180}）"
fi

step "9/9 完成"
cat <<EOF
前置已就绪。剩下两步由部署脚本接手（tools/deploy_update.sh 全包）：
  1) bash $RUNTIME_ROOT/app/scripts/server-build.sh   # 构建 dms-ai-server 镜像，5-10 分钟
  2) DMS_RUNTIME_ROOT=$RUNTIME_ROOT bash $RUNTIME_ROOT/app/scripts/server-restart.sh
API 起来之后再导一次业务字典种子（幂等，决定问数准确性）：
  cd $RUNTIME_ROOT && DMS_SECRET_KEY="\$(cat .secret_key)" DMSAI_SETTINGS=$SETTINGS_FILE \\
    $VENV/bin/python3 app/tools/registry_snapshot.py import $SEED_DIR/registry_snapshot.json
EOF
