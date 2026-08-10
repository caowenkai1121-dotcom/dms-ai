# CLI 调用口：把 `dms-ai-server <子命令>` 的构造收在一处。
#
# 为什么需要它：本机 Smart App Control 处于强制态，按内容哈希拦所有新链接的未签名产物，
# `target/debug/dms-ai-server.exe` 在 Windows 侧**根本起不来**（os error 4551）。
# 判官/评测/对拍三个工具都靠这个 exe，于是全部瘫掉。
#
# 用法（默认仍是本机 exe，行为与改动前一致）：
#   set DMSAI_CLI=docker exec dms-ai-server /app/dms-ai-server
#   python tools/regression.py
#
# 🔴 **不要写 `docker exec -i`**（本文件会主动剥掉它，见 `_drop_stdin_flag`）。
# `-i` 让 docker 把 stdin 接到容器进程；一旦调用方没有真 stdin（后台任务、CI、
# 被挪到后台的长任务），`docker exec` 就**一直等 stdin 而不报错**。
# 症状是「脚本卡住、CPU 占用 1 秒、一道题都不出」——看起来像慢，不像坏。
# 实测为它丢过两次半小时。子命令只吃 argv，`-i` 从来没被需要过。
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
EXE = ROOT / "target" / "debug" / "dms-ai-server.exe"


def _drop_stdin_flag(base):
    """`docker exec` 前缀里的 `-i` / `--interactive` 一律剥掉（理由见文件头）。

    只在识别出 `docker … exec` 时动手：别的前缀（本机 exe、ssh、包装脚本）原样放过，
    它们的 `-i` 可能是有意义的参数。`-it` 不剥——那个会当场报
    「the input device is not a TTY」，是响亮的失败，不会伪装成卡顿。
    """
    if len(base) >= 2 and Path(base[0]).stem == "docker" and "exec" in base[1:3]:
        return [a for a in base if a not in ("-i", "--interactive")]
    return base


def _ensure_stdin_flag(base):
    """仅给 `docker ... exec` 的 option 段补 `-i`；非 Docker 前缀原样返回。

    只检查 `exec` 与容器名之间的 Docker options，避免把容器内命令自己的
    `python -i` 误认成 `docker exec -i`。已有 `-i` / `--interactive` / `-it`
    均保留，不重复注入；`-t` 仍会按 Docker 自己的规则响亮失败。
    """
    if not base or Path(base[0]).stem.lower() != "docker":
        return base
    try:
        exec_at = base.index("exec", 1)
    except ValueError:
        return base

    value_options = {
        "-e", "--env", "--env-file", "--detach-keys",
        "-u", "--user", "-w", "--workdir",
    }
    i = exec_at + 1
    while i < len(base):
        option = base[i]
        if not option.startswith("-"):
            break  # 首个非 option 是容器名；后面属于容器内命令
        if option in ("-i", "--interactive"):
            return base
        if option.startswith("-") and not option.startswith("--") and "i" in option[1:]:
            return base
        if option in value_options:
            i += 2
        else:
            i += 1
    return base[:exec_at + 1] + ["-i"] + base[exec_at + 1:]


def _ts(t):
    return datetime.datetime.fromtimestamp(t).strftime("%m-%d %H:%M")


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
    """
    exe = EXE if exe is None else Path(exe)
    if not exe.exists():
        return None  # 不存在 → 调用方按「依赖缺席」处理，不是本函数的事
    root = ROOT / "crates" if crates is None else Path(crates)
    newest = max((f.stat().st_mtime for f in root.rglob("*.rs")), default=0)
    if newest <= exe.stat().st_mtime:
        return None
    return (
        "本机 exe 比源码旧（exe {} < 源码 {}）——\n"
        "  {}\n"
        "  Smart App Control 拦着它重新链接，所以它会**照旧跑出旧行为**，测出来的数字是假的。\n"
        "  改走容器里的新二进制：\n"
        '    $env:DMSAI_CLI="docker exec dms-ai-server /app/dms-ai-server"'
    ).format(_ts(exe.stat().st_mtime), _ts(newest), exe)


def cli(*args):
    """返回可直接喂给 subprocess 的 argv。回落本机 exe 时先验它不是过期货。"""
    pre = os.environ.get("DMSAI_CLI", "").strip()
    if not pre:
        why = stale_exe()
        if why:
            raise SystemExit("❌ " + why)
    base = shlex.split(pre) if pre else [str(EXE)]
    return _drop_stdin_flag(base) + [str(a) for a in args]


def cli_stdin(*args):
    """构造需要持续 stdin 的 CLI argv；当前用于 `evaluation.py eval-batch`。

    本机 exe、ssh、包装脚本等非 Docker 前缀完全不改；只有 Docker exec 会确保
    interactive stdin 已开启。不要拿它替代普通 `cli()`。
    """
    pre = os.environ.get("DMSAI_CLI", "").strip()
    if not pre:
        why = stale_exe()
        if why:
            raise SystemExit("❌ " + why)
    base = shlex.split(pre) if pre else [str(EXE)]
    return _ensure_stdin_flag(base) + [str(a) for a in args]


def available():
    """CLI 是否可用（缺席时调用方按「依赖缺席跳过」处理，不记失败）。"""
    return bool(os.environ.get("DMSAI_CLI", "").strip()) or EXE.exists()


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
    # 真仓上的实测：本机 exe 若存在，它此刻**就是**过期的（SAC 拦着链接）
    if EXE.exists():
        assert stale_exe(), "本机 exe 存在且不过期？那 SAC 松了，确认一下再删这条"
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
    # DMSAI_CLI 没设 + exe 过期 → 硬失败（本次吞掉一整跑的那条路）
    os.environ.pop("DMSAI_CLI")
    if EXE.exists():
        try:
            cli("ask", "admin", "x")
            raise AssertionError("exe 过期时 cli() 必须硬失败，不许静默回落")
        except SystemExit as e:
            assert "比源码旧" in str(e), e
    print("cli.py 自检通过")
