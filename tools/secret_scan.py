#!/usr/bin/env python3
"""Fail when a Git-add candidate contains credentials or local runtime data."""

from __future__ import annotations

import ipaddress
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]
MAX_TEXT_BYTES = 10 * 1024 * 1024
CONFIG_DOC_EXTS = {".conf", ".env", ".json", ".md", ".toml", ".txt", ".yaml", ".yml"}
BINARY_EXTS = {
    ".7z", ".bak", ".bmp", ".db", ".doc", ".docx", ".err", ".gif", ".gz",
    ".jpeg", ".jpg", ".key", ".log", ".out", ".p12", ".pdf", ".pem", ".pfx",
    ".png", ".ppt", ".pptx", ".sqlite", ".sqlite3", ".tar", ".tmp", ".webp",
    ".xls", ".xlsx", ".zip",
}
ASSET_ALLOWLIST = {
    "tools/kb_fixtures/内部推荐奖金办法.docx",
    "tools/kb_fixtures/劳保用品采购标准_扫描通知.png",
    "tools/kb_fixtures/员工公寓管理办法_文本层.pdf",
    "tools/kb_fixtures/差旅补贴标准_表格.xlsx",
    "tools/kb_fixtures/新员工入职引导.pptx",
    "tools/kb_fixtures/通讯补贴标准_岗位分级.xlsx",
    "tools/kb_fixtures/食堂就餐补助通知_扫描件无文本层.pdf",
}
LOCAL_PATH_PARTS = {
    ".artifacts", ".cache", ".secrets", "kb_data", "kbdata", "logs", "screenshots",
    "secrets", "storage", "temp", "tmp", "tp", "uploads",
}
PLACEHOLDER_WORDS = {
    "changeme", "dummy", "example", "fake", "placeholder", "redacted", "replace-me",
    "replace_me", "test", "your-key", "your-secret", "your_key", "your_secret",
}
FAKE_PASSWORDS = {
    "a:b", "hidden", "ignored", "other", "p", "p@ss", "p:secret", "pass", "password",
    "postgres", "readonly", "s3cret", "secret", "test", "u",
}

TOKEN_RULES = {
    "openai-compatible-key": re.compile(r"(?<![A-Za-z0-9])sk-[A-Za-z0-9_-]{16,}"),
    "github-token": re.compile(r"(?<![A-Za-z0-9])(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})"),
    "aws-access-key": re.compile(r"(?<![A-Z0-9])(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Z0-9])"),
    "slack-token": re.compile(r"(?<![A-Za-z0-9])xox[baprs]-[A-Za-z0-9-]{12,}"),
    "private-key": re.compile(r"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----"),
}
URI_RE = re.compile(
    r"(?i)\b(?:mysql|postgres(?:ql)?|doris|redis|mongodb(?:\+srv)?)://[^\s\"'<>`]+"
)
NAMED_SECRET_RE = re.compile(
    r"(?i)\b(?P<name>api[_-]?key|secret|password|passwd|pwd|access[_-]?token|"
    r"refresh[_-]?token|private[_-]?key|database[_-]?url|dsn)\b[\"']?\s*[:=]\s*"
    r"[\"'](?P<value>[^\"'\r\n]+)[\"']"
)
PRIVATE_IP_RE = re.compile(
    r"(?<!\d)(?:10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|"
    r"172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2})(?!\d)"
)
WEWORK_CORP_RE = re.compile(r"(?<![A-Za-z0-9])ww[A-Za-z0-9]{14,}(?![A-Za-z0-9])")
USER_HOME_RE = re.compile(r"(?i)\b[A-Z]:\\Users\\[^\\\s]+")


