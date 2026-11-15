-- Each person's choice of the UI's colours: their system's, or always
-- light, or always dark.
ALTER TABLE users ADD COLUMN theme text NOT NULL DEFAULT 'system'
    CHECK (theme IN ('system', 'light', 'dark'));
