//! New nodes, allocated in the memory context current during the callback (see `Memory`). Each
//! takes live nodes of the same tree and returns a handle on what it made.

use super::{List, Memory, Node, Oid, Ptr, pg_sys};
use pgrx::pg_sys::NodeTag;
use std::ffi::CString;

#[expect(clippy::unused_self, reason = "Memory is a token: holding it proves the callback's memory context is current")]
impl<'a> Memory<'a> {
    /// A zeroed node of type `T` carrying `tag`.
    fn node<T>(self, tag: NodeTag) -> Ptr<'a, T> {
        // SAFETY: palloc0 returns zeroed memory of the struct's size in the current context (it
        // raises rather than return null), a valid T once its tag is set: Postgres's makeNode
        unsafe {
            let raw = pg_sys::palloc0(std::mem::size_of::<T>()).cast::<pg_sys::Node>();
            (*raw).type_ = tag;
            Ptr::from_raw(raw.cast::<T>()).unwrap_or_else(|| unreachable!("palloc0 never returns null"))
        }
    }

    /// A list of these nodes.
    pub fn list<T>(self, items: impl IntoIterator<Item = Ptr<'a, T>>) -> List<'a, T> {
        let mut raw = std::ptr::null_mut();
        for item in items {
            // SAFETY: lappend allocates in the current context and keeps the pointer as is
            raw = unsafe { pg_sys::lappend(raw, item.as_raw().cast()) };
        }
        // SAFETY: a list of live Ts just built
        unsafe { List::from_raw(raw) }
    }

    /// A copy of `node` (copyObject).
    pub fn copy(self, node: Node<'a>) -> Node<'a> {
        // SAFETY: copies a live node into the current context
        unsafe { Ptr::from_raw(pg_sys::copyObjectImpl(node.as_raw().cast()).cast()) }.unwrap_or(node)
    }

    /// `true`, `false`, or (`isnull`) a boolean NULL: unknown.
    pub fn bool_const(self, value: bool, isnull: bool) -> Node<'a> {
        // SAFETY: makeBoolConst allocates a Const in the current context
        self.made(unsafe { pg_sys::makeBoolConst(value, isnull) })
    }

    /// A `text` constant; None when `s` holds a NUL.
    pub fn text_const(self, s: &str) -> Option<Node<'a>> {
        let s = CString::new(s).ok()?;
        // SAFETY: cstring_to_text copies the string into a text in the current context, which
        // the Const holds (by reference, typlen -1)
        let made = unsafe {
            let datum = pg_sys::Datum::from(pg_sys::cstring_to_text(s.as_ptr()));
            pg_sys::makeConst(pg_sys::TEXTOID, -1, pg_sys::DEFAULT_COLLATION_OID, -1, datum, false, false)
        };
        Some(self.made(made))
    }

    pub fn oid_const(self, value: Oid) -> Node<'a> {
        // SAFETY: a by-value constant of oid's length
        self.made(unsafe { pg_sys::makeConst(pg_sys::OIDOID, -1, pg_sys::InvalidOid, 4, pg_sys::Datum::from(value), false, true) })
    }

    pub fn int4_const(self, value: i32) -> Node<'a> {
        // SAFETY: a by-value constant of int4's length
        self.made(unsafe { pg_sys::makeConst(pg_sys::INT4OID, -1, pg_sys::InvalidOid, 4, pg_sys::Datum::from(value), false, true) })
    }

    /// `args` joined by AND, OR, or (one argument) NOT.
    pub fn bool_expr(self, kind: pg_sys::BoolExprType::Type, args: List<'a, pg_sys::Node>) -> Node<'a> {
        // SAFETY: makeBoolExpr allocates the expression, holding the list as is
        self.made(unsafe { pg_sys::makeBoolExpr(kind, args.as_raw(), -1) })
    }

    /// `left <op> right`, a boolean operator; as `IS DISTINCT FROM` (with `op` the type's
    /// equality) when `distinct`.
    pub fn operator(self, opno: Oid, left: Node<'a>, right: Node<'a>, collation: Oid, distinct: bool) -> Node<'a> {
        let op = self.node::<pg_sys::OpExpr>(if distinct { NodeTag::T_DistinctExpr } else { NodeTag::T_OpExpr });
        let args = self.list([left, right]);
        // SAFETY: a new OpExpr of this tree, its fields set as make_opclause sets them
        unsafe {
            let raw = op.as_raw();
            (*raw).opno = opno;
            (*raw).opfuncid = pg_sys::get_opcode(opno);
            (*raw).opresulttype = pg_sys::BOOLOID;
            (*raw).inputcollid = collation;
            (*raw).args = args.as_raw();
            (*raw).location = -1;
        }
        op.node()
    }

    /// `left <op> ANY (right)`, hashed with `hash` (see `catalog::hash_function`) when given.
    pub fn any(self, opno: Oid, left: Node<'a>, right: Node<'a>, collation: Oid, hash: Option<Oid>) -> Node<'a> {
        let saop = self.node::<pg_sys::ScalarArrayOpExpr>(NodeTag::T_ScalarArrayOpExpr);
        let args = self.list([left, right]);
        // SAFETY: a new ScalarArrayOpExpr of this tree
        unsafe {
            let raw = saop.as_raw();
            (*raw).opno = opno;
            (*raw).opfuncid = pg_sys::get_opcode(opno);
            (*raw).useOr = true;
            (*raw).inputcollid = collation;
            (*raw).args = args.as_raw();
            (*raw).location = -1;
            if let Some(hash) = hash {
                (*raw).hashfuncid = hash;
            }
        }
        saop.node()
    }

    /// `coalesce(clause, otherwise)`, both boolean.
    pub fn coalesce_bool(self, clause: Node<'a>, otherwise: bool) -> Node<'a> {
        let coalesce = self.node::<pg_sys::CoalesceExpr>(NodeTag::T_CoalesceExpr);
        let args = self.list([clause, self.bool_const(otherwise, false)]);
        // SAFETY: a new CoalesceExpr of this tree
        unsafe {
            let raw = coalesce.as_raw();
            (*raw).coalescetype = pg_sys::BOOLOID;
            (*raw).args = args.as_raw();
            (*raw).location = -1;
        }
        coalesce.node()
    }

    /// `expr::to`, an explicit cast from `from`; None when there is none.
    pub fn coerce(self, expr: Node<'a>, from: Oid, to: Oid) -> Option<Node<'a>> {
        if from == to {
            return Some(expr);
        }
        // SAFETY: coerce_to_target_type builds the cast over a live expression (no parse state:
        // none is needed for an explicit cast), returning null when there is none
        let raw = unsafe {
            pg_sys::coerce_to_target_type(
                std::ptr::null_mut(),
                expr.as_raw(),
                from,
                to,
                -1,
                pg_sys::CoercionContext::COERCION_EXPLICIT,
                pg_sys::CoercionForm::COERCE_EXPLICIT_CAST,
                -1,
            )
        };
        // SAFETY: a node of this tree, or null
        unsafe { Ptr::from_raw(raw) }
    }

    /// `SELECT <func>(args) AS <name>` as an analysed query, for a function returning `rettype`.
    pub fn select_call(self, funcid: Oid, rettype: Oid, args: List<'a, pg_sys::Node>, name: &str) -> Option<Ptr<'a, pg_sys::Query>> {
        let name = CString::new(name).ok()?;
        let query = self.node::<pg_sys::Query>(NodeTag::T_Query);
        // SAFETY: the call, its target entry and the query are built in the current context, as
        // the parser builds `SELECT f(...)`
        unsafe {
            let call = pg_sys::makeFuncExpr(
                funcid,
                rettype,
                args.as_raw(),
                pg_sys::get_typcollation(rettype),
                pg_sys::DEFAULT_COLLATION_OID,
                pg_sys::CoercionForm::COERCE_EXPLICIT_CALL,
            );
            let target = pg_sys::makeTargetEntry(call.cast(), 1, pg_sys::pstrdup(name.as_ptr()), false);
            let raw = query.as_raw();
            (*raw).commandType = pg_sys::CmdType::CMD_SELECT;
            (*raw).querySource = pg_sys::QuerySource::QSRC_ORIGINAL;
            (*raw).canSetTag = true;
            (*raw).jointree = pg_sys::makeFromExpr(std::ptr::null_mut(), std::ptr::null_mut());
            (*raw).targetList = pg_sys::lappend(std::ptr::null_mut(), target.cast());
            (*raw).stmt_location = -1;
        }
        Some(query)
    }

    /// `(query)` as an expression: a sub-select returning one value, planned as an InitPlan when
    /// it reads nothing of the outer query.
    pub fn sublink(self, query: Ptr<'a, pg_sys::Query>) -> Node<'a> {
        let sublink = self.node::<pg_sys::SubLink>(NodeTag::T_SubLink);
        // SAFETY: a new SubLink of this tree
        unsafe {
            let raw = sublink.as_raw();
            (*raw).subLinkType = pg_sys::SubLinkType::EXPR_SUBLINK;
            (*raw).subselect = query.as_raw().cast();
            (*raw).location = -1;
        }
        sublink.node()
    }

    /// A String node (a plan's private data).
    pub fn string(self, s: &str) -> Node<'a> {
        let s = CString::new(s.replace('\0', "")).unwrap_or_default();
        // SAFETY: pstrdup copies the string into the current context, which the node holds
        self.made(unsafe { pg_sys::makeString(pg_sys::pstrdup(s.as_ptr())) })
    }

    /// An Integer node (a plan's private data).
    pub fn integer(self, value: i32) -> Node<'a> {
        // SAFETY: makeInteger allocates the node in the current context
        self.made(unsafe { pg_sys::makeInteger(value) })
    }

    /// A handle on a node a Postgres maker function just allocated.
    fn made<T: super::NodeStruct>(self, raw: *mut T) -> Node<'a> {
        // SAFETY: the maker functions allocate a live node in the current context, never null
        unsafe { Ptr::from_raw(raw) }.unwrap_or_else(|| unreachable!("Postgres's makers raise rather than return null")).node()
    }
}
