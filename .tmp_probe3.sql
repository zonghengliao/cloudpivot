\pset pager off
\d schema_migrations
select * from schema_migrations order by 1 desc limit 10;
select count(*) from schema_migrations;
