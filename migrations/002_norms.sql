-- Upgrade 0.3: existing documents, reports, and check runs remain untouched.
-- Normative rules are inert until explicitly approved by the responsible employee.
CREATE TABLE IF NOT EXISTS normative_rules (
    id BIGSERIAL PRIMARY KEY,
    work_type TEXT NOT NULL DEFAULT '',
    document_code TEXT NOT NULL DEFAULT '',
    norm_name TEXT NOT NULL,
    norm_edition TEXT NOT NULL,
    norm_clause TEXT NOT NULL,
    source_reference TEXT NOT NULL,
    description TEXT NOT NULL,
    check_kind TEXT NOT NULL CHECK (check_kind IN ('required_phrase','forbidden_phrase','required_pattern','manual')),
    expected_text TEXT NOT NULL DEFAULT '',
    approval_status TEXT NOT NULL DEFAULT 'draft' CHECK (approval_status IN ('draft','approved','revoked')),
    version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    approved_by TEXT,
    approved_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX IF NOT EXISTS norm_rules_version_unique ON normative_rules(work_type, document_code, norm_name, norm_clause, version);
CREATE INDEX IF NOT EXISTS norm_rules_scope_idx ON normative_rules(approval_status,work_type,document_code);
CREATE TABLE IF NOT EXISTS normative_check_results (
    id BIGSERIAL PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES check_runs(id),
    package_id BIGINT NOT NULL REFERENCES packages(id),
    document_id BIGINT NOT NULL REFERENCES documents(id),
    rule_id BIGINT NOT NULL REFERENCES normative_rules(id),
    rule_version INTEGER NOT NULL,
    result TEXT NOT NULL CHECK (result IN ('evidence_found','mismatch','manual')),
    details TEXT NOT NULL,
    normative_basis TEXT NOT NULL,
    source_reference TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(run_id, document_id, rule_id)
);
CREATE INDEX IF NOT EXISTS norm_check_run_idx ON normative_check_results(run_id, id);
