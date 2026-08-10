# settings.json 的读取口：把「凭据从哪来」收在一处。
#
# 🔴 为什么需要它：`tools/embed_service.py` 与 `tools/cleanup_autodiscover.py` 里原先各写着
# 自有 PG 的**明文口令**（原文已删，不在任何注释里复述），违反本仓那条纪律 ——
# **明文凭据只在 settings.json**，不进库、不进日志、不进响应、不烤进镜像层。
# 而 `settings.json` 里本来就有 `pg_url`，也就是说那两处是无谓的第二份真相源：
# 改了 settings 里的口令，服务照旧拿旧口令连（连不上还得去猜为什么）。
#
# 【D1】settings.json 落盘态可以是 `enc:v1:` AES-256-GCM 密文（服务端启动时幂等迁移）。
# `load()` 在这里统一透明解密：判官/探针拿到的仍是明文 kwargs，**整个 Python 工具链零改动**。
# 派钥与字段清单镜像 Rust 侧 `crates/server/src/db.rs`（改一边必须同步另一边）：
# `DMS_SECRET_KEY`（≥32 字节）sha2-256；未配置则由 hostname+username 机器指纹派生 ——
# 跨机不可迁移，换机/换用户后密文解不开（重填明文凭据或配上环境变量）。
# 解密依赖 `cryptography` 包（仓库 .venv 已装）—— 只在真撞上 enc:v1: 值时才 import，
# 明文配置（如 settings.docker.json 模板）不需要它。
#
# 顺带收掉另一处重复：`judge_scope.py` / `probe_values.py` / `cleanup_autodiscover.py`
# 各抄了一份同样的 `mysql://` 手写正则。手写正则解析 URL 是有坑的
# （口令里的 `@` 必须 percent-encode，所以调用方都得记着调 `unquote`；
# 忘一个就是「口令看着对、连不上」）。
# 改用 stdlib 的 `urlsplit` —— 它自己认 userinfo/host/port/path，`unquote` 也由这里统一做。
import base64
import hashlib
import json
import os
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent
ANALYSIS_TARGET_ENV = "DMSAI_ANALYSIS_TARGET"
ANALYSIS_TARGET_TYPES = frozenset({"doris", "warehouse"})

# 【D1】密文前缀（镜像 Rust `crypto::ENC_PREFIX`）
ENC_PREFIX = "enc:v1:"
# 【D1】敏感字段清单（镜像 Rust `db.rs` 的 SECRET_SCALARS / SECRET_MAP_VALUES + 两个特例）
_SECRET_SCALARS = ("mysql_url", "pg_url", "pg_ro_url", "llm_api_key", "wework_secret")
_SECRET_MAP_VALUES = ("llm_keys", "datasources")


def _first_env(names):
    """第一个**存在**的环境变量；存在但空白 = 回默认（不再试下一个 —— 与 Rust 侧同语义）。"""
    for n in names:
        v = os.environ.get(n)
        if v is not None:
            return v if v.strip() else None
    return None


_KEY = None


def _settings_key():
    """进程级 32B 钥匙（只存内存，不落盘不打印）。镜像 Rust `crypto::default_key`。"""
    global _KEY
    if _KEY is None:
        raw = os.environ.get("DMS_SECRET_KEY") or ""
        if raw:
            _KEY = hashlib.sha256(raw.encode("utf-8")).digest()
        else:
            # 机器指纹兜底：跨机不可迁移（换机/换用户/容器重建后解不开，重填明文凭据）
            host = _first_env(("HOSTNAME", "COMPUTERNAME")) or "unknown-host"
            user = _first_env(("USER", "USERNAME")) or "unknown-user"
            _KEY = hashlib.sha256(
                f"dms-ai/settings-enc v1\nhost={host}\nuser={user}".encode("utf-8")
            ).digest()
    return _KEY


def _decrypt(value):
    """`enc:v1:` 密文 → 明文；**无前缀原样返回**（明文兼容 + 幂等）。错误文案不带值片段。"""
    if not isinstance(value, str) or not value.startswith(ENC_PREFIX):
        return value
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except ImportError:
        raise SystemExit(
            "settings.json 含 enc:v1: 加密凭据：读它需要先 `pip install cryptography`"
            "（仓库 .venv 已装；新机器的判官环境照装一份）"
        ) from None
    try:
        blob = base64.b64decode(value[len(ENC_PREFIX):], validate=True)
    except Exception:
        raise SystemExit("settings.json 的加密凭据不是合法 base64（文件已损坏）") from None
    if len(blob) < 12 + 16:  # nonce(96bit) ‖ ciphertext ‖ tag(128bit)
        raise SystemExit("settings.json 的加密凭据长度不足（被截断）")
    try:
        return AESGCM(_settings_key()).decrypt(blob[:12], blob[12:], None).decode("utf-8")
    except Exception:
        raise SystemExit(
            "settings.json 加密凭据解密失败：DMS_SECRET_KEY 不对，或未配置时机器指纹已变"
            "（换机/换用户/容器重建后需重填明文凭据）"
        ) from None


