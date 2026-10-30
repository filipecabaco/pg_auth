//! Catalog lookups and expression analysis: plain values (OIDs) or handles in, owned data out.
//! Postgres raises its errors as pgrx panics, which the callback's guard turns back into errors.

use super::{Node, Oid, Ptr, pg_sys};
use std::ffi::{CStr, CString, c_int, c_void};

/// A C string Postgres returned (palloc'd or in a cache), copied; None for null.
fn owned(raw: *const std::ffi::c_char) -> Option<String> {
    // SAFETY: Postgres returns null or a NUL-terminated string, copied at once
    (!raw.is_null()).then(|| unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned())
}

/// The schema named `name`, if it exists.
pub fn namespace(name: &CStr) -> Option<Oid> {
    // SAFETY: a syscache lookup by name; missing_ok, so no error when it does not exist
    let oid = unsafe { pg_sys::get_namespace_oid(name.as_ptr(), true) };
    (oid != pg_sys::InvalidOid).then_some(oid)
}

/// Whether function `funcid` belongs to the extension `name`, installed in this database (made by
/// its script, not one of the same name a user wrote).
pub fn function_of_extension(funcid: Oid, name: &CStr) -> bool {
    // SAFETY: catalog lookups by name and by object; missing_ok, so no error when not installed
    unsafe {
        let ext = pg_sys::get_extension_oid(name.as_ptr(), true);
        ext != pg_sys::InvalidOid && pg_sys::getExtensionOfObject(pg_sys::ProcedureRelationId, funcid) == ext
    }
}

/// `schema.table`, as `authz.explain` shows it.
pub fn relation_name(relid: Oid) -> String {
    // SAFETY: syscache lookups of an existing relation and its schema
    let (schema, name) = unsafe { (pg_sys::get_namespace_name(pg_sys::get_rel_namespace(relid)), pg_sys::get_rel_name(relid)) };
    format!("{}.{}", owned(schema).unwrap_or_default(), owned(name).unwrap_or_default())
}

/// `schema.table` quoted where needed, for SQL text.
pub fn relation_sql(relid: Oid) -> String {
    // SAFETY: as relation_name; quote_qualified_identifier returns a new string
    owned(unsafe {
        pg_sys::quote_qualified_identifier(pg_sys::get_namespace_name(pg_sys::get_rel_namespace(relid)), pg_sys::get_rel_name(relid))
    })
    .unwrap_or_default()
}

pub fn attribute_name(relid: Oid, attno: i16) -> Option<String> {
    // SAFETY: a syscache lookup; missing_ok
    owned(unsafe { pg_sys::get_attname(relid, attno, true) })
}

pub fn function_name(funcid: Oid) -> Option<String> {
    // SAFETY: a syscache lookup (null when missing)
    owned(unsafe { pg_sys::get_func_name(funcid) })
}

pub fn function_namespace(funcid: Oid) -> Oid {
    // SAFETY: a syscache lookup
    unsafe { pg_sys::get_func_namespace(funcid) }
}

pub fn function_result_type(funcid: Oid) -> Oid {
    // SAFETY: a syscache lookup
    unsafe { pg_sys::get_func_rettype(funcid) }
}

/// The function `schema.name(argtypes)`, if it exists.
pub fn function(schema: &str, name: &str, argtypes: &[Oid]) -> Option<Oid> {
    let (schema, name) = (CString::new(schema).ok()?, CString::new(name).ok()?);
    let nargs = c_int::try_from(argtypes.len()).ok()?;
    // SAFETY: the qualified name is a list of String nodes in the current context; the lookup
    // reads `nargs` argument types and is missing_ok
    let oid = unsafe {
        let names = pg_sys::lappend(std::ptr::null_mut(), pg_sys::makeString(pg_sys::pstrdup(schema.as_ptr())).cast());
        let names = pg_sys::lappend(names, pg_sys::makeString(pg_sys::pstrdup(name.as_ptr())).cast());
        pg_sys::LookupFuncName(names, nargs, argtypes.as_ptr(), true)
    };
    (oid != pg_sys::InvalidOid).then_some(oid)
}

pub fn operator_name(opno: Oid) -> Option<String> {
    // SAFETY: a syscache lookup (null when missing)
    owned(unsafe { pg_sys::get_opname(opno) })
}

/// The operator `name` on two `typ`s, as the search path finds it.
pub fn operator(name: &str, typ: Oid) -> Option<Oid> {
    let name = CString::new(name).ok()?;
    // SAFETY: the name is a one-element list of a String node; the lookup returns InvalidOid
    // when there is none
    let opno = unsafe {
        let names = pg_sys::lappend(std::ptr::null_mut(), pg_sys::makeString(pg_sys::pstrdup(name.as_ptr())).cast());
        pg_sys::OpernameGetOprid(names, typ, typ)
    };
    (opno != pg_sys::InvalidOid).then_some(opno)
}

/// The type's default equality operator.
pub fn equality(typ: Oid) -> Oid {
    // SAFETY: lookup_type_cache returns the type's cache entry (raising for an unknown type)
    unsafe { (*pg_sys::lookup_type_cache(typ, pg_sys::TYPECACHE_EQ_OPR.cast_signed())).eq_opr }
}

