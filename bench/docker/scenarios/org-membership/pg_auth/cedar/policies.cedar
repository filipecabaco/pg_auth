// the ids under the member (its orgs; their documents), listed when they are few, else each row
// checked by Cedar with its parents
permit (principal, action == Action::"read", resource) when { resource in principal };
