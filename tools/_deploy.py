# -*- coding: utf-8 -*-
"""部署助手：DEPLOY_PW 环境变量提供密码（不落盘、不进日志）。
用法：python tools/_deploy.py run "远程命令" | python tools/_deploy.py put 本地 远程 | get 远程 本地"""
import os
import time
import hashlib
import shlex
import sys

import paramiko

HOST = os.environ.get("DEPLOY_HOST", "127.0.0.1")
USER = os.environ.get("DEPLOY_USER", "root")


def client():
    pw = os.environ.get("DEPLOY_PW")
    if not pw:
        sys.exit("DEPLOY_PW 环境变量未设置")
    c = paramiko.SSHClient()
    known_hosts = os.environ.get("DEPLOY_KNOWN_HOSTS")
    if known_hosts:
        c.load_host_keys(known_hosts)
    else:
        c.load_system_host_keys()
    # 部署会传配置、密钥关联产物并执行远程命令，未知主机不得自动信任。
    c.set_missing_host_key_policy(paramiko.RejectPolicy())
    # 🔴 连接必须能重试（2026-08-14 实测部署死在这里）：构建轮询每 15 秒开一条**新**连接，
    # 一次构建最多 120 条 —— 打满 sshd 的 MaxStartups（默认 10:30:100）后新连接被直接拒，
    # 表现是 `SSHException: No existing session` / `Socket is closed`，
    # 而 deploy_update.sh 开着 `set -e`，一次失败整个部署就退出，**镜像已经建好却没切**。
    # 退避重试放在这里而不是各调用点：run/bput/轮询共十几处，一处一份必漂。
    port = int(os.environ.get("DEPLOY_PORT", "22"))
    last = None
    for backoff in (0, 5, 15, 30):
        if backoff:
            time.sleep(backoff)
        try:
            c.connect(HOST, username=USER, password=pw, port=port,
                      timeout=20, banner_timeout=20)
            return c
        except Exception as e:  # 连接层的异常族很杂（socket/paramiko/EOF），一律退避重试
            last = e
    raise SystemExit(f"SSH 连接失败（已退避重试 4 次）：{last}")


def run(cmd, timeout=300):
    c = client()
    chan = c.get_transport().open_session()
    chan.settimeout(5.0)
    chan.exec_command(cmd)
    out, err = b"", b""
    import time
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if chan.recv_ready():
                out += chan.recv(65536)
            if chan.recv_stderr_ready():
                err += chan.recv_stderr(65536)
            if chan.exit_status_ready():
                # 排干剩余
                while chan.recv_ready():
                    out += chan.recv(65536)
                while chan.recv_stderr_ready():
                    err += chan.recv_stderr(65536)
                break
        except Exception:
            pass
        time.sleep(0.2)
    rc = chan.recv_exit_status() if chan.exit_status_ready() else -1
    c.close()
    return rc, out.decode("utf-8", "replace"), err.decode("utf-8", "replace")


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "run":
        rc, so, se = run(sys.argv[2], timeout=int(sys.argv[3]) if len(sys.argv) > 3 else 300)
        sys.stdout.write(so)
        if se.strip():
            sys.stderr.write(se[-2000:])
        sys.exit(rc)
    if mode == "put":
        c = client()
        sftp = c.open_sftp()
        sftp.put(sys.argv[2], sys.argv[3])
        sftp.close()
        c.close()
        print("put ok:", sys.argv[3])
    elif mode == "bput":
        # SFTP 子系统坏的服务器兜底：base64 走 exec stdin。先写 .part，校验 sha256 后原子替换；
        # 远端写盘/解码/校验任一步失败都必须把非零退出码传给部署脚本。
        import base64
        local_path, remote_path = sys.argv[2], sys.argv[3]
        digest = hashlib.sha256()
        with open(local_path, "rb") as f:
            while chunk := f.read(1024 * 1024):
                digest.update(chunk)
        expected = digest.hexdigest()
        part_path = f"{remote_path}.part-{os.getpid()}"
        q_part = shlex.quote(part_path)
        q_remote = shlex.quote(remote_path)
        q_expected = shlex.quote(expected)
        c = client()
        transport = c.get_transport()
        assert transport is not None
        chan = transport.open_session()
        chan.exec_command(
            f"set -e; umask 077; part={q_part}; remote={q_remote}; expected={q_expected}; "
            "trap 'rm -f -- \"$part\"' EXIT; "
            "base64 -d > \"$part\"; "
            "actual=$(sha256sum \"$part\" | awk '{print $1}'); "
            "test \"$actual\" = \"$expected\"; "
            "mv -f -- \"$part\" \"$remote\"; trap - EXIT"
        )
        with open(local_path, "rb") as f:
            while True:
                chunk = f.read(65536)
                if not chunk:
                    break
                chan.sendall(base64.b64encode(chunk) + b"\n")
        chan.shutdown_write()
        rc = chan.recv_exit_status()
        stderr = b""
        while chan.recv_stderr_ready():
            stderr += chan.recv_stderr(65536)
        c.close()
        if rc != 0:
            if stderr:
                sys.stderr.write(stderr.decode("utf-8", "replace")[-2000:])
            sys.exit(rc)
        print("bput ok:", remote_path, "sha256:", expected)
    elif mode == "get":
        c = client()
        sftp = c.open_sftp()
        sftp.get(sys.argv[2], sys.argv[3])
        sftp.close()
        c.close()
        print("get ok:", sys.argv[3])
