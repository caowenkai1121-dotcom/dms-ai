# -*- coding: utf-8 -*-
"""在服务器上查 DMS 身份库：指定账号的员工/角色实况（只读）。settings.docker.json 已是 enc:v1 密文，用 .secret_key 解。"""
import base64
import hashlib
import json
import sys
from urllib.parse import urlparse, unquote

from cryptography.hazmat.primitives.ciphers.aead import AESGCM

key = hashlib.sha256(open('/opt/dms-ai/.secret_key', encoding='utf-8').read().encode('utf-8')).digest()


def dec(v):
    if not isinstance(v, str) or not v.startswith('enc:v1:'):
        return v
    raw = base64.b64decode(v[7:])
    return AESGCM(key).decrypt(raw[:12], raw[12:], None).decode()


cfg = json.load(open('/opt/dms-ai/settings.docker.json'))
dsn = dec(cfg['mysql_url'])
u = urlparse(dsn)
import pymysql
conn = pymysql.connect(host=u.hostname, port=u.port, user=unquote(u.username), password=unquote(u.password),
                       database=u.path.lstrip('/'), connect_timeout=15, read_timeout=30)
cur = conn.cursor()
key_arg = sys.argv[1] if len(sys.argv) > 1 else '15810080274'
cur.execute("SELECT column_name FROM information_schema.columns WHERE table_schema=%s AND table_name='t_employee' ORDER BY ordinal_position", (u.path.lstrip('/'),))
print('t_employee cols:', [r[0] for r in cur.fetchall()])
cur.execute("SELECT employee_id, employee_num, login_name, actual_name, phone, disabled_flag, deleted_flag, administrator_flag FROM t_employee WHERE login_name=%s OR actual_name=%s OR phone=%s", (key_arg, key_arg, key_arg))
rows = cur.fetchall()
print('t_employee:', rows)
for (eid, *_rest) in rows:
    cur.execute("SELECT r.role_id, r.role_code FROM t_role_employee re "
                "JOIN t_role r ON r.role_id = re.role_id WHERE re.employee_id=%s ORDER BY r.role_id", (eid,))
    print('roles:', cur.fetchall())
conn.close()
