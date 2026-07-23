#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""向量层：bge-small-zh-v1.5(512维, fastembed 本地自包含)。
  build  —— 为 meta.table_doc 计算 embedding 存 pgvector + HNSW 索引（离线一次）
  serve  —— 常驻 HTTP :8077，POST /embed {"texts":[...],"query":true} → {"embeddings":[[...]]}
用法: python embed_service.py build  /  python embed_service.py serve [port]
"""
import os, sys, json
sys.stdout.reconfigure(encoding='utf-8')

MODEL = 'BAAI/bge-small-zh-v1.5'
DIM = 512
QUERY_INSTRUCT = '为这个句子生成表示以用于检索相关文章：'
PG = dict(host='localhost', port=15433, user='postgres', password='dmsai_pg_2026', dbname='dms_ai')

_embedder = None
def embedder():
    global _embedder
    if _embedder is None:
        from fastembed import TextEmbedding
        _embedder = TextEmbedding(model_name=MODEL)
    return _embedder

def embed(texts, is_query=False):
    if is_query:
        texts = [QUERY_INSTRUCT + t for t in texts]
    return [v.tolist() for v in embedder().embed(texts)]

def build():
    import psycopg2
    pg = psycopg2.connect(**PG); pg.autocommit = True; cur = pg.cursor()
    cur.execute("CREATE EXTENSION IF NOT EXISTS vector")
    cur.execute(f"ALTER TABLE meta.table_doc ADD COLUMN IF NOT EXISTS embedding vector({DIM})")
    cur.execute("SELECT table_name, search_doc FROM meta.table_doc")
    rows = cur.fetchall()
    print(f'计算 {len(rows)} 表 embedding …', flush=True)
    docs = [(r[1] or r[0])[:1000] for r in rows]
    vecs = embed(docs, is_query=False)
    for (tname, _), v in zip(rows, vecs):
        cur.execute("UPDATE meta.table_doc SET embedding = %s WHERE table_name = %s",
                    ('[' + ','.join(f'{x:.6f}' for x in v) + ']', tname))
    cur.execute("DROP INDEX IF EXISTS meta.idx_doc_hnsw")
    cur.execute("CREATE INDEX idx_doc_hnsw ON meta.table_doc USING hnsw (embedding vector_cosine_ops)")
    print(f'完成：{len(rows)} 表向量化 + HNSW 索引', flush=True)
    # 语料问句向量（供语义缓存召回；问句侧 query embedding，与 Rust 回写一致）
    cur.execute("SELECT id, question FROM meta.sql_exemplar WHERE status='enabled' AND embedding IS NULL")
    ex = cur.fetchall()
    if ex:
        evecs = embed([r[1] for r in ex], is_query=True)
        for (eid, _), v in zip(ex, evecs):
            cur.execute("UPDATE meta.sql_exemplar SET embedding = %s WHERE id = %s",
                        ('[' + ','.join(f'{x:.6f}' for x in v) + ']', eid))
        print(f'完成：{len(ex)} 条语料问句向量化', flush=True)
    pg.close()

def serve(port=8077):
    from http.server import BaseHTTPRequestHandler, HTTPServer
    embedder()
    print(f'embed 服务就绪 :{port}（{MODEL}, {DIM}维）', flush=True)
    class H(BaseHTTPRequestHandler):
        def log_message(self, *a): pass
        def do_POST(self):
            n = int(self.headers.get('Content-Length', 0))
            try:
                body = json.loads(self.rfile.read(n) or b'{}')
                texts = body.get('texts', [])
                is_q = bool(body.get('query', True))
                out = embed(texts, is_query=is_q) if texts else []
                resp = json.dumps({'embeddings': out}).encode()
                self.send_response(200)
            except Exception as e:
                resp = json.dumps({'error': str(e)}).encode()
                self.send_response(500)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(resp)))
            self.end_headers()
            self.wfile.write(resp)
    HTTPServer(('127.0.0.1', port), H).serve_forever()

if __name__ == '__main__':
    mode = sys.argv[1] if len(sys.argv) > 1 else 'serve'
    if mode == 'build':
        build()
    else:
        serve(int(sys.argv[2]) if len(sys.argv) > 2 else 8077)
