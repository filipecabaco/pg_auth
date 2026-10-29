-- Catalog naive policy: correlated recursive CTE (walk the ancestors of the file's folder) per file.
alter table app.files enable row level security;
create policy naive on app.files for select to bench_user using (exists (
  with recursive anc(id) as (
    select files.folder_id
    union all select f.parent_id from app.folders f join anc on f.id = anc.id where f.parent_id is not null)
  select 1 from anc join app.folder_grants g on g.folder_id = anc.id and g.user_id = (select app.uid())));
