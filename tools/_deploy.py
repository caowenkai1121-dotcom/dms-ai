# -*- coding: utf-8 -*-
"""部署助手：DEPLOY_PW 环境变量提供密码（不落盘、不进日志）。
用法：python tools/_deploy.py run "远程命令" | python tools/_deploy.py put 本地 远程 | get 远程 本地"""
import os
import sys

import paramiko

HOST = os.environ.get("DEPLOY_HOST", "127.0.0.1")
USER = os.environ.get("DEPLOY_USER", "root")


def client():
    pw = os.environ.get("DEPLOY_PW")
    if not pw:
        sys.exit("DEPLOY_PW 环境变量未设置")
    c = paramiko.SSHClient()
    c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    c.connect(HOST, username=USER, password=pw, port=int(os.environ.get("DEPLOY_PORT", "22")),
              timeout=20, banner_timeout=20)
    return c


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
        # SFTP 子系统坏的服务器兜底：base64 走 exec stdin（64KiB 块流式，不爆内存/命令行）
        import base64
        c = client()
        transport = c.get_transport()
        assert transport is not None
        chan = transport.open_session()
        chan.exec_command(f"base64 -d > {sys.argv[3]}")
        with open(sys.argv[2], "rb") as f:
            while True:
                chunk = f.read(65536)
                if not chunk:
                    break
                chan.sendall(base64.b64encode(chunk) + b"\n")
        chan.shutdown_write()
        rc = chan.recv_exit_status()
        c.close()
        print("bput ok:", sys.argv[3], "rc:", rc)
    elif mode == "get":
        c = client()
        sftp = c.open_sftp()
        sftp.get(sys.argv[2], sys.argv[3])
        sftp.close()
        c.close()
        print("get ok:", sys.argv[3])
