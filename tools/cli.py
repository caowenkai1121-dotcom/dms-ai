# CLI 调用口：把 `dms-ai-server <子命令>` 的构造收在一处。
#
# 为什么需要它：本机 Smart App Control 处于强制态，按内容哈希拦所有新链接的未签名产物，
# `target/debug/dms-ai-server.exe` 在 Windows 侧**根本起不来**（os error 4551）。
# 判官/评测/对拍三个工具都靠这个 exe，于是全部瘫掉。
#
# 用法（默认仍是本机 exe，行为与改动前一致）：
#   cmd:        set DMSAI_CLI=docker exec dms-ai-server /app/dms-ai-server
#   PowerShell: $env:DMSAI_CLI="docker exec dms-ai-server /app/dms-ai-server"
#   python tools/regression.py
# 注意：DMSAI_CLI 里的 Windows 路径要写正斜杠或加引号——shlex posix 模式会吃掉反斜杠
# （`C:\tools\wrap.exe` 会被拼成 `C:toolswrap.exe`）。
#
# 🔴 **不要写 `docker exec -i`**（本文件会主动剥掉它，见 `_drop_stdin_flag`）。
# `-i` 让 docker 把 stdin 接到容器进程；一旦调用方没有真 stdin（后台任务、CI、
# 被挪到后台的长任务），`docker exec` 就**一直等 stdin 而不报错**。
# 症状是「脚本卡住、CPU 占用 1 秒、一道题都不出」——看起来像慢，不像坏。
# 实测为它丢过两次半小时。一次性子命令只吃 argv，`-i` 从来没被需要过。
#
# 注意：走 docker exec 每次约多 0.3s 进程开销，evaluation.py 的 p50/p95 因此
# **不可与 Windows 侧基线直接对比**（延迟基线要么全走容器，要么全走本机）。
# `eval-batch` 是唯一例外：它通过 stdin 持续收 NDJSON，必须用 `cli_stdin()`；普通
# `cli()` 仍剥掉 `-i`，避免一次性子命令在后台/CI 等待不存在的 stdin。
import datetime
import os
import shlex
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# 只看 debug 目录是刻意的：本机开发默认 `cargo build` 出 debug；
# release 机器上 available()=False、调用方按「依赖缺席」处理，不会静默用错二进制。
EXE = ROOT / "target" / "debug" / "dms-ai-server.exe"

# `docker exec` 与容器名之间带值的 option（值是下一个 token，扫描时要成对跳过）
_EXEC_VALUE_OPTIONS = {
    "-e", "--env", "--env-file", "--detach-keys",
    "-u", "--user", "-w", "--workdir",
}

# docker 全局 option 中带值的一批（出现在子命令之前，如 `docker --context prod exec …`）
_DOCKER_GLOBAL_VALUE_OPTIONS = {
    "--context", "--config", "-H", "--host", "--log-level",
    "--tlscacert", "--tlscert", "--tlskey",
}


def _docker_exec_at(base):
    """返回 `docker … exec` 前缀里 exec 子命令的下标；不是该形态返回 `None`。

    逐个跳过 docker 全局 option（`--context prod` 这类带值的也跳），首个非 option
    token 必须是 `exec`——镜像名/参数里的字面量 exec（如 `docker run exec …`）不许误判。
    `Path.stem` 统一 `.lower()`，剥旗/补旗两侧对 `Docker`/`DOCKER` 口径一致。
    """
    if len(base) < 2 or Path(base[0]).stem.lower() != "docker":
        return None
    i = 1
    while i < len(base):
        token = base[i]
        if token in _DOCKER_GLOBAL_VALUE_OPTIONS:
            i += 2
        elif token.startswith("-"):
            i += 1
        else:
            return i if token == "exec" else None
    return None


