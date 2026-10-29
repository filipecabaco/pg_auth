//! Safe access to Postgres's trees: with the C callbacks' entry points (`hooks`) and the
//! executor's per-row loops (`exec`), the one place pg_auth uses `unsafe`.
//!
//! A handle (`Ptr<'a, T>`, `Node<'a>`, `List<'a, T>`) points into a tree Postgres handed to a
//! callback and lives for `'a`, that call. Handles come only from such a tree (an entry point,
//! whose `unsafe` states what Postgres guarantees there) or from the fields of another handle, so
//! each one is non-null and points to a live object of its type: Postgres frees a tree only after
//! the callback returns. A node's type is checked by its tag (`Node::cast`).
//!
//! Fields are read and written through raw places: no Rust reference into Postgres's memory is
//! made, so Postgres's own pointers to the same nodes never alias one, and handles are `Copy`.
//! New nodes are allocated in the memory context current during the callback, which outlives the
//! tree they are put into (`build`).

pub mod build;
pub mod catalog;
pub mod exec;
pub mod hooks;
pub mod plan;

pub use pgrx::pg_sys::{self, NodeTag, Oid};
use std::marker::PhantomData;
use std::ptr::NonNull;

/// A handle on a `T` in a Postgres tree, live for `'a`.
pub struct Ptr<'a, T> {
    raw: NonNull<T>,
    tree: PhantomData<(&'a (), *const T)>,
}

impl<T> Clone for Ptr<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ptr<'_, T> {}

impl<T> PartialEq for Ptr<'_, T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<T> Eq for Ptr<'_, T> {}

impl<T> std::fmt::Debug for Ptr<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.raw)
    }
}

impl<'a, T> Ptr<'a, T> {
    /// A handle on `raw`; None when it is null.
    ///
    /// # Safety
    /// `raw` is null or points to a live, well-formed `T` (a node of a Postgres tree, whose fields
    /// hold what Postgres puts in them) that stays so for `'a`.
    pub unsafe fn from_raw(raw: *mut T) -> Option<Self> {
        NonNull::new(raw).map(|raw| Ptr { raw, tree: PhantomData })
    }

    /// The pointer, for Postgres functions (called in this module).
    pub fn as_raw(self) -> *mut T {
        self.raw.as_ptr()
    }

    /// The pointer as an identity: two handles on one node have the same.
    pub fn addr(self) -> usize {
        self.raw.as_ptr() as usize
    }

    /// The memory new nodes for this tree go in.
    #[expect(clippy::unused_self, reason = "the handle only lends its lifetime to the token")]
    pub fn memory(self) -> Memory<'a> {
        Memory(PhantomData)
    }

    fn retype<U>(self) -> Ptr<'a, U> {
        Ptr { raw: self.raw.cast(), tree: PhantomData }
    }
}

/// C structs that begin with a node's tag, which a `Node` handle may point to.
///
/// # Safety
/// The struct's first field is its `NodeTag` (Postgres's `Node` header).
pub unsafe trait NodeStruct {}

/// Node structs identified by the tags their nodes carry.
///
/// # Safety
/// A node carrying one of `TAGS` is allocated as `Self`.
pub unsafe trait Tagged: NodeStruct {
    const TAGS: &'static [NodeTag];
}

macro_rules! node_structs {
    ($($t:ident),* $(,)?) => {
        // SAFETY: each of these begins with the Node header (their definitions in nodes/*.h)
        $(unsafe impl NodeStruct for pg_sys::$t {})*
    };
}

macro_rules! tagged {
    ($($t:ident: $($tag:ident)|+;)*) => {
        $(
            // SAFETY: Postgres makes nodes of these tags as this struct (makeNode in nodes.h)
            unsafe impl Tagged for pg_sys::$t {
                const TAGS: &'static [NodeTag] = &[$(NodeTag::$tag),+];
            }
        )*
    };
}