def _decrypt_secrets(cfg):
    """敏感字段透明解密（只动 enc:v1: 前缀的值）。镜像 Rust `Settings::decrypt_secrets`。"""
    for f in _SECRET_SCALARS:
        if f in cfg:
            cfg[f] = _decrypt(cfg[f])
    for f in _SECRET_MAP_VALUES:
        m = cfg.get(f)
        if isinstance(m, dict):
            cfg[f] = {k: _decrypt(v) for k, v in m.items()}
    targets = cfg.get("mysql_targets")
    if isinstance(targets, dict):
        for name, t in list(targets.items()):
            if isinstance(t, str):
                targets[name] = _decrypt(t)
            elif isinstance(t, dict) and "url" in t:
                t["url"] = _decrypt(t["url"])
    # mcp_keys 的键名本身是凭据（值是 login_name，非密）
    mcp = cfg.get("mcp_keys")
    if isinstance(mcp, dict):
        cfg["mcp_keys"] = {_decrypt(k): v for k, v in mcp.items()}
    return cfg



def settings_path():
    """被测进程使用哪份配置，判官就显式读取同一份。

    默认仍是仓库根的 settings.json；Docker 判官通过 DMSAI_SETTINGS 指向
    settings.docker.json，避免 Python 从一套 DMS 身份库挑人、容器去另一套库验人。
    """
    raw = os.environ.get("DMSAI_SETTINGS", "settings.json")
    p = Path(raw)
    return p if p.is_absolute() else ROOT / p


def load():
    """读 settings.json（敏感字段 enc:v1: 密文在此透明解密）。缺文件就明确说清怎么办。"""
    settings = settings_path()
    if not settings.exists():
        raise SystemExit(
            f"缺 {settings} —— 复制 settings.example.json 改成真值（明文凭据只许住在这里）"
        )
    return _decrypt_secrets(json.loads(settings.read_text(encoding="utf-8")))


def _dsn(raw, label):
    """一个数据库 URL → DB 驱动的连接 kwargs，不在错误里回显 URL。"""
    try:
        u = urlsplit(raw)
        host = u.hostname
        port = u.port
    except (TypeError, ValueError):
        raise SystemExit(f"settings.json 的 {label} 不是可解析的数据库 URL") from None
    if not host:
        raise SystemExit(f"settings.json 的 {label} 不是可解析的 URL（要 scheme://user:pwd@host:port/db）")
    return {
        "host": host,
        # 端口缺省按 scheme 给：mysql 3306 / postgres 5432。本仓自有 PG 是 15433，URL 里都写着。
        "port": port or (3306 if u.scheme.startswith("mysql") else 5432),
        "user": unquote(u.username or ""),
        "password": unquote(u.password or ""),
        "dbname": u.path.lstrip("/"),
    }


def dsn(key, cfg=None):
    """`settings.json` 里某个顶层 URL 键 → DB 驱动的连接 kwargs。

    返回 `{host, port, user, password, dbname}`。**用 host/port 而不是原样传 URL**：
    pymysql 与 psycopg2 都吃 kwargs，而 psycopg2 虽然也认 URL、pymysql 不认。
    percent-decode 在这里做一次，调用方不必记得 `unquote`。
    """
    cfg = cfg if cfg is not None else load()
    raw = cfg.get(key)
    if not raw:
        raise SystemExit(f"settings.json 里没有 {key}（参照 settings.example.json 补上）")
    return _dsn(raw, key)