/// A hash function for `= ANY` with operator `opno` on `typ`'s values, when the elements are
/// passed by value and the operator hashes (see rewrite's `hash_by_value`).
pub fn hash_function(opno: Oid, typ: Oid) -> Option<Oid> {
    let (mut hash, mut rhs) = (pg_sys::InvalidOid, pg_sys::InvalidOid);
    // SAFETY: syscache lookups; the out-parameters are written when found
    let found = unsafe {
        pg_sys::get_typbyval(typ) && pg_sys::op_hashjoinable(opno, typ) && pg_sys::get_op_hash_functions(opno, &raw mut hash, &raw mut rhs)
    };
    found.then_some(hash)
}

/// The array type of `typ`'s elements.
pub fn array_type(typ: Oid) -> Option<Oid> {
    // SAFETY: a syscache lookup (InvalidOid when there is none)
    let array = unsafe { pg_sys::get_array_type(typ) };
    (array != pg_sys::InvalidOid).then_some(array)
}

/// The type's collation (InvalidOid for a type that is not collatable).
pub fn type_collation(typ: Oid) -> Oid {
    // SAFETY: a syscache lookup
    unsafe { pg_sys::get_typcollation(typ) }
}

pub fn collation_is_deterministic(collation: Oid) -> bool {
    // SAFETY: a syscache lookup of an existing collation
    unsafe { pg_sys::get_collation_isdeterministic(collation) }
}

/// `value` as `typ`'s output function writes it.
pub fn output(typ: Oid, value: pg_sys::Datum) -> String {
    let (mut function, mut varlena) = (pg_sys::InvalidOid, false);
    // SAFETY: the type's output function, called on a value of that type; the result is copied
    unsafe {
        pg_sys::getTypeOutputInfo(typ, &raw mut function, &raw mut varlena);
        owned(pg_sys::OidOutputFunctionCall(function, value)).unwrap_or_default()
    }
}

pub fn expr_type(node: Node<'_>) -> Oid {
    // SAFETY: a live expression node
    unsafe { pg_sys::exprType(node.as_raw()) }
}

pub fn expr_collation(node: Node<'_>) -> Oid {
    // SAFETY: a live expression node
    unsafe { pg_sys::exprCollation(node.as_raw()) }
}

pub fn is_volatile(node: Node<'_>) -> bool {
    // SAFETY: walks a live expression
    unsafe { pg_sys::contain_volatile_functions(node.as_raw()) }
}

pub fn is_mutable(node: Node<'_>) -> bool {
    // SAFETY: walks a live expression
    unsafe { pg_sys::contain_mutable_functions(node.as_raw()) }
}

pub fn has_sublink(node: Node<'_>) -> bool {
    // SAFETY: walks a live expression
    unsafe { pg_sys::checkExprHasSubLink(node.as_raw()) }
}

/// Whether the expression may leak its arguments (a function that is not leakproof).
pub fn may_leak(node: Node<'_>) -> bool {
    // SAFETY: walks a live expression
    unsafe { pg_sys::contain_leaked_vars(node.as_raw()) }
}

/// Whether the expression reads a column of this query level.
pub fn reads_this_level(node: Node<'_>) -> bool {
    // SAFETY: walks a live expression
    unsafe { pg_sys::contain_vars_of_level(node.as_raw(), 0) }
}

/// The columns (Vars) an expression reads, in the tree.
pub fn vars(node: Node<'_>) -> Vec<Ptr<'_, pg_sys::Var>> {
    // SAFETY: pull_var_clause returns a new list of pointers to the expression's own Vars
    let list = unsafe { super::List::<pg_sys::Var>::from_raw(pg_sys::pull_var_clause(node.as_raw(), 0)) };
    list.iter().collect()
}

/// The attribute numbers `nodes` read from relation `varno` (system columns negative, 0 for the
/// whole row).
pub fn attnos<'a>(nodes: impl IntoIterator<Item = Node<'a>>, varno: u32) -> Vec<i32> {
    let mut set: *mut pg_sys::Bitmapset = std::ptr::null_mut();
    let mut attnos = Vec::new();
    // SAFETY: pull_varattnos adds to a bitmapset in the current context, read member by member
    unsafe {
        for node in nodes {
            pg_sys::pull_varattnos(node.as_raw(), varno, &raw mut set);
        }
        let mut member = pg_sys::bms_next_member(set, -1);
        while member >= 0 {
            attnos.push(member + pg_sys::FirstLowInvalidHeapAttributeNumber);
            member = pg_sys::bms_next_member(set, member);
        }
    }
    attnos
}

/// The node as Postgres prints it (nodeToString): equal for equal trees.
pub fn node_text(node: Node<'_>) -> String {
    // SAFETY: prints a live node into a new string
    owned(unsafe { pg_sys::nodeToString(node.as_raw().cast::<c_void>().cast_const()) }).unwrap_or_default()
}

