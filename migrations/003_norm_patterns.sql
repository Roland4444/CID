-- Upgrade an existing 0.3 database to support checked regex requisites.
-- Old rules and results remain unchanged.
ALTER TABLE normative_rules DROP CONSTRAINT IF EXISTS normative_rules_check_kind_check;
ALTER TABLE normative_rules ADD CONSTRAINT normative_rules_check_kind_check
 CHECK (check_kind IN ('required_phrase','forbidden_phrase','required_pattern','manual'));
