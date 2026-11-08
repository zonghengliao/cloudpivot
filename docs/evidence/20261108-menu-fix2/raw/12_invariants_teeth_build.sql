-- teeth_build.sql —— 造一个最小可跑的 sys_menu（只为验证 ⑥⑦ 的牙齿，不碰任何业务库）
drop table if exists sys_menu;
create table sys_menu (
  id          uuid primary key,
  name        text,
  title       text,
  menu_type   integer,
  path        text,
  parent_id   uuid,
  sort_order  integer,
  deleted_at  timestamptz
);

-- 一个目录（menu_type=1）+ 一个页面（menu_type=2）挂在它下面，实现 ①–④ 与 ⑥⑦ 的基线全绿
insert into sys_menu (id, name, title, menu_type, path, parent_id, sort_order)
values ('11111111-1111-1111-1111-111111111111', '演示目录', '演示目录', 1, '/business/demo', null, 10);
insert into sys_menu (id, name, title, menu_type, path, parent_id, sort_order)
values ('11111111-1111-1111-1111-111111111112', '演示页面', '演示页面', 2, '/business/demo/page',
        '11111111-1111-1111-1111-111111111111', 10);

-- 补 400 条按钮（menu_type=3），让 ⑤ 菜单总数落在 [400,460]
insert into sys_menu (id, name, title, menu_type, path, parent_id, sort_order)
select gen_random_uuid(), 'btn' || g, '按钮' || g, 3, '/business/demo/btn/' || g,
       '11111111-1111-1111-1111-111111111112', g
  from generate_series(1, 400) g;

select count(*) as total from sys_menu where deleted_at is null;
