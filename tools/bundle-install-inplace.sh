#!/usr/bin/env bash
# dms-ai 就地升级安装器 —— 在**服务器上**跑，认现网那套「源码平铺在 /opt/dms-ai」的布局。
#
# 为什么是就地而不是 app+releases：1.95.7.181 现网就是平铺布局（源码直接在
# /opt/dms-ai 根上，没有 app 链接、没有 releases/）。硬把它改成另一套目录结构，
# 风险全落在一次生产升级上，不值。这个脚本按现状来。
#
# 用法（服务器上，解开包之后）：
#   bash 安装.sh              正常升级
#   bash 安装.sh --dry-run    只检查不动任何东西
#
# 它做什么（每步幂等，重复跑收敛）：
#   0 前置检查        docker / python3 / /opt/dms-ai 在不在
#   1 备份            会被替换的目录另存一份（沿用现网 rollback-before-<日期> 的习惯）
#   2 同步源码        settings / .secret_key / kbdata / venv / web/dist **一律不碰**
#   3 装配置          缺了才装（cp -n），绝不覆盖现网那份
#   4 向量服务转容器  替掉 `nohup ... &` 起的裸进程 —— 那玩意重启机器就没了
#   5 构建后端 + 重启 沿用现网的 server-build.sh / server-restart.sh
#   6 换前端产物      包里带的是构建好的 dist，目标机不需要 Node
#   7 导入业务字典    幂等；少了它问数会明显变笨（现网曾少 90 条样例）
#   8 上线验收        逐表对账 + 服务托管形态 —— /api/health 答不了这些
set -euo pipefail

PKG="$(cd "$(dirname "$0")" && pwd -P)"
ROOT="${DMS_RUNTIME_ROOT:-/opt/dms-ai}"
ROOT="${ROOT%/}"
STAMP="$(date -u '+%Y%m%d-%H%M%S')"
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

die()  { echo; echo "❌ $*" >&2; exit 1; }
step() { echo; echo "═══ $*"; }
run()  { if [ "$DRY" = 1 ]; then echo "  [dry-run] $*"; else eval "$@"; fi; }

step "0/8 前置检查"
[ "$(id -u)" = 0 ] || die "请用 root 跑（要动 /opt/dms-ai 与 docker）"
command -v docker >/dev/null || die "缺 docker"
command -v python3 >/dev/null || die "缺 python3"
[ -d "$ROOT" ] || die "$ROOT 不存在。这个包是**就地升级**用的，全新机器请先按 部署说明.txt 建基础环境"
[ -f "$ROOT/settings.docker.json" ] || die "缺 $ROOT/settings.docker.json —— 这不像是已部署过的机器"
for d in source payload config; do
  [ -d "$PKG/$d" ] || die "包不完整：缺 $PKG/$d"
done
echo "  ✅ root / docker $(docker --version | awk '{print $3}' | tr -d ,) / python3 $(python3 -V | awk '{print $2}')"
echo "  ✅ 目标 $ROOT（现有布局：$([ -L "$ROOT/app" ] && echo 'app+releases' || echo '源码平铺')）"
echo "  ✅ 磁盘剩余 $(df -h "$ROOT" | tail -1 | awk '{print $4}')（向量服务镜像约 1.2G）"

step "1/8 备份会被替换的目录 → $ROOT/rollback-before-$STAMP"
# 只备份**将被覆盖**的东西。venv/kbdata/settings 不在替换范围内，也就不必备份
# （kbdata 几百个原件，备份它只会撑爆磁盘，而它根本不会被动）。
BACKUP="$ROOT/rollback-before-$STAMP"
run "mkdir -p '$BACKUP'"
for d in crates docker docs integrations scripts tools web/dist; do
  [ -e "$ROOT/$d" ] || continue
  run "mkdir -p '$BACKUP/$(dirname "$d")' && cp -a '$ROOT/$d' '$BACKUP/$d'"
done
for f in Cargo.toml Cargo.lock rust-toolchain.toml; do
  [ -e "$ROOT/$f" ] || continue
  run "cp -a '$ROOT/$f' '$BACKUP/$f'"
done
echo "  ✅ 出问题就从 $BACKUP 拷回来"

step "2/8 同步源码（settings / .secret_key / kbdata / venv / web/dist 一律不碰）"
# 🔴 这几样是**运行时状态**，不是代码：
#   settings.docker.json / .secret_key —— 现网凭据，覆盖了就要重配
#   kbdata                            —— 知识库原件，库里有记录而原件没了＝永久损坏
#   venv                              —— 宿主机 Python 环境，重建要几分钟且没必要
#   web/dist                          —— 第 6 步用包里的产物单独换，不走这里
for d in crates docker docs integrations scripts tools; do
  [ -d "$PKG/source/$d" ] || continue
  run "rm -rf '$ROOT/$d.new' && cp -a '$PKG/source/$d' '$ROOT/$d.new'"
  run "rm -rf '$ROOT/$d' && mv '$ROOT/$d.new' '$ROOT/$d'"
  echo "  ✅ $d"
done
for f in Cargo.toml Cargo.lock rust-toolchain.toml .dockerignore settings.example.json README.md; do
  [ -f "$PKG/source/$f" ] || continue
  run "cp -a '$PKG/source/$f' '$ROOT/$f'"