def _drop_stdin_flag(base):
    """`docker exec` 前缀里的 `-i` / `--interactive` 一律剥掉（理由见文件头）。

    只在识别出 `docker … exec` 时动手：别的前缀（本机 exe、ssh、包装脚本）原样放过，
    它们的 `-i` 可能是有意义的参数。`-it` 不剥——那个会当场报
    「the input device is not a TTY」，是响亮的失败，不会伪装成卡顿。
    只剥 exec 与容器名之间的 option 段：容器内命令自己的 `-i`（如 `python -i`）不许动。
    """
    exec_at = _docker_exec_at(base)
    if exec_at is None:
        return base
    out = base[: exec_at + 1]
    i = exec_at + 1
    while i < len(base):
        option = base[i]
        if not option.startswith("-"):
            break  # 首个非 option 是容器名；后面属于容器内命令
        if option in ("-i", "--interactive"):
            i += 1  # 剥掉，不进 out
            continue
        out.append(option)
        if option in _EXEC_VALUE_OPTIONS and i + 1 < len(base):
            out.append(base[i + 1])  # 带值 option 的值要一并保留、一并跳过
            i += 2
        else:
            i += 1
    return out + base[i:]


def _ensure_stdin_flag(base):
    """仅给 `docker ... exec` 的 option 段补 `-i`；非 Docker 前缀原样返回。

    只检查 `exec` 与容器名之间的 Docker options，避免把容器内命令自己的
    `python -i` 误认成 `docker exec -i`。已有 `-i` / `--interactive` / `-it`
    均保留，不重复注入；`-t` 仍会按 Docker 自己的规则响亮失败。
    """
    exec_at = _docker_exec_at(base)
    if exec_at is None:
        return base

    i = exec_at + 1
    while i < len(base):
        option = base[i]
        if not option.startswith("-"):
            break  # 首个非 option 是容器名；后面属于容器内命令
        if option in ("-i", "--interactive"):
            return base
        if option.startswith("-") and not option.startswith("--") and "i" in option[1:]:
            return base
        if option in _EXEC_VALUE_OPTIONS:
            i += 2
        else:
            i += 1
    return base[:exec_at + 1] + ["-i"] + base[exec_at + 1:]


def _ts(t):
    return datetime.datetime.fromtimestamp(t).strftime("%m-%d %H:%M")


def _newest_rs_mtime(root):
    """crates 树下最新 .rs 的 mtime；`target/` 是构建产物目录，不钻进去扫。"""
    newest = 0
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d != "target"]
        for name in filenames:
            if name.endswith(".rs"):
                mtime = os.path.getmtime(os.path.join(dirpath, name))
                if mtime > newest:
                    newest = mtime
    return newest


_STALE_CACHE_UNSET = object()
_stale_cache = _STALE_CACHE_UNSET  # 默认参数（本机 exe）结果的模块级缓存


def stale_exe(exe=None, crates=None):
    """本机 exe 比源码旧 → 返回说明，否则 `None`。

    🔴 由来（实测吞掉一整跑）：`DMSAI_CLI` 没设时 `cli()` 静默回落本机
    `target/debug/dms-ai-server.exe`。Smart App Control 强制态下那个 exe **不会被重新链接**
    （`os error 4551`），于是它停在最后一次链接成功的那天 —— 而它**照样能跑**。
    一次回归因此跑出「47 通过 / 9 失败」，9 个失败全是两天前的旧行为；
    金文件 diff 看起来像真回归（`route=direct-agg`、耗时 245ms，一切正常），
    唯一的破绽是同一个问句走 `docker exec` 出的是正确 SQL。
    「能跑但是旧的」比起不来坏得多：起不来是响亮失败，这个是**假数字**。

    所以过期即硬失败，不是 warn —— warn 会被 `Select-Object -Last N` 截掉，正是这次的情形。

    默认参数（本机 exe + crates/）的结果做模块级缓存：一轮回归 55+ 次调用，
    不必每次都全树扫一遍 .rs；显式传参的调用（自检）不走缓存。
    """
    global _stale_cache
    default_args = exe is None and crates is None
    if default_args and _stale_cache is not _STALE_CACHE_UNSET:
        return _stale_cache
    exe = EXE if exe is None else Path(exe)
    result = None
    if exe.exists():  # 不存在 → 调用方按「依赖缺席」处理，不是本函数的事
        root = ROOT / "crates" if crates is None else Path(crates)
        newest = _newest_rs_mtime(root)
        exe_mtime = exe.stat().st_mtime
        if newest > exe_mtime:
            result = (
                "本机 exe 比源码旧（exe {} < 源码 {}）——\n"
                "  {}\n"
                "  Smart App Control 拦着它重新链接，所以它会**照旧跑出旧行为**，测出来的数字是假的。\n"
                "  改走容器里的新二进制：\n"
                '    $env:DMSAI_CLI="docker exec dms-ai-server /app/dms-ai-server"'
            ).format(_ts(exe_mtime), _ts(newest), exe)
    if default_args:
        _stale_cache = result
    return result