def git_candidates() -> list[Path]:
    out = subprocess.check_output(
        ["git", "-C", str(ROOT), "ls-files", "-z", "--cached", "--others", "--exclude-standard"]
    )
    return [ROOT / name for name in out.decode("utf-8").split("\0") if name]


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def is_placeholder(value: str) -> bool:
    value = unquote(value).strip().strip("`\"'")
    lower = value.lower()
    if not value or (value.startswith("<") and value.endswith(">")):
        return True
    if value.startswith("${") or value.startswith("{{"):
        return True
    compact = re.sub(r"[^a-z0-9_-]", "", lower)
    return compact in PLACEHOLDER_WORDS or compact.startswith(("example_", "your_", "test_", "dummy_"))


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def credential_uri_finding(uri: str) -> bool:
    try:
        parsed = urlsplit(uri.rstrip(".,);]"))
        password = unquote(parsed.password or "")
        host = parsed.hostname or ""
    except ValueError:
        return False
    if not password or is_placeholder(password) or password.lower() in FAKE_PASSWORDS:
        return False
    try:
        private_host = ipaddress.ip_address(host).is_private and not ipaddress.ip_address(host).is_loopback
    except ValueError:
        private_host = False
    return len(password) >= 8 or (private_host and len(password) >= 6)


def named_secret_finding(name: str, value: str) -> bool:
    if is_placeholder(value):
        return False
    compact = re.sub(r"\s+", "", unquote(value))
    if name.lower() in {"password", "passwd", "pwd"}:
        return len(compact) >= 10 and compact.lower() not in FAKE_PASSWORDS
    return len(compact) >= 16 and len(set(compact.lower())) >= 8


def scan_text(path: Path, text: str) -> list[tuple[int, str]]:
    findings: list[tuple[int, str]] = []
    for rule, pattern in TOKEN_RULES.items():
        findings.extend((line_number(text, match.start()), rule) for match in pattern.finditer(text))
    for match in URI_RE.finditer(text):
        if credential_uri_finding(match.group(0)):
            findings.append((line_number(text, match.start()), "credentialed-dsn"))
    for match in NAMED_SECRET_RE.finditer(text):
        if named_secret_finding(match.group("name"), match.group("value")):
            findings.append((line_number(text, match.start()), "named-secret"))

    if path.suffix.lower() in CONFIG_DOC_EXTS:
        findings.extend((line_number(text, m.start()), "private-endpoint") for m in PRIVATE_IP_RE.finditer(text))
        findings.extend((line_number(text, m.start()), "enterprise-id") for m in WEWORK_CORP_RE.finditer(text))
        findings.extend((line_number(text, m.start()), "local-user-path") for m in USER_HOME_RE.finditer(text))
    return findings


def scan() -> list[tuple[str, int, str]]:
    findings: list[tuple[str, int, str]] = []
    for path in git_candidates():
        if not path.is_file():
            continue
        rel = relative(path)
        parts = {part.lower() for part in Path(rel).parts}
        if parts & LOCAL_PATH_PARTS:
            findings.append((rel, 0, "local-runtime-path"))
            continue
        allowed_asset = rel in ASSET_ALLOWLIST
        if path.suffix.lower() in BINARY_EXTS:
            if not allowed_asset:
                findings.append((rel, 0, "binary-or-runtime-artifact"))
            continue
        data = path.read_bytes()
        if len(data) > MAX_TEXT_BYTES or b"\0" in data:
            findings.append((rel, 0, "unreviewed-binary-or-large-file"))
            continue
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            findings.append((rel, 0, "non-utf8-file"))
            continue
        findings.extend((rel, line, rule) for line, rule in scan_text(path, text))
    return sorted(set(findings))


def self_check() -> None:
    assert is_placeholder("<API_KEY>")
    assert is_placeholder("${DATABASE_URL}")
    assert not is_placeholder("actual-value-123")
    token = "sk-" + "A1b2" * 6
    assert any(rule == "openai-compatible-key" for _, rule in scan_text(Path("x.md"), token))
    dangerous = "postgres://svc:" + "long-real-pass" + "@db.internal/app"
    harmless = "postgres://u:" + "p" + "@127.0.0.1/test"
    assert credential_uri_finding(dangerous)
    assert not credential_uri_finding(harmless)


def main() -> int:
    self_check()
    findings = scan()
    if not findings:
        print("secret scan: clean")
        return 0
    print(f"secret scan: {len(findings)} finding(s)", file=sys.stderr)
    for path, line, rule in findings:
        location = f"{path}:{line}" if line else path
        print(f"{location}: {rule}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
