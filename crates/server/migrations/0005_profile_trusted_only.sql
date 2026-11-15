-- Whether only environments trusted with their owner's credentials are
-- given a file: a setting of the file's, which the credentials Hangar knows
-- of start with.
ALTER TABLE profile_files ADD COLUMN trusted_only boolean NOT NULL DEFAULT false;

UPDATE profile_files SET trusted_only = true WHERE path IN (
    '.claude/.credentials.json', '.git-credentials', '.netrc', '.config/gh/hosts.yml',
    '.config/glab-cli/config.yml', '.docker/config.json', '.npmrc', '.pypirc', '.aws/credentials',
    '.kube/config', '.config/gcloud/application_default_credentials.json');
