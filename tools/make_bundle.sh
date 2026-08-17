#!/usr/bin/env bash
# 打一个**可独立使用**的部署包：解压即可一键部署，含真实配置（DSN + 大模型 key）。
#
# 为什么要有这个脚本：2026-08-12 那次部署包是手工拼的 —— `git ls-files` 打出来的 src.tar.gz
# 里没有凭据（.gitignore:8 把 settings*.json 通配掉了），有人把 settings.docker.json 与
# .secret_key 手工贴到包根上。手工步骤不会自己重复，也不会自己校验，于是包一旦过期就没人知道。
# 这个脚本把「什么进包」收口成一份可重跑、可反向验证的定义。
#
# 用法：
#   DMS_BUNDLE_CONFIG=/path/settings.docker.json DMS_BUNDLE_SECRET=/path/.secret_key \
#   [DMS_BUNDLE_SNAPSHOT=/path/registry_snapshot.json] \
#   [DMS_BUNDLE_REQUIREMENTS=/path/freeze.txt] \
#   [DMS_BUNDLE_SKIP_WEB_BUILD=1] \
#   bash tools/make_bundle.sh <输出目录>
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

die() { echo "ERROR: $*" >&2; exit 1; }

# 🔴 跨 bash→python 边界的路径必须是**原生绝对路径**：Windows 版 Python 不认 msys 的
# `/c/Users/...`，而 tools/settings.py 会把相对路径拼到仓库根上（settings.py:137）——
# 两条规则叠加，只有原生绝对路径这一种写法在两个平台上都成立。
native() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi; }

# Windows 上 `python3` 常常是 Microsoft Store 的空壳（一跑就打广告并非零退出），
# 所以按「真能跑起来的 3.8+」挑，而不是认名字。服务器侧脚本不受影响，那边 python3 是真的。
PY=""
for cand in python3 python py; do
  command -v "$cand" >/dev/null 2>&1 || continue
  "$cand" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 8) else 1)' 2>/dev/null || continue
  PY="$cand"; break
done
[ -n "$PY" ] || die "没找到可用的 Python 3.8+"

OUT="${1:?用法：bash tools/make_bundle.sh <输出目录>}"
CONFIG="${DMS_BUNDLE_CONFIG:?DMS_BUNDLE_CONFIG 未设置（真实 settings.docker.json 的路径）}"
SECRET="${DMS_BUNDLE_SECRET:?DMS_BUNDLE_SECRET 未设置（与上面配套的 .secret_key 路径）}"
SNAPSHOT="${DMS_BUNDLE_SNAPSHOT:-}"
REQUIREMENTS="${DMS_BUNDLE_REQUIREMENTS:-}"

