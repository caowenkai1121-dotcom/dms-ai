# -*- coding: utf-8 -*-
"""列生产 DMS 的角色与在岗人数（只读）。"""
import base64
import hashlib
import json
from urllib.parse import urlparse, unquote

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

key = hashlib.sha256(open('/opt/dms-ai/.secret_key', encoding='utf-8').read().encode('utf-8')).digest()


def dec(v):
    if not isinstance(v, str) or not v.startswith('enc:v1:'):
        return v
    raw = base64.b64decode(v[7:])
    return AESGCM(key).decrypt(raw[:12], raw[12:], None).decode()


cfg = json.load(open('/opt/dms-ai/settings.docker.json'))
u = urlparse(dec(cfg['mysql_url']))
import pymysql
conn = pymysql.connect(host=u.hostname, port=u.port, user=unquote(u.username), password=unquote(u.password),
                       database=u.path.lstrip('/'), connect_timeout=15, read_timeout=30)
cur = conn.cursor()
cur.execute("SELECT r.role_id, r.role_code, COUNT(re.employee_id) AS n FROM t_role r "
            "LEFT JOIN t_role_employee re ON re.role_id = r.role_id GROUP BY 1,2 ORDER BY 1")
for r in cur.fetchall():
    print(' role:', r)
conn.close()
