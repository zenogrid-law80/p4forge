ALTER TABLE agents DROP CONSTRAINT agents_os_check;
ALTER TABLE agents ADD CONSTRAINT agents_os_check CHECK (os IN ('linux','windows','macos'));
