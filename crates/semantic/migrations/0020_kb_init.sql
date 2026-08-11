-- 知识库 schema（owner = dms_knowledge；放这里是为了「单 migrator」——两个 sqlx::migrate! 共用
-- 同一张 _sqlx_migrations 表会互相判 VersionMissing 而启动失败）。
-- 迁移号 0020 起属轨 B（契约 B2：轨 A 占 0001-0019）。
-- 幂等：按存在性补结构，并把旧数据按结构真相重新收敛；可重复执行。

CREATE SCHEMA IF NOT EXISTS kb;

-- 知识空间：v1 只有「个人空间」（space_id = 登录名，上传时自动建）。
-- 共享空间留给 K6 管理面。
CREATE TABLE IF NOT EXISTS kb.space(
  space_id    text PRIMARY KEY,
  name        text NOT NULL DEFAULT '',
  owner       text NOT NULL,
  visibility  text NOT NULL DEFAULT 'private',   -- private | role | public
  created_at  timestamptz NOT NULL DEFAULT now()
);

-- 空间内目录树。path 是可读路径快照，parent_id 是结构真相；目录名禁止包含路径分隔符，
-- 由 knowledge 层统一校验。两个唯一索引分别锁住同级重名与路径漂移。
CREATE TABLE IF NOT EXISTS kb.folder(
  folder_id   text PRIMARY KEY,
  space_id    text NOT NULL REFERENCES kb.space(space_id) ON DELETE CASCADE,
  parent_id   text,
  name        text NOT NULL,
  path        text NOT NULL,
  created_by  text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE(space_id, folder_id)
);
-- 早期试运行版曾生成单列 parent FK；正式约束必须同时锁住 space_id。
ALTER TABLE kb.folder DROP CONSTRAINT IF EXISTS folder_parent_id_fkey;
ALTER TABLE kb.folder DROP CONSTRAINT IF EXISTS kb_folder_parent_id_fkey;

-- 旧库先把失效/跨空间父级收回根层，再校验名称与环。遇到环直接拒绝启动，禁止
-- 静默改写用户目录结构；正常旧树随后按 parent_id（结构真相）重算全部路径。
UPDATE kb.folder f SET parent_id=NULL,updated_at=now()
WHERE f.parent_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM kb.folder p
  WHERE p.space_id=f.space_id AND p.folder_id=f.parent_id
);
DO $$ BEGIN
  IF EXISTS (
    SELECT 1 FROM kb.folder
    WHERE name IS NULL OR name<>btrim(name) OR name IN ('','.','..')
       OR char_length(name)>100 OR position('/' in name)>0 OR position(chr(92) in name)>0
       OR name ~ '[[:cntrl:]]'
  ) THEN
    RAISE EXCEPTION 'invalid legacy knowledge-base folder name';
  END IF;
  IF EXISTS (
    WITH RECURSIVE ancestry(space_id,start_id,folder_id,parent_id,seen,cycle) AS (
      SELECT f.space_id,f.folder_id,f.folder_id,f.parent_id,ARRAY[f.folder_id]::text[],false
      FROM kb.folder f
      UNION ALL
      SELECT a.space_id,a.start_id,p.folder_id,p.parent_id,a.seen||p.folder_id,
             p.folder_id=ANY(a.seen)
      FROM ancestry a JOIN kb.folder p
        ON p.space_id=a.space_id AND p.folder_id=a.parent_id
      WHERE a.parent_id IS NOT NULL AND NOT a.cycle
    ) SELECT 1 FROM ancestry WHERE cycle
  ) THEN
    RAISE EXCEPTION 'legacy knowledge-base folder cycle';
  END IF;
END $$;
WITH RECURSIVE tree(folder_id,space_id,path) AS (
  SELECT f.folder_id,f.space_id,'/'||f.name FROM kb.folder f WHERE f.parent_id IS NULL
  UNION ALL
  SELECT f.folder_id,f.space_id,t.path||'/'||f.name
  FROM kb.folder f JOIN tree t ON t.space_id=f.space_id AND t.folder_id=f.parent_id
), normalized AS (
  SELECT folder_id,space_id,path FROM tree
)
UPDATE kb.folder f SET path=n.path,
  updated_at=CASE WHEN f.path IS DISTINCT FROM n.path THEN now() ELSE f.updated_at END
