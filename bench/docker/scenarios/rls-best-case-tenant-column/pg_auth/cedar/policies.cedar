// each compiled into a filter on the column: tenant_id = the token's, owner_id = the subject
permit (principal, action == Action::"read", resource is TenantRow) when { resource.tenant_id == principal.tenant_id };
permit (principal, action == Action::"read", resource is Note) when { resource.owner_id == principal };