def _base_argv():
    """读 `DMSAI_CLI` 前缀（未设时先验本机 exe 不过期），返回 shlex 切好的 base。"""
    pre = os.environ.get("DMSAI_CLI", "").strip()
    if not pre:
        why = stale_exe()
        if why:
            raise SystemExit("❌ " + why)
    return shlex.split(pre) if pre else [str(EXE)]


def cli(*args):
    """返回可直接喂给 subprocess 的 argv。回落本机 exe 时先验它不是过期货。"""
    return _drop_stdin_flag(_base_argv()) + [str(a) for a in args]


def cli_stdin(*args):
    """构造需要持续 stdin 的 CLI argv；当前用于 `evaluation.py eval-batch`。

    本机 exe、ssh、包装脚本等非 Docker 前缀完全不改；只有 Docker exec 会确保
    interactive stdin 已开启。不要拿它替代普通 `cli()`。
    """
    return _ensure_stdin_flag(_base_argv()) + [str(a) for a in args]


def available():
    """CLI 是否可用（缺席时调用方按「依赖缺席跳过」处理，不记失败）。

    与 `cli()` 的口径同步：本机 exe 存在但已过期时返回 False——否则调用方按
    available()=True 走下去，会被 `cli()` 的过期硬失败炸掉整个进程。
    """
    if os.environ.get("DMSAI_CLI", "").strip():
        return True
    return EXE.exists() and stale_exe() is None


