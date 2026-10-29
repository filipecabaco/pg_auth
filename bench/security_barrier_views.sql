create or replace view app.documents_sb with (security_barrier) as
  select * from app.documents where org_id in (select org_id from app.memberships where user_id = (select app.uid()));
create or replace view app.documents_p_sb with (security_barrier) as
  select * from app.documents_p where org_id in (select org_id from app.memberships where user_id = (select app.uid()));
create or replace view app.comments_sb with (security_barrier) as
  select * from app.comments where document_id in (select id from app.documents_sb);
