CREATE TABLE IF NOT EXISTS projects (
 id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS packages (
 id BIGSERIAL PRIMARY KEY, project_id BIGINT NOT NULL REFERENCES projects(id),
 name TEXT NOT NULL, catalog_name TEXT, required_codes_json TEXT NOT NULL DEFAULT '[]',
 UNIQUE(project_id,name)
);
CREATE TABLE IF NOT EXISTS documents (
 id BIGSERIAL PRIMARY KEY, package_id BIGINT NOT NULL REFERENCES packages(id),
 relative_path TEXT NOT NULL, source_kind TEXT NOT NULL CHECK(source_kind IN ('ID','REFERENCE')),
 origin TEXT NOT NULL CHECK(origin IN ('synology','upload')),
 is_present BOOLEAN NOT NULL DEFAULT TRUE, sha256 TEXT NOT NULL, version INTEGER NOT NULL,
 archive_path TEXT NOT NULL, draft_path TEXT, document_type TEXT, catalog_code TEXT,
 recognized_text TEXT NOT NULL DEFAULT '', recognition_status TEXT NOT NULL DEFAULT 'pending',
 recognition_confidence REAL, spelling_notes_json TEXT NOT NULL DEFAULT '[]',
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 UNIQUE(package_id, relative_path, version)
);
CREATE INDEX IF NOT EXISTS doc_pkg_path_idx ON documents(package_id, relative_path, id DESC);
CREATE TABLE IF NOT EXISTS check_runs (
 id BIGSERIAL PRIMARY KEY, package_id BIGINT NOT NULL REFERENCES packages(id),
 started_at TIMESTAMPTZ NOT NULL DEFAULT now(), rule_version INTEGER NOT NULL DEFAULT 1,
 source TEXT NOT NULL DEFAULT 'synology'
);
CREATE TABLE IF NOT EXISTS findings (
 id BIGSERIAL PRIMARY KEY, run_id BIGINT NOT NULL REFERENCES check_runs(id),
 package_id BIGINT NOT NULL REFERENCES packages(id), document_id BIGINT REFERENCES documents(id),
 block INTEGER NOT NULL, rule_code TEXT NOT NULL, description TEXT NOT NULL,
 source_location TEXT NOT NULL DEFAULT '', reference TEXT NOT NULL DEFAULT '',
 ntd TEXT NOT NULL DEFAULT '', severity TEXT NOT NULL DEFAULT 'review',
 result TEXT NOT NULL CHECK(result IN ('mismatch','manual')),
 review_status TEXT NOT NULL DEFAULT 'pending' CHECK(review_status IN ('pending','confirmed','rejected','resolved')),
 reviewed_by TEXT, reviewed_at TIMESTAMPTZ, comment TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS finding_run_idx ON findings(run_id,id);
CREATE TABLE IF NOT EXISTS audit (
 id BIGSERIAL PRIMARY KEY, timestamp TIMESTAMPTZ NOT NULL DEFAULT now(),
 actor TEXT NOT NULL, action TEXT NOT NULL, details TEXT NOT NULL DEFAULT ''
);
