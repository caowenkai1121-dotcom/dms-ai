#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""容器内解析服务 = `tools/embed_service.py` 的**运输壳**，不是第二份解析实现。

解析/分块/能力上报的真相源全在挂载进来的 `tools/embed_service.py`（`/app/tools`，运行时挂载，
不进镜像层）。本文件只补它在容器里跑不了的三处，一处一个理由：

  1. **绑 0.0.0.0**：`es.serve` 绑 127.0.0.1，容器里等于谁都连不上 —— `-p` 转发到容器网卡，
     回环监听不接。容器本身就是沙箱边界，绑全零在这里是对的。
  2. **不加载 fastembed**：`es.serve` 开头就 `embedder()`，那会拖进 onnxruntime + 95MB 模型下载。
     embed 在宿主机上是好的（SAC 只拦 lxml 那类编译扩展），没必要在镜像里再养一份 →
     `/embed` 透传 `EMBED_UPSTREAM`，**没配就 503 明说**，不回空向量。
  3. **/health 带上 `container: true`**：同一个 Rust 客户端可能指着宿主机也可能指着容器，
     出问题时第一件要确认的事就是「我打到哪一个了」。

路由与 JSON 逐字沿用 `es.handle_post` / `es.parse_caps`，所以 Rust 侧 `connector/src/doc.rs`
一行不用改（`service_url` 是单一键，见裁决 V1）。

**镜像为什么存在**：宿主机 lxml 的编译扩展被 Smart App Control 拦死
（`ImportError: DLL load failed while importing etree: 应用程序控制策略已阻止此文件`），
实测宿主机 `:8077/health` → `parse_ok.docx=false, pptx=false`，即业主点名的 word/ppt
在这台机器上恒不可用。容器里没有 SAC，同一份代码全绿（证据见 scripts/parser.ps1 probe）。
"""
import json
import os
import sys
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.environ.get('TOOLS_DIR', '/app/tools'))
import embed_service as es  # noqa: E402  （挂载目录必须先进 sys.path）

PORT = int(os.environ.get('PARSER_PORT', '8077'))
# 宿主机 embed 服务（例 `http://host.docker.internal:8077`）。不配 = /embed 恒 503，不静默。
EMBED_UPSTREAM = os.environ.get('EMBED_UPSTREAM', '').rstrip('/')
EMBED_TIMEOUT = 60
# `/parse` 只许读这些根之下的文件（见 `guard_path`）。默认 `/kbdata` = 挂进来的知识库根；
# `/tmp` 是给探针造夹具用的。**别为了「方便」加 `/`** —— 那就等于把守卫删掉。
PARSE_ROOTS = [p for p in os.environ.get('PARSE_ROOTS', '/kbdata:/tmp').split(':') if p]


def health():
    h = {'ok': True, 'container': True,
         'parse_ok': es.parse_ok(), 'parse_caps': es.parse_caps()}
    if EMBED_UPSTREAM:
        # model/dim 是宿主机 embed 服务的属性，本服务不 embed → 只能问它。
        # 合并顺序**上游在前、自己在后**：上游的 parse_ok 里 docx/pptx 是 false（正是本轮要治的病），
        # 让它覆盖容器的真结果就等于把体检报告写反。
        try:
            with urllib.request.urlopen(f'{EMBED_UPSTREAM}/health', timeout=3) as r:
                h = {**json.loads(r.read()), **h}
        except Exception as e:
            h['embed_upstream_error'] = str(e)
    return h


def proxy(path, body):
    """/embed 等非解析路由透传给宿主机 embed 服务。
    没配上游就 **503 明说** —— 回空 embeddings 会让检索静默降级成词典召回。"""
    if not EMBED_UPSTREAM:
        raise es.ParseError('embed_upstream_unset',
                            f'本服务只做 /parse 与 /chunk；{path} 需要设 EMBED_UPSTREAM'
                            '（例 http://host.docker.internal:8077）', 503)
    req = urllib.request.Request(EMBED_UPSTREAM + path, json.dumps(body).encode(),
                                 {'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=EMBED_TIMEOUT) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        # 原样带回上游状态码：吞成 500 会把上游的可修错误报成「服务挂了」
        raise es.ParseError('embed_upstream', e.read().decode('utf-8', 'replace')[:500], e.code)
    except Exception as e:
        raise es.ParseError('embed_upstream', str(e), 502)


# `/parse` 收的是**路径**，而这个服务原先对它不做任何检查 —— 实测
# `POST /parse {"path":"/etc/passwd","mime":"text/plain"}` → **200 原样返回全文**，
# `{"path":"/app/tools/settings.py"}` 返回源码。也就是一个无鉴权的任意文件读接口。
# 容器还挂着 `D:\kbdata`（全部客户文档），所以这条缝的代价是整个知识库。
#
# 收容分两层：① `scripts/parser.ps1` 把发布面绑回 `127.0.0.1`（见那里的注释）；
# ② **这一层** —— path 必须落在允许的根之内。只有 ② 拦得住「下一个人又写了一次 `-p`」。
# 允许根默认 `/kbdata`（Rust 侧传进来的就是 kb_root 下的文件），可用 `PARSE_ROOTS` 覆盖。
def guard_path(p):
    if not p:
        raise es.ParseError('bad_request', '缺 path', 400)
    try:
        real = os.path.realpath(str(p))       # realpath：连符号链接一起拍平，别只 normpath
    except OSError as e:
        raise es.ParseError('bad_request', f'path 无法解析：{e}', 400)
    for root in PARSE_ROOTS:
        r = os.path.realpath(root)
        if real == r or real.startswith(r + os.sep):
            return
    # 不回显 real —— 那本身是一点信息泄漏（告诉对方我们把路径解析到了哪）
    raise es.ParseError(
        'forbidden',
        f'path 必须落在 {":".join(PARSE_ROOTS)} 之内（本服务只解析知识库里的文件）',
        403,
    )


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _send(self, status, obj):
        body = json.dumps(obj, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json; charset=utf-8')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self._send(200, health()) if self.path == '/health' else self._send(404, {'error': 'not found'})

    def do_POST(self):
        try:
            body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
            if self.path.startswith('/parse'):
                guard_path(body.get('path'))
            # /parse 与 /chunk 全权交给 es.handle_post（含它的路由与默认值）；
            # 其余走上游 —— es 对未知路径的兜底也是「当 embed 处理」，同形。
            resp = es.handle_post(self.path, body) \
                if self.path.startswith(('/parse', '/chunk')) else proxy(self.path, body)
            self._send(200, resp)
        except es.ParseError as e:
            self._send(e.status, e.payload)
        except Exception as e:
            self._send(500, {'error': str(e)})


if __name__ == '__main__':
    bad = [f"{e}（{c['why']}）" for e, c in sorted(es.parse_caps().items()) if not c['ok']]
    print(f'解析服务就绪 :{PORT}  parse_ok={es.parse_ok()}  '
          f'embed 上游={EMBED_UPSTREAM or "(未配，/embed 恒 503)"}'
          + ''.join(f'\n  ⛔ {b}' for b in bad), flush=True)
    # ThreadingHTTPServer（es.serve 用的是单线程 HTTPServer）：一份多页扫描件 OCR 要几十秒，
    # 单线程会把 /health 探针一起堵死，容器编排看到的就是「服务挂了」。
    ThreadingHTTPServer(('0.0.0.0', PORT), H).serve_forever()