node_structs!(
    Node,
    Expr,
    Query,
    RangeTblEntry,
    Var,
    Const,
    Param,
    FuncExpr,
    OpExpr,
    BoolExpr,
    ScalarArrayOpExpr,
    CoerceViaIO,
    RelabelType,
    SubLink,
    VariableSetStmt,
    Path,
    IndexPath,
    BitmapHeapPath,
    BitmapAndPath,
    BitmapOrPath,
    CustomPath,
    Plan,
    RestrictInfo,
    String,
    Integer,
    PlanState,
    CoalesceExpr,
    FromExpr,
    TargetEntry,
    WithCheckOption,
    CommonTableExpr
);

tagged! {
    Query: T_Query;
    RangeTblEntry: T_RangeTblEntry;
    Var: T_Var;
    Const: T_Const;
    Param: T_Param;
    FuncExpr: T_FuncExpr;
    OpExpr: T_OpExpr;
    BoolExpr: T_BoolExpr;
    ScalarArrayOpExpr: T_ScalarArrayOpExpr;
    CoerceViaIO: T_CoerceViaIO;
    RelabelType: T_RelabelType;
    SubLink: T_SubLink;
    VariableSetStmt: T_VariableSetStmt;
    IndexPath: T_IndexPath;
    BitmapHeapPath: T_BitmapHeapPath;
    BitmapAndPath: T_BitmapAndPath;
    BitmapOrPath: T_BitmapOrPath;
    String: T_String;
    Integer: T_Integer;
}

/// Any node.
pub type Node<'a> = Ptr<'a, pg_sys::Node>;

impl<'a> Node<'a> {
    pub fn tag(self) -> NodeTag {
        // SAFETY: a Node handle points to a live node, whose first field is its tag
        unsafe { (*self.as_raw()).type_ }
    }

    /// This node as a `T`, when its tag says it is one.
    pub fn cast<T: Tagged>(self) -> Option<Ptr<'a, T>> {
        T::TAGS.contains(&self.tag()).then(|| self.retype())
    }
}

impl<'a, T: NodeStruct> Ptr<'a, T> {
    /// This node as any node.
    pub fn node(self) -> Node<'a> {
        self.retype()
    }
}

/// A Postgres `List` of pointers to `T` (null is the empty list), live for `'a`.
pub struct List<'a, T> {
    raw: *mut pg_sys::List,
    tree: PhantomData<(&'a (), *const T)>,
}

impl<T> Clone for List<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for List<'_, T> {}

impl<'a, T> List<'a, T> {
    /// # Safety
    /// `raw` is null or a live pointer list whose elements are null or live `T`s, for `'a`.
    pub unsafe fn from_raw(raw: *mut pg_sys::List) -> Self {
        List { raw, tree: PhantomData }
    }

    pub fn empty() -> Self {
        List { raw: std::ptr::null_mut(), tree: PhantomData }
    }

    pub fn as_raw(self) -> *mut pg_sys::List {
        self.raw
    }

    pub fn len(self) -> usize {
        if self.raw.is_null() {
            return 0;
        }
        // SAFETY: a live list (invariant)
        usize::try_from(unsafe { (*self.raw).length }).unwrap_or(0)
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The `i`th element; None past the end or for a null element.
    pub fn get(self, i: usize) -> Option<Ptr<'a, T>> {
        if i >= self.len() {
            return None;
        }
        // SAFETY: i is within the list's length; its cells hold pointers to live Ts or null
        unsafe { Ptr::from_raw((*(*self.raw).elements.add(i)).ptr_value.cast::<T>()) }
    }

    pub fn iter(self) -> impl Iterator<Item = Ptr<'a, T>> {
        (0..self.len()).filter_map(move |i| self.get(i))
    }

    /// Put `value` in place of the `i`th element; false past the end.
    pub fn set(self, i: usize, value: Ptr<'a, T>) -> bool {
        if i >= self.len() {
            return false;
        }
        // SAFETY: i is within the list's length, and the cell takes a pointer to a live T
        unsafe { (*(*self.raw).elements.add(i)).ptr_value = value.as_raw().cast() };
        true
    }

    pub fn position(self, value: Ptr<'a, T>) -> Option<usize> {
        self.iter().position(|v| v == value)
    }
}

impl<'a, T: NodeStruct> List<'a, T> {
    /// The same list, its elements as any node.
    pub fn nodes(self) -> List<'a, pg_sys::Node> {
        List { raw: self.raw, tree: PhantomData }
    }
}