def _analysis_target_entry(raw):
    """规范化一个 mysql_targets 条目；错误文本绝不包含连接 URL。"""
    if isinstance(raw, str):
        return (raw.strip(), False, None) if raw.strip() else (None, False, "旧字符串 URL 为空")
    if not isinstance(raw, dict):
        return None, False, "必须是旧 URL 字符串或 {url,type} 对象"

    target_type = raw.get("type")
    if not isinstance(target_type, str) or not target_type.strip():
        return None, True, "对象缺少 type（仅允许 doris/warehouse）"
    target_type = target_type.strip().lower()
    if target_type not in ANALYSIS_TARGET_TYPES:
        if target_type == "production_lookup":
            return None, True, "type=production_lookup 是生产点查库，不允许离线分析"
        return None, True, "对象 type 未知（仅允许 doris/warehouse）"

    url = raw.get("url")
    if not isinstance(url, str) or not url.strip():
        return None, True, "数仓对象缺少非空 url"
    return url.strip(), True, None


def analysis_target(cfg=None):
    """选择离线事实探针使用的非 DMS 分析目标，返回 ``(name, url)``。

    新对象只接受显式 ``type=doris|warehouse``；旧字符串仅为兼容保留，并排除
    与身份库相同的 host:port。绝不回退 ``mysql_url``。
    """
    cfg = cfg if cfg is not None else load()
    raw_targets = cfg.get("mysql_targets") or {}
    if not isinstance(raw_targets, dict):
        raise SystemExit("settings.json 的 mysql_targets 必须是对象")
    auth = _dsn(cfg.get("mysql_url", ""), "mysql_url")
    auth_endpoint = (str(auth["host"]).lower(), auth["port"])
    targets = {}
    rejected = {}
    for raw_name, raw in raw_targets.items():
        name = str(raw_name)
        url, explicit, reason = _analysis_target_entry(raw)
        if reason:
            rejected[name] = reason
            continue
        try:
            parsed = _dsn(url, f"mysql_targets.{name}")
        except SystemExit as exc:
            rejected[name] = str(exc)
            continue
        endpoint = (str(parsed["host"]).lower(), parsed["port"])
        if endpoint == auth_endpoint:
            rejected[name] = "与身份库使用同一 endpoint，已拒绝"
            continue
        targets[name] = (url, explicit)

    requested = os.environ.get(ANALYSIS_TARGET_ENV, "").strip()
    if requested:
        if requested not in targets:
            if requested in rejected:
                raise SystemExit(
                    f"{ANALYSIS_TARGET_ENV}={requested} 已拒绝：{rejected[requested]}"
                )
            names = ", ".join(sorted(targets)) or "无"
            raise SystemExit(f"{ANALYSIS_TARGET_ENV}={requested} 未配置（可选：{names}）")
        return requested, targets[requested][0]

    explicit = {name: value for name, value in targets.items() if value[1]}
    legacy = {name: value for name, value in targets.items() if not value[1]}
    for pool in (explicit, legacy):
        if "doris_warehouse" in pool:
            return "doris_warehouse", pool["doris_warehouse"][0]
        if pool:
            name = sorted(pool, key=str.lower)[0]
            return name, pool[name][0]

    reasons = "；".join(f"{name}（{rejected[name]}）" for name in sorted(rejected)) or "未配置目标"
    raise SystemExit(f"settings.json 没有可用离线分析数仓目标：{reasons}")


def mysql_kwargs(cfg=None):
    """DMS 身份/角色/权限库；禁止用于业务事实、实体值域或基数探测。"""
    cfg = cfg if cfg is not None else load()
    if not urlsplit(cfg.get("mysql_url", "")).scheme.startswith("mysql"):
        raise SystemExit("settings.json 的 mysql_url 必须使用 mysql://")
    d = dsn("mysql_url", cfg)
    d["database"] = d.pop("dbname")
    d["charset"] = "utf8mb4"
    return d


def analysis_mysql_kwargs(cfg=None):
    """非 DMS 的只读 MySQL/Doris 分析目标，供离线事实探针使用。"""
    name, raw = analysis_target(cfg)
    if not urlsplit(raw).scheme.startswith("mysql"):
        raise SystemExit(f"settings.json 的 mysql_targets.{name} 必须使用 mysql://")
    d = _dsn(raw, f"mysql_targets.{name}")
    d["database"] = d.pop("dbname")
    d["charset"] = "utf8mb4"
    return d


def pg_kwargs(cfg=None):
    """自有 PG（元数据 / 知识库 / 向量）。psycopg2 的 db 参数就叫 `dbname`。"""
    cfg = cfg if cfg is not None else load()
    if not urlsplit(cfg.get("pg_url", "")).scheme.startswith("postgres"):
        raise SystemExit("settings.json 的 pg_url 必须使用 postgres://")
    return dsn("pg_url", cfg)