if __name__ == "__main__":  # python tools/cli.py —— 自检
    assert _drop_stdin_flag(["docker", "exec", "-i", "c", "/app/x"]) == ["docker", "exec", "c", "/app/x"]
    assert _drop_stdin_flag(["docker", "exec", "--interactive", "c"]) == ["docker", "exec", "c"]
    # 无 -i：原样
    assert _drop_stdin_flag(["docker", "exec", "c"]) == ["docker", "exec", "c"]
    # 非 docker 前缀：`-i` 可能有意义，不许动
    assert _drop_stdin_flag(["ssh", "-i", "key", "host"]) == ["ssh", "-i", "key", "host"]
    assert _drop_stdin_flag(["/x/dms-ai-server.exe"]) == ["/x/dms-ai-server.exe"]
    # `-it` 不剥（响亮失败优于伪装成卡顿）
    assert _drop_stdin_flag(["docker", "exec", "-it", "c"]) == ["docker", "exec", "-it", "c"]
    # 大小写不敏感：`Docker` 同样剥（与 _ensure_stdin_flag 口径一致）
    assert _drop_stdin_flag(["Docker", "exec", "-i", "c", "/app/x"]) == ["Docker", "exec", "c", "/app/x"]
    # exec 在全局 option 之后（`--context prod` 带值）也要剥得到
    assert _drop_stdin_flag(["docker", "--context", "prod", "exec", "-i", "c"]) == [
        "docker", "--context", "prod", "exec", "c",
    ]
    # 只剥 option 段：容器内命令自己的 `-i` 不许动
    assert _drop_stdin_flag(["docker", "exec", "-i", "c", "python", "-i"]) == [
        "docker", "exec", "c", "python", "-i",
    ]
    # 带值 option（-u root）不误把值当容器名
    assert _drop_stdin_flag(["docker", "exec", "-u", "root", "-i", "c"]) == [
        "docker", "exec", "-u", "root", "c",
    ]
    # 字面量 exec 不在子命令位（如 `docker run exec …`）：不许误判剥旗
    assert _drop_stdin_flag(["docker", "run", "exec", "-i", "img"]) == ["docker", "run", "exec", "-i", "img"]
    # 长驻 stdin：仅 Docker exec 补/留 interactive；容器内命令的 `-i` 不能误判。
    assert _ensure_stdin_flag(["docker", "exec", "c", "/app/x"]) == [
        "docker", "exec", "-i", "c", "/app/x",
    ]
    assert _ensure_stdin_flag(["docker", "exec", "-i", "c"]) == ["docker", "exec", "-i", "c"]
    assert _ensure_stdin_flag(["docker", "exec", "--interactive", "c"]) == [
        "docker", "exec", "--interactive", "c",
    ]
    assert _ensure_stdin_flag(["docker", "exec", "-it", "c"]) == ["docker", "exec", "-it", "c"]
    assert _ensure_stdin_flag(["docker", "--context", "prod", "exec", "c", "python", "-i"]) == [
        "docker", "--context", "prod", "exec", "-i", "c", "python", "-i",
    ]
    assert _ensure_stdin_flag(["ssh", "-i", "key", "host"]) == ["ssh", "-i", "key", "host"]
    # 字面量 exec 不在子命令位：不许误判插旗
    assert _ensure_stdin_flag(["docker", "run", "exec", "img"]) == ["docker", "run", "exec", "img"]
    # 过期判据：造一个「exe 比 .rs 旧」的目录树，必须报；反过来必须不报
    import tempfile

    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        (d / "c").mkdir()
        old, new = d / "x.exe", d / "c" / "a.rs"
        old.write_text("x")
        new.write_text("y")
        os.utime(old, (1, 1))  # exe 停在 1970
        assert stale_exe(old, d / "c"), "exe 比源码旧却没报"
        os.utime(new, (0, 0))  # 源码更旧 → 不许报
        assert stale_exe(old, d / "c") is None, "exe 比源码新却报了"
        assert stale_exe(d / "nope.exe", d / "c") is None, "exe 不存在时不该判过期"
    # 真仓上的水位计：本机 exe 若存在，此刻它**通常**是过期的（SAC 拦着链接）。
    # 这是环境耦合的观察项，哪天 SAC 松了、exe 真新了，只提示不算错。
    if EXE.exists() and not stale_exe():
        print("⚠️ 本机 exe 存在且不过期：SAC 可能松了，确认一下这条水位计是否还有效")
    os.environ["DMSAI_CLI"] = "docker exec dms-ai-server /app/dms-ai-server"
    assert cli_stdin("eval-batch") == [
        "docker", "exec", "-i", "dms-ai-server", "/app/dms-ai-server", "eval-batch",
    ]
    os.environ["DMSAI_CLI"] = "docker exec -i dms-ai-server /app/dms-ai-server"
    assert cli_stdin("eval-batch") == [
        "docker", "exec", "-i", "dms-ai-server", "/app/dms-ai-server", "eval-batch",
    ]
    assert cli("ask", "admin", "问句") == [
        "docker", "exec", "dms-ai-server", "/app/dms-ai-server", "ask", "admin", "问句",
    ]
    # DMSAI_CLI 已设时 available() 不看本机 exe
    assert available() is True
    # DMSAI_CLI 没设 + exe 过期 → 硬失败（本次吞掉一整跑的那条路）
    os.environ.pop("DMSAI_CLI")
    if EXE.exists():
        try:
            cli("ask", "admin", "x")
            raise AssertionError("exe 过期时 cli() 必须硬失败，不许静默回落")
        except SystemExit as e:
            assert "比源码旧" in str(e), e
    print("cli.py 自检通过，覆盖：")
    print("  - 剥 -i / --interactive（含 Docker 大写、--context 全局 option、-it 保留）")
    print("  - 只剥 exec option 段：容器内 `python -i` 不误判、带值 option（-u root）成对跳")
    print("  - 字面量 exec 不在子命令位（docker run exec …）剥/补两侧都不误判")
    print("  - 补 -i（eval-batch 长驻 stdin；已有 -i/--interactive/-it 不重复注入）")
    print("  - 过期判据（exe 旧报、源码旧不报、exe 缺席不报）与 cli() 过期硬失败")
    print("  - available() 与 cli() 口径同步（含 DMSAI_CLI 短路）")
