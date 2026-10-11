-- The Code tab keeps its settings in ~/.config/hangar/, which the built-in
-- profile shares, so they follow each person to every environment.
INSERT INTO pack_paths (pack_id, path, sensitive)
SELECT id, '~/.config/hangar/', false FROM packs WHERE builtin
ON CONFLICT DO NOTHING;