/// A non-null `text` constant's value.
pub fn text_value(c: Ptr<'_, pg_sys::Const>) -> Option<String> {
    if c.is_null() || c.const_type() != pg_sys::TEXTOID {
        return None;
    }
    // SAFETY: a non-null text constant's value is a text datum, converted into a new C string
    owned(unsafe { pg_sys::text_to_cstring(c.value().cast_mut_ptr()) })
}

/// A non-null `unknown` constant's value (a string literal where a parameter takes "any").
pub fn unknown_value(c: Ptr<'_, pg_sys::Const>) -> Option<String> {
    if c.is_null() || c.const_type() != pg_sys::UNKNOWNOID {
        return None;
    }
    // SAFETY: an unknown constant's value is a C string
    owned(c.value().cast_mut_ptr::<std::ffi::c_char>())
}

/// The columns of `relid` that a unique index has as its only key.
pub fn unique_columns(relid: Oid) -> Vec<i16> {
    let mut keys = Vec::new();
    // SAFETY: the relation is opened and closed here; its index list and each index's pg_index
    // row are read while held (the row's fixed part follows its header, t_hoff bytes in)
    unsafe {
        let rel = pg_sys::RelationIdGetRelation(relid);
        if rel.is_null() {
            return keys;
        }
        let list = pg_sys::RelationGetIndexList(rel);
        let n = if list.is_null() { 0 } else { usize::try_from((*list).length).unwrap_or(0) };
        for i in 0..n {
            let index = (*(*list).elements.add(i)).oid_value;
            let tuple = pg_sys::SearchSysCache1(pg_sys::SysCacheIdentifier::INDEXRELID.cast_signed(), pg_sys::Datum::from(index));
            if tuple.is_null() {
                continue;
            }
            let form = (*tuple).t_data.cast::<u8>().add(usize::from((*(*tuple).t_data).t_hoff)).cast::<pg_sys::FormData_pg_index>();
            if (*form).indisunique && (*form).indnkeyatts == 1 {
                keys.push(*(*form).indkey.values.as_ptr());
            }
            pg_sys::ReleaseSysCache(tuple);
        }
        pg_sys::RelationClose(rel);
    }
    keys
}

/// Whether `expr`, read as an expression over `relid`'s columns, is IMMUTABLE. Only parses and
/// analyses it (Postgres raises its own error when it does not fit the relation).
pub fn expression_is_immutable(relid: Oid, expr: &str) -> bool {
    let Ok(sql) = CString::new(format!("SELECT ({expr}) FROM {}", relation_sql(relid))) else { return false };
    // SAFETY: raw_parser and parse_analyze_fixedparams build the statement's trees in the current
    // context; the one target entry's expression is then walked
    unsafe {
        let raw = super::List::<pg_sys::RawStmt>::from_raw(pg_sys::raw_parser(sql.as_ptr(), pg_sys::RawParseMode::RAW_PARSE_DEFAULT));
        let (1, Some(stmt)) = (raw.len(), raw.get(0)) else { return false };
        let query = pg_sys::parse_analyze_fixedparams(stmt.as_raw(), sql.as_ptr(), std::ptr::null(), 0, std::ptr::null_mut());
        let Some(query) = Ptr::from_raw(query) else { return false };
        match query.target_list().get(0).and_then(Ptr::expr) {
            Some(expr) => !is_mutable(expr),
            None => false,
        }
    }
}

// The tree walkers: functions before 16, macros over these since
#[cfg(feature = "pg15")]
use pg_sys::{expression_tree_walker, query_tree_walker};
#[cfg(not(feature = "pg15"))]
use pg_sys::{expression_tree_walker_impl as expression_tree_walker, query_tree_walker_impl as query_tree_walker};

/// Every query nested in `query`'s expressions (sub-selects: `IN (SELECT ...)`, `EXISTS`, a
/// policy's), not those in FROM or WITH.
pub fn sublink_queries(query: Ptr<'_, pg_sys::Query>) -> Vec<Ptr<'_, pg_sys::Query>> {
    let mut found: Vec<*mut pg_sys::Query> = Vec::new();
    // SAFETY: query_tree_walker calls `walk` on the live query's expressions, with `found` as its
    // context for the duration of the call
    unsafe {
        query_tree_walker(
            query.as_raw(),
            Some(walk),
            (&raw mut found).cast(),
            (pg_sys::QTW_IGNORE_RT_SUBQUERIES | pg_sys::QTW_IGNORE_CTE_SUBQUERIES).cast_signed(),
        );
    }
    // SAFETY: the walker found these in the live tree
    found.into_iter().filter_map(|q| unsafe { Ptr::from_raw(q) }).collect()
}

#[pgrx::pg_guard]
unsafe extern "C-unwind" fn walk(node: *mut pg_sys::Node, context: *mut c_void) -> bool {
    // SAFETY: called by the expression walker with a node of the tree (or null) and the context
    // sublink_queries passed, a Vec it owns for the walk
    unsafe {
        if node.is_null() {
            return false;
        }
        if (*node).type_ == pg_sys::NodeTag::T_Query {
            (*context.cast::<Vec<*mut pg_sys::Query>>()).push(node.cast());
            return false;
        }
        expression_tree_walker(node, Some(walk), context)
    }
}
