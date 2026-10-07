-- What of its owner's an environment is kept from is five settings of its
-- spec, each false when absent: an untrusted one is kept from their
-- sensitive files, SSH keys and registry credentials, and its editor
-- trusts nothing.
UPDATE templates SET spec = (spec - 'untrusted') ||
    '{"no_sensitive_files": true, "no_ssh_keys": true, "no_registry": true, "no_editor_trust": true}'
WHERE spec->'untrusted' = 'true';
UPDATE templates SET spec = spec - 'untrusted' WHERE spec ? 'untrusted';

UPDATE environments SET spec = (spec - 'untrusted') ||
    '{"no_sensitive_files": true, "no_ssh_keys": true, "no_registry": true, "no_editor_trust": true}'
WHERE spec->'untrusted' = 'true';
UPDATE environments SET spec = spec - 'untrusted' WHERE spec ? 'untrusted';

-- A credential in a file set is a sensitive file.
ALTER TABLE file_trust RENAME TO file_sensitivity;
ALTER TABLE file_sensitivity RENAME COLUMN trusted_only TO sensitive;