FROM normalized n WHERE n.space_id=f.space_id AND n.folder_id=f.folder_id;
DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM kb.folder WHERE path IS NULL OR path='/' OR path NOT LIKE '/%' OR char_length(path)>1000)
     OR (SELECT count(*) FROM kb.folder)<>(SELECT count(*) FROM kb.folder WHERE path IS NOT NULL AND char_length(path)<=1000) THEN
    RAISE EXCEPTION 'legacy knowledge-base folder path is invalid or too long';
  END IF;
END $$;
CREATE UNIQUE INDEX IF NOT EXISTS uq_kb_folder_sibling_name
  ON kb.folder(space_id, COALESCE(parent_id, ''), lower(name));
CREATE UNIQUE INDEX IF NOT EXISTS uq_kb_folder_path ON kb.folder(space_id, path);
CREATE INDEX IF NOT EXISTS idx_kb_folder_parent ON kb.folder(space_id, parent_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS uq_kb_folder_space_id ON kb.folder(space_id, folder_id);
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.folder'::regclass AND conname='fk_kb_folder_parent_space') THEN
    ALTER TABLE kb.folder ADD CONSTRAINT fk_kb_folder_parent_space
      FOREIGN KEY(space_id,parent_id) REFERENCES kb.folder(space_id,folder_id)
      ON DELETE RESTRICT NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.folder VALIDATE CONSTRAINT fk_kb_folder_parent_space;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.folder'::regclass AND conname='ck_kb_folder_name') THEN
    ALTER TABLE kb.folder ADD CONSTRAINT ck_kb_folder_name CHECK (
      name=btrim(name) AND name NOT IN ('','.','..') AND char_length(name)<=100
      AND position('/' in name)=0 AND position(chr(92) in name)=0 AND name !~ '[[:cntrl:]]'
    ) NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.folder VALIDATE CONSTRAINT ck_kb_folder_name;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.folder'::regclass AND conname='ck_kb_folder_shape') THEN
    ALTER TABLE kb.folder ADD CONSTRAINT ck_kb_folder_shape CHECK (
      parent_id IS DISTINCT FROM folder_id AND path LIKE '/%' AND path<>'/' AND char_length(path)<=1000
    ) NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.folder VALIDATE CONSTRAINT ck_kb_folder_shape;

CREATE TABLE IF NOT EXISTS kb.doc(
  doc_id      text PRIMARY KEY,                  -- uuid，同时是磁盘文件名（原名只入库，防路径穿越）
  space_id    text NOT NULL REFERENCES kb.space(space_id) ON DELETE CASCADE,
  folder_id   text,
  folder_path text NOT NULL DEFAULT '/',         -- 上传/移动时的目录路径快照
  name        text NOT NULL,                     -- 上传时的原始文件名（仅展示）
  mime        text NOT NULL DEFAULT '',
  bytes       bigint NOT NULL DEFAULT 0,
  sha256      text NOT NULL,
  status      text NOT NULL DEFAULT 'pending',   -- pending|parsing|chunked|embedded|failed
  enabled     boolean NOT NULL DEFAULT true,     -- false=保留原文但不参与检索（历史/废止版本）
  tags        text[] NOT NULL DEFAULT '{}',      -- 治理标签（主题/部门/用途等）
  business_domain text,                          -- 所属业务域
  effective_from date,                           -- 生效日期（含）
  effective_to date,                             -- 失效日期（含）
  source_uri  text,                              -- 原始来源地址/外部系统引用
  document_family text,                         -- 同一制度/手册的版本族标识
  document_revision text,                       -- 业务版本号，例如 v2.1 / 2026-08
  error       text NOT NULL DEFAULT '',
  notice      text NOT NULL DEFAULT '',          -- OCR/部分解析/表格降级等非失败提示
  page_count  int NOT NULL DEFAULT 0,
  chunk_count int NOT NULL DEFAULT 0,
  uploaded_by text NOT NULL,
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now(),
  UNIQUE(space_id, sha256),                      -- 同空间内同文件不重复入库
  UNIQUE(space_id, doc_id)
);
CREATE INDEX IF NOT EXISTS idx_kb_doc_space ON kb.doc(space_id, created_at DESC);
-- 旧库幂等补列：CREATE TABLE IF NOT EXISTS 不会给已存在的 kb.doc 增列。
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS notice text NOT NULL DEFAULT '';
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS enabled boolean NOT NULL DEFAULT true;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS tags text[] NOT NULL DEFAULT '{}';
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS business_domain text;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS effective_from date;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS effective_to date;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS source_uri text;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS document_family text;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS document_revision text;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS folder_id text;
ALTER TABLE kb.doc ADD COLUMN IF NOT EXISTS folder_path text NOT NULL DEFAULT '/';
CREATE UNIQUE INDEX IF NOT EXISTS uq_kb_doc_space_id ON kb.doc(space_id, doc_id);
CREATE INDEX IF NOT EXISTS idx_kb_doc_name_trgm ON kb.doc USING gin (name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_kb_doc_folder ON kb.doc(space_id, folder_id, created_at DESC);
ALTER TABLE kb.doc DROP CONSTRAINT IF EXISTS doc_folder_id_fkey;
ALTER TABLE kb.doc DROP CONSTRAINT IF EXISTS kb_doc_folder_id_fkey;
UPDATE kb.doc d SET folder_id=NULL,folder_path='/',updated_at=now()
WHERE d.folder_id IS NOT NULL AND NOT EXISTS (
  SELECT 1 FROM kb.folder f WHERE f.space_id=d.space_id AND f.folder_id=d.folder_id
);
UPDATE kb.doc d SET folder_path=f.path,updated_at=now()
FROM kb.folder f
WHERE f.space_id=d.space_id AND f.folder_id=d.folder_id AND d.folder_path IS DISTINCT FROM f.path;
UPDATE kb.doc SET folder_path='/',updated_at=now()
WHERE folder_id IS NULL AND folder_path IS DISTINCT FROM '/';
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.doc'::regclass AND conname='fk_kb_doc_folder_space') THEN
    ALTER TABLE kb.doc ADD CONSTRAINT fk_kb_doc_folder_space
      FOREIGN KEY(space_id,folder_id) REFERENCES kb.folder(space_id,folder_id)
      ON DELETE RESTRICT NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.doc VALIDATE CONSTRAINT fk_kb_doc_folder_space;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.doc'::regclass AND conname='ck_kb_doc_folder_path') THEN
    ALTER TABLE kb.doc ADD CONSTRAINT ck_kb_doc_folder_path CHECK (
      (folder_id IS NULL AND folder_path='/') OR
      (folder_id IS NOT NULL AND folder_path LIKE '/%' AND folder_path<>'/' AND char_length(folder_path)<=1000)
    ) NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.doc VALIDATE CONSTRAINT ck_kb_doc_folder_path;

-- 显式文档引用。source -> target 表示 source 的内容/治理元数据明确依赖 target；
-- 检索与答案侧仍会重新套 visible_docs ACL，表本身不构成可见性授权。
CREATE TABLE IF NOT EXISTS kb.doc_link(
  space_id     text NOT NULL,
  source_doc_id text NOT NULL,
  target_doc_id text NOT NULL,
  kind          text NOT NULL DEFAULT 'reference',
  created_by    text NOT NULL,
  created_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY(source_doc_id, target_doc_id, kind)
);
ALTER TABLE kb.doc_link ADD COLUMN IF NOT EXISTS space_id text;
DELETE FROM kb.doc_link l WHERE l.source_doc_id=l.target_doc_id
  OR NOT EXISTS (SELECT 1 FROM kb.doc s WHERE s.doc_id=l.source_doc_id)
  OR NOT EXISTS (SELECT 1 FROM kb.doc t WHERE t.doc_id=l.target_doc_id);
UPDATE kb.doc_link l SET space_id=d.space_id FROM kb.doc d
WHERE d.doc_id=l.source_doc_id AND l.space_id IS DISTINCT FROM d.space_id;
DELETE FROM kb.doc_link l USING kb.doc s,kb.doc t
WHERE s.doc_id=l.source_doc_id AND t.doc_id=l.target_doc_id AND s.space_id IS DISTINCT FROM t.space_id;
ALTER TABLE kb.doc_link ALTER COLUMN space_id SET NOT NULL;
CREATE INDEX IF NOT EXISTS idx_kb_doc_link_target ON kb.doc_link(target_doc_id, source_doc_id);
ALTER TABLE kb.doc_link DROP CONSTRAINT IF EXISTS doc_link_source_doc_id_fkey;
ALTER TABLE kb.doc_link DROP CONSTRAINT IF EXISTS doc_link_target_doc_id_fkey;
ALTER TABLE kb.doc_link DROP CONSTRAINT IF EXISTS kb_doc_link_source_doc_id_fkey;
ALTER TABLE kb.doc_link DROP CONSTRAINT IF EXISTS kb_doc_link_target_doc_id_fkey;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.doc_link'::regclass AND conname='fk_kb_link_source_space') THEN
    ALTER TABLE kb.doc_link ADD CONSTRAINT fk_kb_link_source_space
      FOREIGN KEY(space_id,source_doc_id) REFERENCES kb.doc(space_id,doc_id)
      ON DELETE CASCADE NOT VALID;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.doc_link'::regclass AND conname='fk_kb_link_target_space') THEN
    ALTER TABLE kb.doc_link ADD CONSTRAINT fk_kb_link_target_space
      FOREIGN KEY(space_id,target_doc_id) REFERENCES kb.doc(space_id,doc_id)
      ON DELETE CASCADE NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.doc_link VALIDATE CONSTRAINT fk_kb_link_source_space;
ALTER TABLE kb.doc_link VALIDATE CONSTRAINT fk_kb_link_target_space;
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint
    WHERE conrelid='kb.doc_link'::regclass AND conname='ck_kb_doc_link_not_self') THEN
    ALTER TABLE kb.doc_link ADD CONSTRAINT ck_kb_doc_link_not_self
      CHECK(source_doc_id<>target_doc_id) NOT VALID;
  END IF;
END $$;
ALTER TABLE kb.doc_link VALIDATE CONSTRAINT ck_kb_doc_link_not_self;

CREATE OR REPLACE FUNCTION kb.guard_folder_tree() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE parent_path text;
BEGIN
  PERFORM pg_advisory_xact_lock(hashtextextended(NEW.space_id,0));
  IF NEW.name<>btrim(NEW.name) OR NEW.name IN ('','.','..') OR char_length(NEW.name)>100
     OR position('/' in NEW.name)>0 OR position(chr(92) in NEW.name)>0
     OR NEW.name ~ '[[:cntrl:]]' THEN
    RAISE EXCEPTION 'invalid folder name';
  END IF;
  IF TG_OP='UPDATE' THEN
    IF NEW.space_id IS DISTINCT FROM OLD.space_id THEN
      RAISE EXCEPTION 'folder cannot move across spaces';
    END IF;
    IF NEW.space_id IS NOT DISTINCT FROM OLD.space_id
       AND NEW.parent_id IS NOT DISTINCT FROM OLD.parent_id
       AND NEW.name IS NOT DISTINCT FROM OLD.name THEN
      RETURN NEW;
    END IF;
  END IF;
  IF NEW.parent_id IS NULL THEN
    NEW.path := '/' || NEW.name;
  ELSE
    SELECT path INTO parent_path FROM kb.folder
    WHERE space_id=NEW.space_id AND folder_id=NEW.parent_id;
    IF parent_path IS NULL THEN RAISE EXCEPTION 'invalid folder parent'; END IF;
    IF EXISTS (
      WITH RECURSIVE ancestors(id,parent_id,seen,cycle) AS (
        SELECT f.folder_id,f.parent_id,ARRAY[f.folder_id]::text[],f.folder_id=NEW.folder_id
        FROM kb.folder f WHERE f.space_id=NEW.space_id AND f.folder_id=NEW.parent_id
        UNION ALL
        SELECT f.folder_id,f.parent_id,a.seen||f.folder_id,
               f.folder_id=NEW.folder_id OR f.folder_id=ANY(a.seen)
        FROM kb.folder f JOIN ancestors a ON f.folder_id=a.parent_id
        WHERE f.space_id=NEW.space_id AND NOT a.cycle
      ) SELECT 1 FROM ancestors WHERE cycle
    ) THEN RAISE EXCEPTION 'folder cycle'; END IF;
    NEW.path := parent_path || '/' || NEW.name;
  END IF;
  IF char_length(NEW.path)>1000 THEN RAISE EXCEPTION 'folder path too long'; END IF;
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS trg_kb_folder_tree ON kb.folder;
CREATE TRIGGER trg_kb_folder_tree BEFORE INSERT OR UPDATE OF space_id,parent_id,name
ON kb.folder FOR EACH ROW EXECUTE FUNCTION kb.guard_folder_tree();

CREATE OR REPLACE FUNCTION kb.guard_doc_folder() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE canonical_path text;
BEGIN
  PERFORM pg_advisory_xact_lock(hashtextextended(NEW.space_id,0));
  IF TG_OP='UPDATE' AND NEW.space_id IS DISTINCT FROM OLD.space_id THEN
    RAISE EXCEPTION 'document cannot move across spaces';
  END IF;
  IF NEW.folder_id IS NULL THEN
    NEW.folder_path := '/';
  ELSE
    SELECT path INTO canonical_path FROM kb.folder
    WHERE space_id=NEW.space_id AND folder_id=NEW.folder_id;
    IF canonical_path IS NULL THEN RAISE EXCEPTION 'invalid document folder'; END IF;
    NEW.folder_path := canonical_path;
  END IF;
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS trg_kb_doc_folder ON kb.doc;
CREATE TRIGGER trg_kb_doc_folder BEFORE INSERT OR UPDATE OF space_id,folder_id
ON kb.doc FOR EACH ROW EXECUTE FUNCTION kb.guard_doc_folder();

CREATE OR REPLACE FUNCTION kb.guard_doc_link_space() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_space text; target_space text;
BEGIN
  SELECT space_id INTO source_space FROM kb.doc WHERE doc_id=NEW.source_doc_id;
  SELECT space_id INTO target_space FROM kb.doc WHERE doc_id=NEW.target_doc_id;
  IF source_space IS NULL OR target_space IS DISTINCT FROM source_space THEN
    RAISE EXCEPTION 'cross-space document link';
  END IF;
  NEW.space_id := source_space;
  PERFORM pg_advisory_xact_lock(hashtextextended(NEW.space_id,0));
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS trg_kb_doc_link_space ON kb.doc_link;
CREATE TRIGGER trg_kb_doc_link_space BEFORE INSERT OR UPDATE OF space_id,source_doc_id,target_doc_id
ON kb.doc_link FOR EACH ROW EXECUTE FUNCTION kb.guard_doc_link_space();

CREATE TABLE IF NOT EXISTS kb.chunk(
  chunk_id     bigserial PRIMARY KEY,
  doc_id       text NOT NULL REFERENCES kb.doc(doc_id) ON DELETE CASCADE,
  ord          int NOT NULL,
  text         text NOT NULL,
  heading_path text NOT NULL DEFAULT '',
  folder_path  text NOT NULL DEFAULT '/',         -- 与所属文档同步，供关系召回与诊断
  page         int,
  tokens       int NOT NULL DEFAULT 0,
  embedding_text text NOT NULL DEFAULT '',       -- 配方输入快照；正文仍以 text 为引用真相
  embedding_recipe smallint NOT NULL DEFAULT 0, -- 配方版本，升级后旧向量自动失效
  embedding    vector(512),                      -- 复用现有 bge-small-zh-v1.5，不引第二模型
  ts           tsvector GENERATED ALWAYS AS (to_tsvector('simple', text)) STORED,
  UNIQUE(doc_id, ord)
);
ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS folder_path text NOT NULL DEFAULT '/';
ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS embedding_text text NOT NULL DEFAULT '';
ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS embedding_recipe smallint NOT NULL DEFAULT 0;

-- recipe v1：文件名 + 目录路径 + 章节路径 + 正文。正文列不改写，引用仍返回原始 text。
CREATE OR REPLACE FUNCTION kb.chunk_embedding_text(
  doc_name text, folder_path text, heading_path text, body text
) RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
  SELECT '文件：' || COALESCE(doc_name, '') || E'\n目录：' ||
         COALESCE(NULLIF(folder_path, ''), '/') || E'\n章节：' ||
         COALESCE(NULLIF(heading_path, ''), '正文') || E'\n\n' || COALESCE(body, '')
$$;

-- folder_id/path 由旧库升级得到时，chunk 的目录快照也必须先追平文档，随后再计算配方文本。
UPDATE kb.chunk c SET folder_path=d.folder_path
FROM kb.doc d WHERE d.doc_id=c.doc_id AND c.folder_path IS DISTINCT FROM d.folder_path;

-- 旧库应用本迁移时退回 chunked；后台按批渐进补齐，不阻塞启动做全量向量计算。
WITH stale AS (
  UPDATE kb.chunk c SET
    embedding=NULL,
    embedding_text=kb.chunk_embedding_text(d.name,c.folder_path,c.heading_path,c.text),
    embedding_recipe=1
  FROM kb.doc d
  WHERE d.doc_id=c.doc_id AND (c.embedding_recipe<>1 OR c.embedding_text IS DISTINCT FROM
    kb.chunk_embedding_text(d.name,c.folder_path,c.heading_path,c.text))
  RETURNING c.doc_id
)
UPDATE kb.doc d SET status='chunked',updated_at=now()
WHERE d.status='embedded' AND d.doc_id IN (SELECT doc_id FROM stale);
CREATE INDEX IF NOT EXISTS idx_kb_chunk_vec ON kb.chunk USING hnsw (embedding vector_cosine_ops);
CREATE INDEX IF NOT EXISTS idx_kb_chunk_ts  ON kb.chunk USING gin (ts);
CREATE INDEX IF NOT EXISTS idx_kb_chunk_trgm ON kb.chunk USING gin (text gin_trgm_ops);
CREATE INDEX IF NOT EXISTS idx_kb_chunk_heading_trgm ON kb.chunk USING gin (heading_path gin_trgm_ops);

-- 词级稀疏召回（第 9 路，对照 Yuxi 的 BM25 半）：jieba 精确模式分词的词集合。
-- 分词只在 Rust 侧（store::terms_of 单一事实源：写入/查询/回填三处共用），PG 不引分词扩展。
-- NULL = 还没过分词器（待回填：启动任务 terms_backfill 按库内现有 text 直接重算，不要求重传），
-- {} = 分过了但没留下词（纯标点块等）。GIN 不收录 NULL 行，回填期查询自动只扫已填部分。
ALTER TABLE kb.chunk ADD COLUMN IF NOT EXISTS terms text[];
CREATE INDEX IF NOT EXISTS idx_kb_chunk_terms ON kb.chunk USING gin (terms);

-- 文档/空间/数据源的可见性与写权限。
-- perm 分 read/write：没有它连「可读不可写」都表达不了（→ 对他人知识库的投毒写）。
-- scope='ds' 的行给 K4 上传表格建出的数据源用（私有台账不该被别人 NL2SQL 查到）。
CREATE TABLE IF NOT EXISTS kb.acl(
  scope        text NOT NULL,                    -- space | doc | ds
  target_id    text NOT NULL,
  grantee_kind text NOT NULL,                    -- login | role | dept
  grantee      text NOT NULL,
  perm         text NOT NULL DEFAULT 'read',     -- read | write
  created_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY(scope, target_id, grantee_kind, grantee, perm)
);
CREATE INDEX IF NOT EXISTS idx_kb_acl_grantee ON kb.acl(grantee_kind, grantee);

-- 【share_config v2 · 部门支路】login → 部门 的 PG 侧映射。
-- 部门归属的真相在 MySQL t_employee.department_id，而可见性 SQL 全部在 PG 内求值：
-- 要让 dept 授权与 login/role 授权在同一条 SQL 里取并集（不做按行 N+1 反查），
-- 必须有一份 PG 内按键可查的映射。KB 端点每次请求按现算的 Principal 幂等刷新
-- （knowledge/acl.rs::sync_viewer_dept），无部门即删行。
-- 映射缺失或滞留只会让 dept 支路不命中/按旧值求值（fail-closed 方向），
-- 不会放宽 login/role 两路的任何既有判定。
CREATE TABLE IF NOT EXISTS kb.user_dept(
  login       text PRIMARY KEY,
  dept        text NOT NULL,                 -- t_department.department_id 的字符串形，与 kb.acl.grantee 同型比较
  updated_at  timestamptz NOT NULL DEFAULT now()
);
