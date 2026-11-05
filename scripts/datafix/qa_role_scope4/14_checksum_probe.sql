\pset pager off
SELECT version, name, checksum, applied_via FROM schema_migrations
 WHERE version IN ('246','247','248','249') ORDER BY version;
