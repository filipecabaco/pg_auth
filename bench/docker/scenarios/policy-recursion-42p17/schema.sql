create schema app;
create function app.uid() returns int language sql stable as $f$ select current_setting('request.uid')::int $f$;
create table app.memberships(user_id int, org_id int, primary key (user_id, org_id));
insert into app.memberships select u, 1 + (u * 7) % 10 from generate_series(1, 100) u;
insert into app.memberships select u, 1 + (u * 3 + 1) % 10 from generate_series(1, 100) u on conflict do nothing;
create index on app.memberships(org_id);
create table app.docs(id int primary key, org_id int not null, title text not null);
insert into app.docs select i, 1 + (i % 10), 'doc ' || i from generate_series(1, :scale) i;
create index on app.docs(org_id);
-- cross-table cycle material: an invite table whose policy reads docs, while docs' policy (in the
-- cycle variant) reads invites
create table app.invites(id int primary key, doc_id int not null, user_id int not null);
insert into app.invites select i, i, 1 + (i % 100) from generate_series(1, 200) i;
