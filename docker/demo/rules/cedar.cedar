// The demo's policies in Cedar, stored as the rule named `documents` beside the schema. pg_auth
// turns each into a filter on the table: `principal in resource.org` becomes `org_id IN (the
// subject's orgs)`, `principal in resource` the ids of the documents shared with them.

// an org's members read its memberships; its admins, who are members too, change them
permit (principal, action == Action::"read", resource is Org) when { principal in resource };
permit (principal, action == Action::"manage", resource is OrgAdmins) when { principal in resource };

// a document is read by its org's members and by whoever it is shared with
permit (principal, action == Action::"view", resource is Document) when { principal in resource.org };
permit (principal, action == Action::"view", resource is Document) when { principal in resource };

// and written, or shared, by its org's admins
permit (principal, action == Action::"edit", resource is Document) when { principal in resource.admins };
