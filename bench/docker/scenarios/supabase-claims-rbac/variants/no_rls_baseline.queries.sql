-- name: channels
select id, name from app.channels where org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'channels.read' where m.user_id = '{{subject}}'::uuid) order by id

-- name: recent
select id from app.messages where org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid) order by created_at desc, id desc limit 20

-- name: count
select count(*) from app.messages where org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid)

-- name: join
select count(*) from app.messages m join app.channels c on c.id = m.channel_id where m.org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid) and c.org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'channels.read' where m.user_id = '{{subject}}'::uuid)

-- name: join_page
select m.id, c.name from app.messages m join app.channels c on c.id = m.channel_id where m.org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid) and c.org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'channels.read' where m.user_id = '{{subject}}'::uuid) order by m.created_at desc, m.id desc limit 20

-- name: feed_a
select id from app.messages where channel_id = 4 and org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid) order by created_at desc, id desc limit 20

-- name: feed_b
select id from app.messages where channel_id = 62 and org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid) order by created_at desc, id desc limit 20

-- name: by_org
select count(*) from app.messages where org_id = 63 and org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid)

-- name: pk
select * from app.messages where id = 4242 and org_id in (select m.org_id from app.org_members m join app.role_permissions rp on rp.role = m.role and rp.permission = 'messages.read' where m.user_id = '{{subject}}'::uuid)
