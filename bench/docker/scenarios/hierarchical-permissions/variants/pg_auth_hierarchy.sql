-- @set: pg_auth.sidecar_socket=/run/pg_auth/default.sock
-- pg_auth with the folder tree declared (sql/hierarchy.sql): OpenFGA lists only the subject's
-- direct grants (viewer_direct); Postgres expands them down a closure of app.folders that
-- triggers keep current. A broad subject's expansion becomes its index set, with no fill.
select authz_internal.protect('app.files', 'folder', 'folder_id', 'viewer',
                     hierarchy => 'app.folders(id, parent_id)', direct => 'viewer_direct');