/// A Postgres `List` of integers.
#[derive(Clone, Copy)]
pub struct IntList<'a> {
    raw: *mut pg_sys::List,
    tree: PhantomData<&'a ()>,
}

impl IntList<'_> {
    pub fn contains(self, value: i32) -> bool {
        // SAFETY: a live integer list or null (NIL), which list_member_int reads
        unsafe { pg_sys::list_member_int(self.raw, value) }
    }
}

/// Where new nodes go: the memory context current during the callback, which lives at least as
/// long as the tree they are put into. Had from a handle on that tree (`Ptr::memory`).
#[derive(Clone, Copy)]
pub struct Memory<'a>(PhantomData<&'a ()>);

/// A C field's value as a safe one: scalars as they are, pointers as handles.
///
/// # Safety
/// `read` is given what a well-formed tree holds in such a field, and makes handles only from it;
/// `write` gives back a value a well-formed tree may hold there.
pub unsafe trait Field<'a, Raw>: Sized {
    /// # Safety
    /// `raw` is the content of such a field of a live node.
    unsafe fn read(raw: Raw) -> Self;
    fn write(self) -> Raw;
}

macro_rules! scalar_fields {
    ($($t:ty),*) => {
        $(
            // SAFETY: plain values, read and written as they are
            unsafe impl<'a> Field<'a, $t> for $t {
                unsafe fn read(raw: $t) -> Self {
                    raw
                }
                fn write(self) -> $t {
                    self
                }
            }
        )*
    };
}

scalar_fields!(bool, i16, i32, u16, u32, i64, f64, Oid, NodeTag, pg_sys::Datum);

// SAFETY: a pointer field holds a live node of its type, or null
unsafe impl<'a, T> Field<'a, *mut T> for Option<Ptr<'a, T>> {
    unsafe fn read(raw: *mut T) -> Self {
        // SAFETY: the field of a live node points to a live T or is null
        unsafe { Ptr::from_raw(raw) }
    }
    fn write(self) -> *mut T {
        self.map_or(std::ptr::null_mut(), Ptr::as_raw)
    }
}

// SAFETY: an expression field holds a live expression node, or null
unsafe impl<'a> Field<'a, *mut pg_sys::Expr> for Option<Node<'a>> {
    unsafe fn read(raw: *mut pg_sys::Expr) -> Self {
        // SAFETY: as above; every expression is a node
        unsafe { Ptr::from_raw(raw.cast::<pg_sys::Node>()) }
    }
    fn write(self) -> *mut pg_sys::Expr {
        self.map_or(std::ptr::null_mut(), |n| n.as_raw().cast())
    }
}

// SAFETY: a list field holds a list of the declared element type (see each `fields!`), or NIL
unsafe impl<'a, T> Field<'a, *mut pg_sys::List> for List<'a, T> {
    unsafe fn read(raw: *mut pg_sys::List) -> Self {
        // SAFETY: as above
        unsafe { List::from_raw(raw) }
    }
    fn write(self) -> *mut pg_sys::List {
        self.raw
    }
}

// SAFETY: an integer list field holds an integer list, or NIL
unsafe impl<'a> Field<'a, *mut pg_sys::List> for IntList<'a> {
    unsafe fn read(raw: *mut pg_sys::List) -> Self {
        IntList { raw, tree: PhantomData }
    }
    fn write(self) -> *mut pg_sys::List {
        self.raw
    }
}