[ -s "$CONFIG" ] || die "配置文件不存在或为空：$CONFIG"
[ -s "$SECRET" ] || die "密钥文件不存在或为空：$SECRET"
[ "$(wc -c < "$SECRET")" -ge 32 ] || die "密钥不足 32 字节：$SECRET"

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd -P)"
# 🔴 成品包里有明文可用的凭据（密文 + 配套主钥 = 明文等价）。绝不允许落在仓库工作区内：
# `.secret_key` 这个名字**不被 .gitignore 命中**（:24 的 `*.key` 只吃 `.key` 后缀，`_key` 不匹配），
# 一次 `git add .` 就进历史，而 deploy_update 的默认打包口径还会主动把它收进每个 release。
case "$OUT/" in
  "$REPO"/*) die "输出目录不能在仓库工作区内（凭据会被 git 收进历史）：$OUT" ;;
esac

echo "== 1/7 本机门禁（部署契约 + 全部 shell 脚本 LF/语法）"
bash scripts/test-deploy-contract.sh

echo "== 2/7 前端产物"
if [ "${DMS_BUNDLE_SKIP_WEB_BUILD:-0}" = 1 ]; then
  [ -s web/dist/index.html ] || die "--skip-web-build 但 web/dist 里没有 index.html"
  echo "SKIP 构建，沿用现有 web/dist（$(date -r web/dist/index.html '+%F %T')）"
else
  ( cd web && npm ci && npm test && npm run build )
fi

echo "== 3/7 源码树 source/（口径与 deploy_update 一致：受控文件 + 未忽略的新文件）"
rm -rf "$OUT/source" "$OUT/payload" "$OUT/config"
mkdir -p "$OUT/source" "$OUT/payload" "$OUT/config"
COUNT=0
while IFS= read -r -d '' path; do
  [ -e "$path" ] || [ -L "$path" ] || continue
  mkdir -p "$OUT/source/$(dirname "$path")"
  cp -p "$path" "$OUT/source/$path"
  COUNT=$((COUNT + 1))
done < <(git ls-files -co --exclude-standard -z)
echo "OK  $COUNT 个文件"
# 包会被复制/同步/解压很多次，任何一次行尾规范化都会让服务器侧 `bash` 报 `$'\r': command not found`。
# 这里对**成品**再查一遍（仓库里那份已经在 1/7 查过）。
while IFS= read -r script; do
  LC_ALL=C grep -Uq $'\r' "$script" && die "成品包里出现 CRLF 脚本：$script"
done < <(find "$OUT/source" -name '*.sh')

echo "== 4/7 payload/"
tar -czf "$OUT/payload/web-dist.tar.gz" -C web/dist .
[ -n "$SNAPSHOT" ] && [ -s "$SNAPSHOT" ] && cp -p "$SNAPSHOT" "$OUT/payload/registry_snapshot.json" \
  && echo "OK  registry_snapshot.json（$(du -h "$SNAPSHOT" | cut -f1)）"
[ -n "$REQUIREMENTS" ] && [ -s "$REQUIREMENTS" ] && cp -p "$REQUIREMENTS" "$OUT/payload/requirements-embed.lock.txt" \
  && echo "OK  requirements-embed.lock.txt"
echo "OK  web-dist.tar.gz（$(du -h "$OUT/payload/web-dist.tar.gz" | cut -f1)）"

echo "== 5/7 config/（真实凭据；密文 + 配套主钥）"
install -m 600 "$CONFIG" "$OUT/config/settings.docker.json"
install -m 600 "$SECRET" "$OUT/config/secret.key"
# 判据：这两个必须**成对**才算数。不成对的包看起来一切正常，直到部署最后一步才报解密失败。
DMS_SECRET_KEY="$(cat "$SECRET")" DMSAI_SETTINGS="$(native "$OUT/config/settings.docker.json")" \
  "$PY" - "$(native "$REPO")" <<'PY' || die "配置与密钥不成对：密文解不开，这个包不可用"
import sys, os
sys.path.insert(0, os.path.join(sys.argv[1], "tools"))
import settings as st
cfg = st.load()
still = [k for k in ("pg_url", "pg_ro_url", "mysql_url", "llm_api_key") if str(cfg.get(k, "")).startswith("enc:v1:")]
if still:
    raise SystemExit(f"仍是密文：{still}")
missing = [k for k in ("pg_url", "mysql_url", "service_url", "kb_root", "listen") if not cfg.get(k)]
if missing:
    raise SystemExit(f"必填键缺失：{missing}")
if cfg.get("kb_root") != "/kbdata":
    raise SystemExit(f'容器部署要求 kb_root=/kbdata，当前 {cfg.get("kb_root")!r}')
if not str(cfg.get("listen", "")).startswith(("0.0.0.0:", "[::]:")):
    raise SystemExit(f'容器部署要求 listen 绑 0.0.0.0，当前 {cfg.get("listen")!r}')
PY
echo "OK  凭据可解密，必填键齐全，kb_root/listen 符合容器部署要求"

echo "== 6/7 安装器与说明（服务器侧就地安装的形态）"
# 入口是**服务器上**跑的 `安装.sh`，不是 Windows 侧的客户端脚本 ——
# 运维的工作方式是「传个 tar 上去解开、在服务器上跑」，包要长成使用者的形状。
cp -p tools/bundle-install-inplace.sh "$OUT/安装.sh"
chmod +x "$OUT/安装.sh"
cp -p tools/bundle-README-inplace.txt "$OUT/部署说明.txt"
echo "OK  安装.sh + 部署说明.txt"

echo "== 7/8 清单"
"$PY" - "$OUT" "$REPO" <<'PY'
import hashlib, json, pathlib, subprocess, sys, datetime

out, repo = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])


def sh(*a):
    # 必须显式 utf-8：commit subject 是中文，而 Windows 上 subprocess 的默认解码走 GBK，
    # 直接 UnicodeDecodeError（打包最后一步炸掉，前面六步的产物全在，看着像成功了一半）。
    out = subprocess.run(a, cwd=repo, capture_output=True,
                         encoding="utf-8", errors="replace").stdout
    return (out or "").strip()


def digest(p):
    h = hashlib.sha256()
    with p.open("rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


# 🔴 只记**本脚本产出的东西**。此前是 `out.rglob("*")` 全收 —— 于是业主自己放在包根下的
# 一个 18.9MB payload.tar（他手工打包传服务器用的）被算进了完整性清单，包"大了一倍"，
# 而清单本该回答的问题是「我发出去的这一份是什么」，不是「这个目录里现在有什么」。
OWNED = ("source", "payload", "config")
FILES_AT_ROOT = ("安装.sh", "部署说明.txt")
files = {}
for top in OWNED:
    for f in sorted((out / top).rglob("*")):
        if f.is_file():
            files[f.relative_to(out).as_posix()] = {"sha256": digest(f), "bytes": f.stat().st_size}
for name in FILES_AT_ROOT:
    f = out / name
    if f.is_file():
        files[name] = {"sha256": digest(f), "bytes": f.stat().st_size}
# 包根下的陌生文件不进清单，但要说出来 —— 它们会跟着一起被打包/同步走。
strays = sorted(
    f.name for f in out.iterdir()
    if f.is_file() and f.name not in FILES_AT_ROOT and f.name != "MANIFEST.json"
)
if strays:
    print("NOTE 包根下有非本脚本产出的文件（不进清单，但同步/打包会带走）：" + ", ".join(strays))

dirty = sh("git", "status", "--porcelain")
manifest = {
    "built_at_utc": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "git": {
        "commit": sh("git", "rev-parse", "HEAD"),
        "subject": sh("git", "log", "-1", "--format=%s"),
        "branch": sh("git", "rev-parse", "--abbrev-ref", "HEAD"),
        "worktree_clean": not dirty,
        "dirty_files": [l[3:] for l in dirty.splitlines()] if dirty else [],
    },
    "components": {
        "embedding": "qwen text-embedding-v4 @1024",
        "rerank": "gte-rerank-v2",
        "runtime_root_default": "/opt/dms-ai",
    },
    "file_count": len(files),
    "total_bytes": sum(v["bytes"] for v in files.values()),
    "files": files,
}
(out / "MANIFEST.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
print(f'OK  MANIFEST.json：{len(files)} 个文件 / {manifest["total_bytes"] / 1048576:.1f} MB'
      f'{"" if manifest["git"]["worktree_clean"] else "  ⚠️ 工作区不干净，已记进清单"}')
PY

echo "== 8/8 成品自检 + 打包"
# 包里所有 .sh 都要能过语法检查：装到一半才发现语法错，生产已经动过了。
while IFS= read -r sh; do
  bash -n "$sh" || die "成品包里有语法错的脚本：$sh"
  LC_ALL=C grep -Uq $'\r' "$sh" && die "成品包里出现 CRLF 脚本：$sh"
done < <(find "$OUT" -name '*.sh')
# 安装器 --dry-run 只在服务器上跑得动（要 root/docker），本机只验语法。
echo "OK  $(find "$OUT" -name '*.sh' | wc -l) 个脚本语法通过、全 LF"

# 打成一个 tar：运维传一个文件就够了。tar 里带顶层目录，解开不会撒一地。
TARBALL="$(dirname "$OUT")/$(basename "$OUT").tar.gz"
rm -f "$TARBALL"
tar -czf "$TARBALL" -C "$(dirname "$OUT")" "$(basename "$OUT")"
echo "OK  $TARBALL（$(du -h "$TARBALL" | cut -f1)）"
echo
echo "部署包已生成："
echo "  目录 $OUT"
echo "  压缩 $TARBALL   ← 传这一个文件上服务器"
