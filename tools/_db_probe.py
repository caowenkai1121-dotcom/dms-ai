# -*- coding: utf-8 -*-
"""测一个 MySQL DSN 的只读性（settings 保存链的同一道闸）+ 列库名。只读操作。"""
import sys
import pymysql

host, port, user, pw = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
conn = pymysql.connect(host=host, port=port, user=user, password=pw,
                       connect_timeout=10, read_timeout=15)
cur = conn.cursor()
cur.execute("SELECT @@SESSION.transaction_read_only, CURRENT_USER()")
ro, who = cur.fetchone()
print(f"session_read_only={ro} (1=只读可保存, 0=可写必被拒) user={who}")
cur.execute("SHOW DATABASES")
print("databases:", [r[0] for r in cur.fetchall()])
conn.close()