if __name__ == "__main__":  # python tools/settings.py —— 自检，不读真 settings.json
    assert settings_path() == ROOT / os.environ.get("DMSAI_SETTINGS", "settings.json")
    old_requested = os.environ.pop(ANALYSIS_TARGET_ENV, None)
    fake = {
        # 口令里带 `@` 与 `:`，正是手写正则会解析错的那两个字符
        "mysql_url": "mysql://identity:p%40w@1.2.3.4:3306/xh_dms",
        "mysql_targets": {
            "dms": "mysql://ignored:ignored@9.9.9.9/xh_dms",
            "renamed_dms": "mysql://other:other@1.2.3.4:3306/other_db",
            "analytics": "mysql://reader:a%3Ab@2.3.4.5:3306/facts",
            "doris_warehouse": {
                "url": "mysql://reader:p%40w@3.4.5.6:9030/dms_ods",
                "type": "warehouse",
            },
            "production": {
                "url": "mysql://reader:hidden@4.5.6.7:3306/xh_dms",
                "type": "production_lookup",
            },
            "unknown": {
                "url": "mysql://reader:hidden@5.6.7.8:3306/other",
                "type": "future_engine",
            },
        },
        "pg_url": "postgres://postgres:p%3Aw%40rd@localhost:15433/dms_ai",
        "no_port": "postgres://u:p@h/db",
    }
    m = mysql_kwargs(fake)
    assert m == {"host": "1.2.3.4", "port": 3306, "user": "identity",
                 "password": "p@w", "database": "xh_dms", "charset": "utf8mb4"}, m
    assert analysis_target(fake)[0] == "doris_warehouse"
    no_doris = dict(fake, mysql_targets={
        "DMS": "mysql://ignored:ignored@9.9.9.9/xh_dms",
        "renamed_dms": "mysql://other:other@1.2.3.4:3306/other_db",
        "analytics": "mysql://reader:a%3Ab@2.3.4.5:3306/facts",
    })
    assert analysis_target(no_doris)[0] == "analytics"
    a = analysis_mysql_kwargs(fake)
    assert a == {"host": "3.4.5.6", "port": 9030, "user": "reader",
                 "password": "p@w", "database": "dms_ods", "charset": "utf8mb4"}, a
    try:
        os.environ[ANALYSIS_TARGET_ENV] = "production"
        try:
            analysis_target(fake)
            raise AssertionError("production_lookup 不应成为离线分析目标")
        except SystemExit as exc:
            assert "生产点查库" in str(exc) and "mysql://" not in str(exc)

        os.environ[ANALYSIS_TARGET_ENV] = "unknown"
        try:
            analysis_target(fake)
            raise AssertionError("未知对象 type 不应成为离线分析目标")
        except SystemExit as exc:
            assert "type 未知" in str(exc) and "mysql://" not in str(exc)

        os.environ[ANALYSIS_TARGET_ENV] = "renamed_dms"
        try:
            analysis_target(fake)
            raise AssertionError("同身份库 endpoint 的 legacy 目标不应成为离线分析目标")
        except SystemExit as exc:
            assert "同一 endpoint" in str(exc) and "mysql://" not in str(exc)

        os.environ.pop(ANALYSIS_TARGET_ENV, None)
        blocked_only = dict(fake, mysql_targets={
            "production": fake["mysql_targets"]["production"],
            "unknown": fake["mysql_targets"]["unknown"],
        })
        try:
            analysis_target(blocked_only)
            raise AssertionError("非 warehouse 对象不应成为默认离线分析目标")
        except SystemExit as exc:
            assert "没有可用离线分析数仓目标" in str(exc) and "mysql://" not in str(exc)
    finally:
        if old_requested is None:
            os.environ.pop(ANALYSIS_TARGET_ENV, None)
        else:
            os.environ[ANALYSIS_TARGET_ENV] = old_requested
    p = pg_kwargs(fake)
    assert p == {"host": "localhost", "port": 15433, "user": "postgres",
                 "password": "p:w@rd", "dbname": "dms_ai"}, p
    # 端口缺省按 scheme
    assert dsn("no_port", fake)["port"] == 5432
    # 缺键 / 不是 URL 一律当场退出，不许返半个配置让调用方拿去连
    for bad in [{}, {"pg_url": ""}, {"pg_url": "不是URL"}]:
        try:
            dsn("pg_url", bad)
            raise AssertionError(f"没拦住 {bad}")
        except SystemExit:
            pass
    # 【D1】enc:v1: 透明解密：无前缀原样放行；密文按进程派钥往返；坏密文响亮失败
    assert _decrypt("mysql://u:p@h/db") == "mysql://u:p@h/db"
    assert _decrypt(None) is None
    assert _decrypt_secrets({"mysql_url": "mysql://u:p@h/db", "listen": "x"}) == {
        "mysql_url": "mysql://u:p@h/db", "listen": "x",
    }
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM as _AESGCM
    except ImportError:
        _AESGCM = None
    if _AESGCM is not None:
        _nonce = b"\x01" * 12
        _sealed = "enc:v1:" + base64.b64encode(
            _nonce + _AESGCM(_settings_key()).encrypt(_nonce, "sk-机密@x".encode(), None)
        ).decode()
        assert _decrypt(_sealed) == "sk-机密@x"
        # 字段遍历全形态：标量 / mysql_targets 两种形态 / llm_keys 值 / mcp_keys 键名；非敏感不动
        _cfg = _decrypt_secrets({
            "mysql_url": _sealed,
            "mysql_targets": {"a": _sealed, "b": {"url": _sealed, "type": "warehouse"}},
            "llm_keys": {"qwen": _sealed},
            "mcp_keys": {_sealed: "alice"},
            "listen": "127.0.0.1:8100",
        })
        assert _cfg["mysql_url"] == "sk-机密@x"
        assert _cfg["mysql_targets"]["a"] == "sk-机密@x"
        assert _cfg["mysql_targets"]["b"] == {"url": "sk-机密@x", "type": "warehouse"}
        assert _cfg["llm_keys"] == {"qwen": "sk-机密@x"}
        assert _cfg["mcp_keys"] == {"sk-机密@x": "alice"}
        assert _cfg["listen"] == "127.0.0.1:8100"
        # 错钥匙（别的钥匙封的）：响亮失败且文案不带值片段
        _bad = "enc:v1:" + base64.b64encode(
            _nonce + _AESGCM(b"\x02" * 32).encrypt(_nonce, b"top-secret", None)
        ).decode()
        try:
            _decrypt(_bad)
            raise AssertionError("错钥匙必须响亮失败")
        except SystemExit as exc:
            assert "top-secret" not in str(exc)
        # 坏 base64 / 截断同样响亮
        for _corrupt in ["enc:v1:!!!", "enc:v1:" + base64.b64encode(b"\x00" * 8).decode()]:
            try:
                _decrypt(_corrupt)
                raise AssertionError(f"坏密文没拦住 {_corrupt!r}")
            except SystemExit:
                pass
    else:
        print("（跳过 enc:v1: 往返自检：未装 cryptography —— 明文配置不受影响）")
    # 🔴 纪律断言：`tools/` 下不许出现**字面量口令赋值**。
    #
    # 判据刻意是通用形状（`password='…'` / `password="…"`）而不是「搜某个已知口令串」：
    # 搜已知串只挡得住这一个口令，而且**断言自己就得把口令写进来** —— 那等于为了检查泄漏
    # 而泄漏一次（我第一版就是这么写的，自检当场把自己的注释和断言一起抓了出来，
    # 连注释里引用口令原文也算泄漏）。
    # 本文件用的是 `"password": unquote(...)`（dict 键 + 变量值），不匹配这个形状。
    import re as _re
    lit = _re.compile(r"""password\s*[=:]\s*['"][^'"]""")
    # 扫描**跳过本文件**：它是这条判据的实现，里面必然有 `password='x'` 形状的
    # 反向验证样本（就在下面两行）。判据不扫自己是常规做法，但得写明白 ——
    # 代价是「有人把真口令写进 settings.py」这一种情况扫不到，
    # 而那也是这个文件唯一被允许碰凭据的地方（它只从 settings.json 读，不存）。
    hits = [
        f"{p.name}:{i}"
        for p in ROOT.joinpath("tools").glob("*.py")
        if p.name != "settings.py"
        for i, line in enumerate(p.read_text(encoding="utf-8").splitlines(), 1)
        if lit.search(line)
    ]
    assert not hits, f"tools/ 里有字面量口令赋值：{hits}"
    # 反向验证：这条断言真的抓得住（不然它就是又一个恒真断言）
    assert lit.search("password='x'") and lit.search('password = "y"')
    assert not lit.search("password=cfg['pw']") and not lit.search('"password": unquote(u.password)')
    print("settings.py 自检通过（含「tools/ 无字面量口令赋值」那条纪律断言 + 它自己的反向验证）")