/// Accessors for a node struct's fields: `getter: c_field -> Type`, and `getter / setter:` for
/// fields pg_auth changes. List fields name their element type, which is what Postgres keeps in
/// them (the field's comment in its header).
macro_rules! fields {
    ($t:ident { $($get:ident $(/ $set:ident)?: $field:ident -> $ty:ty),* $(,)? }) => {
        impl<'a> Ptr<'a, pg_sys::$t> {
            $(
                #[allow(dead_code, reason = "every field gets its getter, used or not")]
                pub fn $get(self) -> $ty {
                    // SAFETY: the handle points to a live node of this type (module invariant),
                    // whose field holds what its conversion reads
                    unsafe { <$ty as Field<'a, _>>::read((*self.as_raw()).$field) }
                }
                $(
                    pub fn $set(self, value: $ty) {
                        // SAFETY: as above; the tree is the callback's to change, and the value is
                        // one a well-formed tree may hold in this field
                        unsafe { (*self.as_raw()).$field = Field::write(value) }
                    }
                )?
            )*
        }
    };
}
pub(crate) use fields;

fields!(Query {
    command_type: commandType -> pg_sys::CmdType::Type,
    result_relation: resultRelation -> i32,
    has_sublinks / set_has_sublinks: hasSubLinks -> bool,
    rtable: rtable -> List<'a, pg_sys::RangeTblEntry>,
    jointree: jointree -> Option<Ptr<'a, pg_sys::FromExpr>>,
    sort_clause: sortClause -> List<'a, pg_sys::Node>,
    row_marks: rowMarks -> List<'a, pg_sys::Node>,
    with_check_options: withCheckOptions -> List<'a, pg_sys::WithCheckOption>,
    cte_list: cteList -> List<'a, pg_sys::CommonTableExpr>,
    target_list: targetList -> List<'a, pg_sys::TargetEntry>,
});

fields!(RangeTblEntry {
    rtekind: rtekind -> pg_sys::RTEKind::Type,
    relid: relid -> Oid,
    inh: inh -> bool,
    security_quals: securityQuals -> List<'a, pg_sys::Node>,
    subquery: subquery -> Option<Ptr<'a, pg_sys::Query>>,
});

fields!(WithCheckOption { qual / set_qual: qual -> Option<Node<'a>> });
fields!(CommonTableExpr { cte_query: ctequery -> Option<Node<'a>> });
fields!(FromExpr { quals: quals -> Option<Node<'a>> });
fields!(TargetEntry { expr: expr -> Option<Node<'a>> });
fields!(BoolExpr { boolop: boolop -> pg_sys::BoolExprType::Type, args: args -> List<'a, pg_sys::Node> });
fields!(OpExpr { opno: opno -> Oid, input_collid: inputcollid -> Oid, args: args -> List<'a, pg_sys::Node> });
fields!(FuncExpr { funcid: funcid -> Oid, args: args -> List<'a, pg_sys::Node> });
fields!(ScalarArrayOpExpr { use_or: useOr -> bool, args: args -> List<'a, pg_sys::Node> });
fields!(CoerceViaIO { arg: arg -> Option<Node<'a>> });
fields!(RelabelType { arg: arg -> Option<Node<'a>> });
fields!(Var { varno: varno -> i32, varattno: varattno -> i16, varlevelsup: varlevelsup -> u32, vartype: vartype -> Oid });
fields!(Param { paramkind: paramkind -> pg_sys::ParamKind::Type, paramid: paramid -> i32 });
fields!(SubPlan { set_param: setParam -> IntList<'a>, plan_id: plan_id -> i32 });
fields!(VariableSetStmt { name_raw: name -> Option<Ptr<'a, std::ffi::c_char>> });

impl Ptr<'_, pg_sys::Const> {
    pub fn const_type(self) -> Oid {
        // SAFETY: a live Const (module invariant)
        unsafe { (*self.as_raw()).consttype }
    }

    pub fn is_null(self) -> bool {
        // SAFETY: as above
        unsafe { (*self.as_raw()).constisnull }
    }

    pub fn value(self) -> pg_sys::Datum {
        // SAFETY: as above
        unsafe { (*self.as_raw()).constvalue }
    }
}

impl Ptr<'_, pg_sys::Integer> {
    pub fn value(self) -> i32 {
        // SAFETY: a live Integer node
        unsafe { (*self.as_raw()).ival }
    }
}

impl Ptr<'_, pg_sys::VariableSetStmt> {
    /// The setting a SET names (None for RESET ALL and the like).
    pub fn name(self) -> Option<std::ffi::CString> {
        let name = self.name_raw()?;
        // SAFETY: a live statement's name is a NUL-terminated string, copied here
        Some(unsafe { std::ffi::CStr::from_ptr(name.as_raw()) }.to_owned())
    }
}