done
run "cp -a '$PKG/部署说明.txt' '$ROOT/部署说明.txt' 2>/dev/null || true"

step "3/8 配置与密钥（已存在就保留现网那份，绝不覆盖）"
for pair in "settings.docker.json:$PKG/config/settings.docker.json" ".secret_key:$PKG/config/secret.key"; do
  dst="$ROOT/${pair%%:*}"; src="${pair#*:}"
  if [ -e "$dst" ]; then
    echo "  ⏭️  保留现网 $dst"
  else
    run "install -m 600 '$src' '$dst'" && echo "  ✅ 装入 $dst"
  fi
done

step "4/8 向量·精排·解析服务 → 容器（替掉 nohup 起的裸进程）"
# 现网这套服务是按旧版 部署说明.txt 第 3 步 `nohup venv/bin/python ... &` 起的。
# 那不是谁做错了 —— 说明书就是那么写的。但代价实打实：重启机器服务就没了，
# 部署换了代码它也不跟着变（进程还抱着旧文件），而 /api/health 全绿。
# 容器形态把依赖和代码一起装进镜像，`--restart unless-stopped` 天然开机自启。
if [ "$DRY" = 1 ]; then
  echo "  [dry-run] 跳过（会构建镜像并接管 8078）"
else
  # TAKEOVER=1 是有意的：装这个包的目的就是接管那个裸进程。会中断向量服务数秒。
  DMS_RUNTIME_ROOT="$ROOT" DMS_EMBED_TAKEOVER=1 bash "$ROOT/scripts/embed-install.sh" \
    || die "向量服务安装失败。旧进程可能已被停掉 —— 看上面的报错，修好后重跑本脚本即可"
fi

step "5/8 构建后端镜像 + 重启（5-10 分钟）"
if [ "$DRY" = 1 ]; then
  echo "  [dry-run] 跳过 server-build.sh / server-restart.sh"
else
  ( cd "$ROOT" && bash scripts/server-build.sh ) || die "后端镜像构建失败（上面有 cargo 报错）"
  DMS_RUNTIME_ROOT="$ROOT" DMS_SERVER_HOST=172.17.0.1 bash "$ROOT/scripts/server-restart.sh" \
    || die "后端重启失败 —— server-restart.sh 会自动恢复旧容器，生产不会停在半路"
fi

step "6/8 前端产物（包里已构建好，目标机不需要 Node）"
if [ "$DRY" = 1 ]; then
  echo "  [dry-run] 跳过 web-update.sh"
elif [ -s "$PKG/payload/web-dist.tar.gz" ]; then
  bash "$ROOT/scripts/web-update.sh" "$PKG/payload/web-dist.tar.gz" dms-ai-web \
    || die "前端更新失败（web-update.sh 会自动恢复旧目录）"
else
  echo "  ⏭️  包里没有 web-dist.tar.gz"
fi

step "7/8 导入业务字典种子（幂等；问数准确性的一半靠它）"
# 🔴 现网实测过它缺席的代价：SQL 样例 173 行（应 263）、教训 50 条（应 98），
# 「本月销售额按省份的分布」这类问句直接答不可计算，而老服务器答得出来。
if [ "$DRY" = 1 ]; then
  echo "  [dry-run] 跳过快照导入"
elif [ -s "$PKG/payload/registry_snapshot.json" ]; then
  if [ -x "$ROOT/venv/bin/python3" ]; then
    ( cd "$ROOT" && DMS_SECRET_KEY="$(cat "$ROOT/.secret_key")" \
        DMSAI_SETTINGS="$ROOT/settings.docker.json" \
        venv/bin/python3 tools/registry_snapshot.py import "$PKG/payload/registry_snapshot.json" ) \
      || die "快照导入失败"
  else
    die "没有 $ROOT/venv/bin/python3，导不了快照。建它：python3 -m venv $ROOT/venv && $ROOT/venv/bin/pip install -r $ROOT/tools/requirements-embed.txt"
  fi
else
  echo "  ⏭️  包里没有 registry_snapshot.json（问数会明显低于现网水平）"
fi

step "8/8 上线验收（/api/health 答不了的那几件事）"
if [ "$DRY" = 1 ]; then
  echo "  [dry-run] 跳过验收"
else
  cp -f "$PKG/payload/registry_snapshot.json" "$ROOT/registry_snapshot.json" 2>/dev/null || true
  DMS_RUNTIME_ROOT="$ROOT" DMS_SNAPSHOT="$ROOT/registry_snapshot.json" \
    bash "$ROOT/scripts/server-verify.sh" || die "验收未通过（见上面带 ❌ 的行）。全部幂等，修完重跑本脚本"
fi

echo
echo "═══ 装完了"
echo "回滚材料：$BACKUP"
echo "人工冒烟三题（浏览器打开站点问）："
echo "  1. 本月销售额          → 应走确定性问数，带口径收据"
echo "  2. 销售额按省份的分布   → 应出 20+ 行省份数据（这题此前答『不可计算』）"
echo "  3. 市场费用的报销政策是什么 → 应走知识库，带引用"
