//#![allow(dead_code)]
use super::gc::{GcWeak, GcoVec};
use super::instruction::{OpIndex, Opcode, ValueType};
use super::metadata::*;
pub use super::objects::*;
use super::stack::Stack;
use ordered_float;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::hash::{Hash, Hasher};
use std::num::Wrapping;
use std::rc::Rc;
use std::result;

type F32 = ordered_float::OrderedFloat<f32>;
type F64 = ordered_float::OrderedFloat<f64>;
pub type IRC = i32;
pub type RCount = Cell<IRC>;
pub type RCQueue = VecDeque<IRC>;

#[inline]
pub fn rcount_mark_and_queue(rc: &RCount, queue: &mut RCQueue) {
    let i = rc.get();
    if i <= 0 {
        queue.push_back(i);
        rc.set(1);
    }
}

macro_rules! unwrap_gos_val {
    ($name:tt, $self_:ident) => {
        if let GosValue::$name(k) = $self_ {
            k
        } else {
            unreachable!();
        }
    };
}

macro_rules! union_op_wrap {
    ($a:ident, $b:ident, $name:tt, $op:tt) => {
        GosValue64{
            data: V64Union {
            $name: (Wrapping($a.data.$name) $op Wrapping($b.data.$name)).0,
        }}
    };
}

macro_rules! union_op {
    ($a:ident, $b:ident, $name:tt, $op:tt) => {
        GosValue64{
            data: V64Union {
            $name: $a.data.$name $op $b.data.$name,
        }}
    };
}

macro_rules! union_shift {
    ($a:ident, $b:ident, $name:tt, $op:tt) => {
        GosValue64 {
            data: V64Union {
                $name: $a.data.$name.$op($b).unwrap_or(0),
            },
        }
    };
}

macro_rules! union_cmp {
    ($a:ident, $b:ident, $name:tt, $op:tt) => {
        $a.data.$name $op $b.data.$name
    };
}

macro_rules! binary_op_int_float {
    ($t:ident, $a:ident, $b:ident, $op:tt) => {
        match $t {
            ValueType::Int => union_op_wrap!($a, $b, int, $op),
            ValueType::Int8 => union_op_wrap!($a, $b, int8, $op),
            ValueType::Int16 => union_op_wrap!($a, $b, int16, $op),
            ValueType::Int32 => union_op_wrap!($a, $b, int32, $op),
            ValueType::Int64 => union_op_wrap!($a, $b, int64, $op),
            ValueType::Uint => union_op_wrap!($a, $b, uint, $op),
            ValueType::UintPtr => union_op_wrap!($a, $b, uint_ptr, $op),
            ValueType::Uint8 => union_op_wrap!($a, $b, uint8, $op),
            ValueType::Uint16 => union_op_wrap!($a, $b, uint16, $op),
            ValueType::Uint32 => union_op_wrap!($a, $b, uint32, $op),
            ValueType::Uint64 => union_op_wrap!($a, $b, uint64, $op),
            ValueType::Float32 => union_op!($a, $b, float32, $op),
            ValueType::Float64 => union_op!($a, $b, float64, $op),
            _ => unreachable!(),
        }
    };
}

macro_rules! binary_op_int_no_wrap {
    ($t:ident, $a:ident, $b:ident, $op:tt) => {
        match $t {
            ValueType::Int => union_op!($a, $b, int, $op),
            ValueType::Int8 => union_op!($a, $b, int8, $op),
            ValueType::Int16 => union_op!($a, $b, int16, $op),
            ValueType::Int32 => union_op!($a, $b, int32, $op),
            ValueType::Int64 => union_op!($a, $b, int64, $op),
            ValueType::Uint => union_op!($a, $b, uint, $op),
            ValueType::UintPtr => union_op!($a, $b, uint_ptr, $op),
            ValueType::Uint8 => union_op!($a, $b, uint8, $op),
            ValueType::Uint16 => union_op!($a, $b, uint16, $op),
            ValueType::Uint32 => union_op!($a, $b, uint32, $op),
            ValueType::Uint64 => union_op!($a, $b, uint64, $op),
            _ => unreachable!(),
        }
    };
}

macro_rules! cmp_bool_int_float {
    ($t:ident, $a:ident, $b:ident, $op:tt) => {
        match $t {
            ValueType::Bool => union_cmp!($a, $b, ubool, $op),
            ValueType::Int => union_cmp!($a, $b, int, $op),
            ValueType::Int8 => union_cmp!($a, $b, int8, $op),
            ValueType::Int16 => union_cmp!($a, $b, int16, $op),
            ValueType::Int32 => union_cmp!($a, $b, int32, $op),
            ValueType::Int64 => union_cmp!($a, $b, int64, $op),
            ValueType::Uint => union_cmp!($a, $b, uint, $op),
            ValueType::UintPtr => union_cmp!($a, $b, uint_ptr, $op),
            ValueType::Uint8 => union_cmp!($a, $b, uint8, $op),
            ValueType::Uint16 => union_cmp!($a, $b, uint16, $op),
            ValueType::Uint32 => union_cmp!($a, $b, uint32, $op),
            ValueType::Uint64 => union_cmp!($a, $b, uint64, $op),
            ValueType::Float32 => union_cmp!($a, $b, float32, $op),
            ValueType::Float64 => union_cmp!($a, $b, float64, $op),
            _ => unreachable!(),
        }
    };
}

macro_rules! cmp_int_float {
    ($t:ident, $a:ident, $b:ident, $op:tt) => {
        match $t {
            ValueType::Int => union_cmp!($a, $b, int, $op),
            ValueType::Int8 => union_cmp!($a, $b, int8, $op),
            ValueType::Int16 => union_cmp!($a, $b, int16, $op),
            ValueType::Int32 => union_cmp!($a, $b, int32, $op),
            ValueType::Int64 => union_cmp!($a, $b, int64, $op),
            ValueType::Uint => union_cmp!($a, $b, uint, $op),
            ValueType::UintPtr => union_cmp!($a, $b, uint_ptr, $op),
            ValueType::Uint8 => union_cmp!($a, $b, uint8, $op),
            ValueType::Uint16 => union_cmp!($a, $b, uint16, $op),
            ValueType::Uint32 => union_cmp!($a, $b, uint32, $op),
            ValueType::Uint64 => union_cmp!($a, $b, uint64, $op),
            ValueType::Float32 => union_cmp!($a, $b, float32, $op),
            ValueType::Float64 => union_cmp!($a, $b, float64, $op),
            _ => unreachable!(),
        }
    };
}

macro_rules! shift_int {
    ($t:ident, $a:ident, $b:ident, $op:tt) => {
        *$a = match $t {
            ValueType::Int => union_shift!($a, $b, int, $op),
            ValueType::Int8 => union_shift!($a, $b, int8, $op),
            ValueType::Int16 => union_shift!($a, $b, int16, $op),
            ValueType::Int32 => union_shift!($a, $b, int32, $op),
            ValueType::Int64 => union_shift!($a, $b, int64, $op),
            ValueType::Uint => union_shift!($a, $b, uint, $op),
            ValueType::UintPtr => union_shift!($a, $b, uint_ptr, $op),
            ValueType::Uint8 => union_shift!($a, $b, uint8, $op),
            ValueType::Uint16 => union_shift!($a, $b, uint16, $op),
            ValueType::Uint32 => union_shift!($a, $b, uint32, $op),
            ValueType::Uint64 => union_shift!($a, $b, uint64, $op),
            _ => unreachable!(),
        }
    };
}

macro_rules! convert_to_int {
    ($val:expr, $vt:expr, $d_type:tt, $typ:tt) => {{
        unsafe {
            match $vt {
                ValueType::Uint => $val.data.$d_type = $val.data.uint as $typ,
                ValueType::UintPtr => $val.data.$d_type = $val.data.uint_ptr as $typ,
                ValueType::Uint8 => $val.data.$d_type = $val.data.uint8 as $typ,
                ValueType::Uint16 => $val.data.$d_type = $val.data.uint16 as $typ,
                ValueType::Uint32 => $val.data.$d_type = $val.data.uint32 as $typ,
                ValueType::Uint64 => $val.data.$d_type = $val.data.uint64 as $typ,
                ValueType::Int => $val.data.$d_type = $val.data.int as $typ,
                ValueType::Int8 => $val.data.$d_type = $val.data.int8 as $typ,
                ValueType::Int16 => $val.data.$d_type = $val.data.int16 as $typ,
                ValueType::Int32 => $val.data.$d_type = $val.data.int32 as $typ,
                ValueType::Int64 => $val.data.$d_type = $val.data.int64 as $typ,
                ValueType::Float32 => $val.data.$d_type = f32::from($val.data.float32) as $typ,
                ValueType::Float64 => $val.data.$d_type = f64::from($val.data.float64) as $typ,
                _ => unreachable!(),
            }
        }
    }};
}

macro_rules! convert_to_float {
    ($val:expr, $vt:expr, $d_type:tt, $f_type:tt, $typ:tt) => {{
        unsafe {
            match $vt {
                ValueType::Uint => $val.data.$d_type = $f_type::from($val.data.uint as $typ),
                ValueType::UintPtr => $val.data.$d_type = $f_type::from($val.data.uint_ptr as $typ),
                ValueType::Uint8 => $val.data.$d_type = $f_type::from($val.data.uint8 as $typ),
                ValueType::Uint16 => $val.data.$d_type = $f_type::from($val.data.uint16 as $typ),
                ValueType::Uint32 => $val.data.$d_type = $f_type::from($val.data.uint32 as $typ),
                ValueType::Uint64 => $val.data.$d_type = $f_type::from($val.data.uint64 as $typ),
                ValueType::Int => $val.data.$d_type = $f_type::from($val.data.int as $typ),
                ValueType::Int8 => $val.data.$d_type = $f_type::from($val.data.int8 as $typ),
                ValueType::Int16 => $val.data.$d_type = $f_type::from($val.data.int16 as $typ),
                ValueType::Int32 => $val.data.$d_type = $f_type::from($val.data.int32 as $typ),
                ValueType::Int64 => $val.data.$d_type = $f_type::from($val.data.int64 as $typ),
                ValueType::Float32 => {
                    $val.data.$d_type = $f_type::from(f32::from($val.data.float32) as $typ)
                }
                ValueType::Float64 => {
                    $val.data.$d_type = $f_type::from(f64::from($val.data.float64) as $typ)
                }
                _ => unreachable!(),
            }
        }
    }};
}

pub type RuntimeResult<T> = result::Result<T, String>;

// ----------------------------------------------------------------------------
// GosValue
#[derive(Debug)]
pub enum GosValue {
    Nil(GosMetadata),
    Bool(bool),
    Int(isize),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    Uint(usize),
    UintPtr(usize),
    Uint8(u8),
    Uint16(u16),
    Uint32(u32),
    Uint64(u64),
    Float32(F32),
    Float64(F64), // becasue in Go there is no "float", just float64
    Complex64(F32, F32),
    Complex128(Box<(F64, F64)>),

    // the 3 below are not visible to users, they are "values" not "variables"
    // they are static data, don't use Rc for better performance
    Function(FunctionKey),
    Package(PackageKey),
    Metadata(GosMetadata),

    Str(Rc<StringObj>), // "String" is taken
    Array(Rc<(ArrayObj, RCount)>),
    Pointer(Box<PointerObj>),
    Closure(Rc<(RefCell<ClosureObj>, RCount)>),
    Slice(Rc<(SliceObj, RCount)>),
    Map(Rc<(MapObj, RCount)>),
    Interface(Rc<RefCell<InterfaceObj>>),
    Struct(Rc<(RefCell<StructObj>, RCount)>),
    Channel(Rc<ChannelObj>),

    Named(Box<(GosValue, GosMetadata)>),
}

impl GosValue {
    #[inline]
    pub fn new_nil() -> GosValue {
        GosValue::Nil(GosMetadata::Untyped)
    }

    #[inline]
    pub fn new_str(s: String) -> GosValue {
        GosValue::Str(Rc::new(StringObj::with_str(s)))
    }

    #[inline]
    pub fn new_pointer(v: PointerObj) -> GosValue {
        GosValue::Pointer(Box::new(v))
    }

    #[inline]
    pub fn array_with_size(
        size: usize,
        val: &GosValue,
        meta: GosMetadata,
        gcobjs: &GcoVec,
    ) -> GosValue {
        let arr = Rc::new((ArrayObj::with_size(size, val, meta, gcobjs), Cell::new(0)));
        let v = GosValue::Array(arr);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn array_with_val(val: Vec<GosValue>, meta: GosMetadata, gcobjs: &GcoVec) -> GosValue {
        let arr = Rc::new((ArrayObj::with_data(val, meta), Cell::new(0)));
        let v = GosValue::Array(arr);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_slice(
        len: usize,
        cap: usize,
        meta: GosMetadata,
        dval: Option<&GosValue>,
        gcobjs: &GcoVec,
    ) -> GosValue {
        let s = Rc::new((SliceObj::new(len, cap, meta, dval), Cell::new(0)));
        let v = GosValue::Slice(s);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_slice_nil(meta: GosMetadata, gcobjs: &GcoVec) -> GosValue {
        let s = Rc::new((SliceObj::new_nil(meta), Cell::new(0)));
        let v = GosValue::Slice(s);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn slice_with_obj(obj: SliceObj, gcobjs: &GcoVec) -> GosValue {
        let s = Rc::new((obj, Cell::new(0)));
        let v = GosValue::Slice(s);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn slice_with_val(val: Vec<GosValue>, meta: GosMetadata, gcobjs: &GcoVec) -> GosValue {
        let s = Rc::new((SliceObj::with_data(val, meta), Cell::new(0)));
        let v = GosValue::Slice(s);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn slice_with_array(arr: &GosValue, begin: isize, end: isize, gcobjs: &GcoVec) -> GosValue {
        let s = Rc::new((
            SliceObj::with_array(&arr.as_array().0, begin, end),
            Cell::new(0),
        ));
        let v = GosValue::Slice(s);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_map(meta: GosMetadata, default_val: GosValue, gcobjs: &GcoVec) -> GosValue {
        let val = Rc::new((MapObj::new(meta, default_val), Cell::new(0)));
        let v = GosValue::Map(val);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_map_nil(meta: GosMetadata, default_val: GosValue, gcobjs: &GcoVec) -> GosValue {
        let val = Rc::new((MapObj::new_nil(meta, default_val), Cell::new(0)));
        let v = GosValue::Map(val);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_struct(obj: StructObj, gcobjs: &GcoVec) -> GosValue {
        let val = Rc::new((RefCell::new(obj), Cell::new(0)));
        let v = GosValue::Struct(val);
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_function(
        package: PackageKey,
        meta: GosMetadata,
        objs: &mut VMObjects,
        gcv: &GcoVec,
        flag: FuncFlag,
    ) -> GosValue {
        let val = FunctionVal::new(package, meta, objs, gcv, flag);
        GosValue::Function(objs.functions.insert(val))
    }

    #[inline]
    pub fn new_closure(
        fkey: FunctionKey,
        fobjs: &FunctionObjs, /*, gcobjs: &mut GcoVec todo! */
    ) -> GosValue {
        let val = ClosureObj::new_gos(fkey, fobjs, None);
        GosValue::Closure(Rc::new((RefCell::new(val), Cell::new(0))))
    }

    #[inline]
    pub fn new_runtime_closure(clsobj: ClosureObj, gcobjs: &GcoVec) -> GosValue {
        let v = GosValue::Closure(Rc::new((RefCell::new(clsobj), Cell::new(0))));
        gcobjs.add(&v);
        v
    }

    #[inline]
    pub fn new_iface(meta: GosMetadata, underlying: IfaceUnderlying) -> GosValue {
        let val = Rc::new(RefCell::new(InterfaceObj::new(meta, underlying)));
        GosValue::Interface(val)
    }

    #[inline]
    pub fn new_empty_iface(mdata: &Metadata, underlying: GosValue) -> GosValue {
        let val = Rc::new(RefCell::new(InterfaceObj::new(
            mdata.empty_iface,
            IfaceUnderlying::Gos(underlying, None),
        )));
        GosValue::Interface(val)
    }

    #[inline]
    pub fn new_channel(meta: GosMetadata, cap: usize) -> GosValue {
        GosValue::Channel(Rc::new(ChannelObj::new(meta, cap)))
    }

    #[inline]
    pub fn new_meta(t: MetadataType, metas: &mut MetadataObjs) -> GosValue {
        GosValue::Metadata(GosMetadata::NonPtr(metas.insert(t), MetaCategory::Default))
    }

    #[inline]
    pub fn as_bool(&self) -> &bool {
        unwrap_gos_val!(Bool, self)
    }

    #[inline]
    pub fn as_int(&self) -> &isize {
        unwrap_gos_val!(Int, self)
    }

    #[inline]
    pub fn as_uint8(&self) -> &u8 {
        unwrap_gos_val!(Uint8, self)
    }

    #[inline]
    pub fn as_uint32(&self) -> &u32 {
        unwrap_gos_val!(Uint32, self)
    }

    #[inline]
    pub fn as_uint64(&self) -> &u64 {
        unwrap_gos_val!(Uint64, self)
    }

    #[inline]
    pub fn as_int32(&self) -> &i32 {
        unwrap_gos_val!(Int32, self)
    }

    #[inline]
    pub fn as_int_mut(&mut self) -> &mut isize {
        unwrap_gos_val!(Int, self)
    }

    #[inline]
    pub fn as_float32(&self) -> &f32 {
        unwrap_gos_val!(Float32, self)
    }

    #[inline]
    pub fn as_float(&self) -> &f64 {
        unwrap_gos_val!(Float64, self)
    }

    #[inline]
    pub fn as_str(&self) -> &Rc<StringObj> {
        unwrap_gos_val!(Str, self)
    }

    #[inline]
    pub fn as_array(&self) -> &Rc<(ArrayObj, RCount)> {
        unwrap_gos_val!(Array, self)
    }

    #[inline]
    pub fn as_slice(&self) -> &Rc<(SliceObj, RCount)> {
        unwrap_gos_val!(Slice, self)
    }

    #[inline]
    pub fn as_map(&self) -> &Rc<(MapObj, RCount)> {
        unwrap_gos_val!(Map, self)
    }

    #[inline]
    pub fn as_interface(&self) -> &Rc<RefCell<InterfaceObj>> {
        unwrap_gos_val!(Interface, self)
    }

    #[inline]
    pub fn as_channel(&self) -> &Rc<ChannelObj> {
        unwrap_gos_val!(Channel, self)
    }

    #[inline]
    pub fn as_function(&self) -> &FunctionKey {
        unwrap_gos_val!(Function, self)
    }

    #[inline]
    pub fn as_package(&self) -> &PackageKey {
        unwrap_gos_val!(Package, self)
    }

    #[inline]
    pub fn as_struct(&self) -> &Rc<(RefCell<StructObj>, RCount)> {
        unwrap_gos_val!(Struct, self)
    }

    #[inline]
    pub fn as_closure(&self) -> &Rc<(RefCell<ClosureObj>, RCount)> {
        unwrap_gos_val!(Closure, self)
    }

    #[inline]
    pub fn as_meta(&self) -> &GosMetadata {
        unwrap_gos_val!(Metadata, self)
    }

    #[inline]
    pub fn as_pointer(&self) -> &Box<PointerObj> {
        unwrap_gos_val!(Pointer, self)
    }

    #[inline]
    pub fn as_named(&self) -> &Box<(GosValue, GosMetadata)> {
        unwrap_gos_val!(Named, self)
    }

    #[inline]
    pub fn is_nil(&self) -> bool {
        match &self {
            GosValue::Nil(_) => true,
            _ => false,
        }
    }

    #[inline]
    pub fn try_as_interface(&self) -> Option<&Rc<RefCell<InterfaceObj>>> {
        match &self {
            GosValue::Named(n) => Some(n.0.as_interface()),
            GosValue::Interface(_) => Some(self.as_interface()),
            _ => None,
        }
    }

    #[inline]
    pub fn try_as_struct(&self) -> Option<&Rc<(RefCell<StructObj>, RCount)>> {
        match &self {
            GosValue::Named(n) => Some(n.0.as_struct()),
            GosValue::Struct(_) => Some(self.as_struct()),
            _ => None,
        }
    }

    #[inline]
    pub fn try_as_map(&self) -> Option<&Rc<(MapObj, RCount)>> {
        match &self {
            GosValue::Named(n) => Some(n.0.as_map()),
            GosValue::Map(_) => Some(self.as_map()),
            _ => None,
        }
    }

    #[inline]
    pub fn unwrap_named(self) -> GosValue {
        match self {
            GosValue::Named(n) => n.0,
            _ => self,
        }
    }

    #[inline]
    pub fn unwrap_named_ref(&self) -> &GosValue {
        match self {
            GosValue::Named(n) => &n.0,
            _ => &self,
        }
    }

    #[inline]
    pub fn iface_underlying(&self) -> Option<GosValue> {
        match &self {
            GosValue::Named(n) => match &n.0 {
                GosValue::Nil(_) => Some(n.0.clone()),
                GosValue::Interface(i) => {
                    let b = i.borrow();
                    b.underlying_value().map(|x| x.clone())
                }
                _ => unreachable!(),
            },
            GosValue::Interface(v) => {
                let b = v.borrow();
                b.underlying_value().map(|x| x.clone())
            }
            _ => unreachable!(),
        }
    }

    #[inline]
    pub fn equals_nil(&self) -> bool {
        match &self {
            GosValue::Nil(_) => true,
            GosValue::Named(n) => n.0.is_nil(),
            GosValue::Slice(s) => s.0.is_nil(),
            GosValue::Map(m) => m.0.is_nil(),
            GosValue::Interface(iface) => iface.borrow().is_nil(),
            _ => false,
        }
    }

    #[inline]
    pub fn typ(&self) -> ValueType {
        match self {
            GosValue::Nil(_) => ValueType::Nil,
            GosValue::Bool(_) => ValueType::Bool,
            GosValue::Int(_) => ValueType::Int,
            GosValue::Int8(_) => ValueType::Int8,
            GosValue::Int16(_) => ValueType::Int16,
            GosValue::Int32(_) => ValueType::Int32,
            GosValue::Int64(_) => ValueType::Int64,
            GosValue::Uint(_) => ValueType::Uint,
            GosValue::UintPtr(_) => ValueType::UintPtr,
            GosValue::Uint8(_) => ValueType::Uint8,
            GosValue::Uint16(_) => ValueType::Uint16,
            GosValue::Uint32(_) => ValueType::Uint32,
            GosValue::Uint64(_) => ValueType::Uint64,
            GosValue::Float32(_) => ValueType::Float32,
            GosValue::Float64(_) => ValueType::Float64,
            GosValue::Complex64(_, _) => ValueType::Complex64,
            GosValue::Complex128(_) => ValueType::Complex128,
            GosValue::Str(_) => ValueType::Str,
            GosValue::Array(_) => ValueType::Array,
            GosValue::Pointer(_) => ValueType::Pointer,
            GosValue::Closure(_) => ValueType::Closure,
            GosValue::Slice(_) => ValueType::Slice,
            GosValue::Map(_) => ValueType::Map,
            GosValue::Interface(_) => ValueType::Interface,
            GosValue::Struct(_) => ValueType::Struct,
            GosValue::Channel(_) => ValueType::Channel,
            GosValue::Function(_) => ValueType::Function,
            GosValue::Package(_) => ValueType::Package,
            GosValue::Metadata(_) => ValueType::Metadata,
            GosValue::Named(_) => ValueType::Named,
        }
    }

    pub fn identical(&self, other: &GosValue) -> bool {
        self.typ() == other.typ() && self == other
    }

    pub fn meta(&self, objs: &VMObjects, stack: &Stack) -> GosMetadata {
        match self {
            GosValue::Nil(m) => *m,
            GosValue::Bool(_) => objs.metadata.mbool,
            GosValue::Int(_) => objs.metadata.mint,
            GosValue::Int8(_) => objs.metadata.mint8,
            GosValue::Int16(_) => objs.metadata.mint16,
            GosValue::Int32(_) => objs.metadata.mint32,
            GosValue::Int64(_) => objs.metadata.mint64,
            GosValue::Uint(_) => objs.metadata.muint,
            GosValue::UintPtr(_) => objs.metadata.muint_ptr,
            GosValue::Uint8(_) => objs.metadata.muint8,
            GosValue::Uint16(_) => objs.metadata.muint16,
            GosValue::Uint32(_) => objs.metadata.muint32,
            GosValue::Uint64(_) => objs.metadata.muint64,
            GosValue::Float32(_) => objs.metadata.mfloat32,
            GosValue::Float64(_) => objs.metadata.mfloat64,
            GosValue::Complex64(_, _) => objs.metadata.mcomplex64,
            GosValue::Complex128(_) => objs.metadata.mcomplex128,
            GosValue::Str(_) => objs.metadata.mstr,
            GosValue::Array(a) => a.0.meta,
            GosValue::Pointer(b) => {
                let bobj: &PointerObj = &*b;
                let pointee = match bobj {
                    //PointerObj::Nil => GosMetadata::Untyped,
                    PointerObj::UpVal(uv) => {
                        let state: &UpValueState = &uv.inner.borrow();
                        match state {
                            UpValueState::Open(d) => stack
                                .get_with_type(d.index as usize, d.typ)
                                .meta(objs, stack),
                            UpValueState::Closed(v) => v.meta(objs, stack),
                        }
                    }
                    PointerObj::Struct(s, named_md) => match named_md {
                        GosMetadata::Untyped => s.0.borrow().meta,
                        _ => *named_md,
                    },
                    PointerObj::Array(a, named_md) => match named_md {
                        GosMetadata::Untyped => a.0.meta,
                        _ => *named_md,
                    },
                    PointerObj::Slice(s, named_md) => match named_md {
                        GosMetadata::Untyped => s.0.meta,
                        _ => *named_md,
                    },
                    PointerObj::Map(m, named_md) => match named_md {
                        GosMetadata::Untyped => m.0.meta,
                        _ => *named_md,
                    },
                    PointerObj::StructField(sobj, index) => {
                        sobj.0.borrow().fields[*index as usize].meta(objs, stack)
                    }
                    PointerObj::SliceMember(sobj, index) => {
                        sobj.0.borrow()[*index as usize].borrow().meta(objs, stack)
                    }
                    PointerObj::PkgMember(pkey, index) => {
                        objs.packages[*pkey].member(*index).meta(objs, stack)
                    }
                    PointerObj::UserData(_) => objs.metadata.unsafe_ptr,
                    PointerObj::Released => unreachable!(),
                };
                pointee.ptr_to()
            }
            GosValue::Closure(c) => c.0.borrow().meta,
            GosValue::Slice(s) => s.0.meta,
            GosValue::Map(m) => m.0.meta,
            GosValue::Interface(i) => i.borrow().meta,
            GosValue::Struct(s) => s.0.borrow().meta,
            GosValue::Channel(_) => unimplemented!(),
            GosValue::Function(_) => unimplemented!(),
            GosValue::Package(_) => unimplemented!(),
            GosValue::Metadata(_) => unimplemented!(),
            GosValue::Named(v) => v.1,
        }
    }

    #[inline]
    pub fn copy_semantic(&self, gcos: &GcoVec) -> GosValue {
        match self {
            GosValue::Slice(s) => {
                let rc = Rc::new((SliceObj::clone(&s.0), Cell::new(0)));
                gcos.add_weak(GcWeak::Slice(Rc::downgrade(&rc)));
                GosValue::Slice(rc)
            }
            GosValue::Map(m) => {
                let rc = Rc::new((MapObj::clone(&m.0), Cell::new(0)));
                gcos.add_weak(GcWeak::Map(Rc::downgrade(&rc)));
                GosValue::Map(rc)
            }
            GosValue::Struct(s) => {
                let rc = Rc::new((RefCell::clone(&s.0), Cell::new(0)));
                gcos.add_weak(GcWeak::Struct(Rc::downgrade(&rc)));
                GosValue::Struct(rc)
            }
            GosValue::Named(v) => GosValue::Named(Box::new((v.0.copy_semantic(gcos), v.1))),
            _ => self.clone(),
        }
    }

    #[inline]
    pub fn as_index(&self) -> usize {
        match self {
            GosValue::Int(i) => *i as usize,
            GosValue::Int8(i) => *i as usize,
            GosValue::Int16(i) => *i as usize,
            GosValue::Int32(i) => *i as usize,
            GosValue::Int64(i) => *i as usize,
            GosValue::Uint(i) => *i as usize,
            GosValue::Uint8(i) => *i as usize,
            GosValue::Uint16(i) => *i as usize,
            GosValue::Uint32(i) => *i as usize,
            GosValue::Uint64(i) => *i as usize,
            _ => unreachable!(),
        }
    }

    #[inline]
    pub fn add_str(a: &GosValue, b: &GosValue) -> GosValue {
        let mut s = a.as_str().as_str().to_string();
        s.push_str(b.as_str().as_str());
        GosValue::new_str(s)
    }

    #[inline(always)]
    pub fn load_index(&self, ind: &GosValue) -> RuntimeResult<GosValue> {
        match self {
            GosValue::Map(map) => Ok(map.0.get(&ind).clone()),
            GosValue::Slice(slice) => {
                let index = ind.as_index();
                slice
                    .0
                    .get(index)
                    .map_or_else(|| Err(format!("index {} out of range", index)), |x| Ok(x))
            }
            GosValue::Str(s) => {
                let index = ind.as_index();
                s.get_byte(index).map_or_else(
                    || Err(format!("index {} out of range", index)),
                    |x| Ok(GosValue::Int((*x).into())),
                )
            }
            GosValue::Array(arr) => {
                let index = ind.as_index();
                arr.0
                    .get(index)
                    .map_or_else(|| Err(format!("index {} out of range", index)), |x| Ok(x))
            }
            _ => unreachable!(),
        }
    }

    #[inline]
    pub fn load_index_int(&self, i: usize) -> RuntimeResult<GosValue> {
        match self {
            GosValue::Slice(slice) => slice
                .0
                .get(i)
                .map_or_else(|| Err(format!("index {} out of range", i)), |x| Ok(x)),
            GosValue::Map(map) => {
                let ind = GosValue::Int(i as isize);
                Ok(map.0.get(&ind).clone())
            }
            GosValue::Str(s) => s.get_byte(i).map_or_else(
                || Err(format!("index {} out of range", i)),
                |x| Ok(GosValue::Int((*x).into())),
            ),
            GosValue::Array(arr) => arr
                .0
                .get(i)
                .map_or_else(|| Err(format!("index {} out of range", i)), |x| Ok(x)),
            GosValue::Named(n) => n.0.load_index_int(i),
            _ => {
                dbg!(self);
                unreachable!();
            }
        }
    }

    #[inline]
    pub fn load_field(&self, ind: &GosValue, objs: &VMObjects) -> GosValue {
        match self {
            GosValue::Struct(sval) => match &ind {
                GosValue::Int(i) => sval.0.borrow().fields[*i as usize].clone(),
                _ => unreachable!(),
            },
            GosValue::Package(pkey) => {
                let pkg = &objs.packages[*pkey];
                pkg.member(*ind.as_int() as OpIndex).clone()
            }
            _ => unreachable!(),
        }
    }

    /// for gc
    pub fn ref_sub_one(&self) {
        match &self {
            GosValue::Array(obj) => obj.1.set(obj.1.get() - 1),
            GosValue::Pointer(obj) => obj.ref_sub_one(),
            GosValue::Closure(obj) => obj.1.set(obj.1.get() - 1),
            GosValue::Slice(obj) => obj.1.set(obj.1.get() - 1),
            GosValue::Map(obj) => obj.1.set(obj.1.get() - 1),
            GosValue::Interface(obj) => obj.borrow().ref_sub_one(),
            GosValue::Struct(obj) => obj.1.set(obj.1.get() - 1),
            GosValue::Named(obj) => obj.0.ref_sub_one(),
            _ => {}
        };
    }

    /// for gc
    pub fn mark_dirty(&self, queue: &mut RCQueue) {
        match &self {
            GosValue::Array(obj) => rcount_mark_and_queue(&obj.1, queue),
            GosValue::Pointer(obj) => obj.mark_dirty(queue),
            GosValue::Closure(obj) => rcount_mark_and_queue(&obj.1, queue),
            GosValue::Slice(obj) => rcount_mark_and_queue(&obj.1, queue),
            GosValue::Map(obj) => rcount_mark_and_queue(&obj.1, queue),
            GosValue::Interface(obj) => obj.borrow().mark_dirty(queue),
            GosValue::Struct(obj) => rcount_mark_and_queue(&obj.1, queue),
            GosValue::Named(obj) => obj.0.mark_dirty(queue),
            _ => {}
        };
    }

    pub fn rc(&self) -> IRC {
        match &self {
            GosValue::Array(obj) => obj.1.get(),
            GosValue::Closure(obj) => obj.1.get(),
            GosValue::Slice(obj) => obj.1.get(),
            GosValue::Map(obj) => obj.1.get(),
            GosValue::Struct(obj) => obj.1.get(),
            _ => unreachable!(),
        }
    }

    pub fn set_rc(&self, rc: IRC) {
        match &self {
            GosValue::Array(obj) => obj.1.set(rc),
            GosValue::Closure(obj) => obj.1.set(rc),
            GosValue::Slice(obj) => obj.1.set(rc),
            GosValue::Map(obj) => obj.1.set(rc),
            GosValue::Struct(obj) => obj.1.set(rc),
            _ => unreachable!(),
        }
    }
}

impl Clone for GosValue {
    #[inline(always)]
    fn clone(&self) -> Self {
        match self {
            GosValue::Nil(m) => GosValue::Nil(*m),
            GosValue::Bool(v) => GosValue::Bool(*v),
            GosValue::Int(v) => GosValue::Int(*v),
            GosValue::Int8(v) => GosValue::Int8(*v),
            GosValue::Int16(v) => GosValue::Int16(*v),
            GosValue::Int32(v) => GosValue::Int32(*v),
            GosValue::Int64(v) => GosValue::Int64(*v),
            GosValue::Uint(v) => GosValue::Uint(*v),
            GosValue::UintPtr(v) => GosValue::UintPtr(*v),
            GosValue::Uint8(v) => GosValue::Uint8(*v),
            GosValue::Uint16(v) => GosValue::Uint16(*v),
            GosValue::Uint32(v) => GosValue::Uint32(*v),
            GosValue::Uint64(v) => GosValue::Uint64(*v),
            GosValue::Float32(v) => GosValue::Float32(*v),
            GosValue::Float64(v) => GosValue::Float64(*v),
            GosValue::Complex64(r, i) => GosValue::Complex64(*r, *i),
            GosValue::Complex128(v) => GosValue::Complex128(v.clone()),
            GosValue::Str(v) => GosValue::Str(v.clone()),
            GosValue::Array(v) => GosValue::Array(v.clone()),
            GosValue::Pointer(v) => GosValue::Pointer(v.clone()),
            GosValue::Closure(v) => GosValue::Closure(v.clone()),
            GosValue::Slice(v) => GosValue::Slice(v.clone()),
            GosValue::Map(v) => GosValue::Map(v.clone()),
            GosValue::Interface(v) => GosValue::Interface(v.clone()),
            GosValue::Struct(v) => GosValue::Struct(v.clone()),
            GosValue::Channel(v) => GosValue::Channel(v.clone()),
            GosValue::Function(v) => GosValue::Function(*v),
            GosValue::Package(v) => GosValue::Package(*v),
            GosValue::Metadata(v) => GosValue::Metadata(*v),
            GosValue::Named(v) => GosValue::Named(v.clone()),
        }
    }
}

impl Eq for GosValue {}

impl PartialEq for GosValue {
    #[inline]
    fn eq(&self, b: &GosValue) -> bool {
        match (self, b) {
            (Self::Nil(_), Self::Nil(_)) => true,
            (Self::Bool(x), Self::Bool(y)) => x == y,
            (Self::Int(x), Self::Int(y)) => x == y,
            (Self::Int8(x), Self::Int8(y)) => x == y,
            (Self::Int16(x), Self::Int16(y)) => x == y,
            (Self::Int32(x), Self::Int32(y)) => x == y,
            (Self::Int64(x), Self::Int64(y)) => x == y,
            (Self::Uint(x), Self::Uint(y)) => x == y,
            (Self::UintPtr(x), Self::UintPtr(y)) => x == y,
            (Self::Uint8(x), Self::Uint8(y)) => x == y,
            (Self::Uint16(x), Self::Uint16(y)) => x == y,
            (Self::Uint32(x), Self::Uint32(y)) => x == y,
            (Self::Uint64(x), Self::Uint64(y)) => x == y,
            (Self::Float32(x), Self::Float32(y)) => x == y,
            (Self::Float64(x), Self::Float64(y)) => x == y,
            (Self::Complex64(xr, xi), Self::Complex64(yr, yi)) => xr == yr && xi == yi,
            (Self::Complex128(x), Self::Complex128(y)) => x.0 == y.0 && x.1 == y.1,
            (Self::Function(x), Self::Function(y)) => x == y,
            (Self::Package(x), Self::Package(y)) => x == y,
            (Self::Metadata(x), Self::Metadata(y)) => x == y,
            (Self::Str(x), Self::Str(y)) => *x == *y,
            (Self::Array(x), Self::Array(y)) => x.0 == y.0,
            (Self::Pointer(x), Self::Pointer(y)) => x == y,
            (Self::Closure(x), Self::Closure(y)) => Rc::ptr_eq(x, y),
            (Self::Slice(x), Self::Slice(y)) => Rc::ptr_eq(x, y),
            (Self::Map(x), Self::Map(y)) => Rc::ptr_eq(x, y),
            (Self::Interface(x), Self::Interface(y)) => InterfaceObj::eq(&x.borrow(), &y.borrow()),
            (Self::Struct(x), Self::Struct(y)) => StructObj::eq(&x.0.borrow(), &y.0.borrow()),
            (Self::Channel(x), Self::Channel(y)) => Rc::ptr_eq(x, y),
            (Self::Named(x), Self::Named(y)) => x.0 == y.0,
            (Self::Nil(_), nil) | (nil, Self::Nil(_)) => nil.equals_nil(),
            (Self::Interface(iface), val) | (val, Self::Interface(iface)) => {
                match iface.borrow().underlying_value() {
                    Some(v) => v == val,
                    None => false,
                }
            }
            _ => false,
        }
    }
}

impl PartialOrd for GosValue {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for GosValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self {
            GosValue::Bool(b) => b.hash(state),
            GosValue::Int(i) => i.hash(state),
            GosValue::Int8(i) => i.hash(state),
            GosValue::Int16(i) => i.hash(state),
            GosValue::Int32(i) => i.hash(state),
            GosValue::Int64(i) => i.hash(state),
            GosValue::Uint(i) => i.hash(state),
            GosValue::UintPtr(i) => i.hash(state),
            GosValue::Uint8(i) => i.hash(state),
            GosValue::Uint16(i) => i.hash(state),
            GosValue::Uint32(i) => i.hash(state),
            GosValue::Uint64(i) => i.hash(state),
            GosValue::Float32(f) => f.to_bits().hash(state),
            GosValue::Float64(f) => f.to_bits().hash(state),
            GosValue::Str(s) => s.as_str().hash(state),
            GosValue::Array(a) => a.0.hash(state),
            GosValue::Complex64(i, r) => {
                i.hash(state);
                r.hash(state);
            }
            GosValue::Complex128(c) => {
                c.0.hash(state);
                c.1.hash(state);
            }
            GosValue::Struct(s) => {
                s.0.borrow().hash(state);
            }
            GosValue::Interface(i) => {
                i.borrow().hash(state);
            }
            GosValue::Pointer(p) => {
                PointerObj::hash(&p, state);
            }
            GosValue::Named(n) => n.0.hash(state),
            _ => unreachable!(),
        }
    }
}

impl Ord for GosValue {
    fn cmp(&self, b: &Self) -> Ordering {
        match (self, b) {
            (Self::Bool(x), Self::Bool(y)) => x.cmp(y),
            (Self::Int(x), Self::Int(y)) => x.cmp(y),
            (Self::Int8(x), Self::Int8(y)) => x.cmp(y),
            (Self::Int16(x), Self::Int16(y)) => x.cmp(y),
            (Self::Int32(x), Self::Int32(y)) => x.cmp(y),
            (Self::Int64(x), Self::Int64(y)) => x.cmp(y),
            (Self::Uint(x), Self::Uint(y)) => x.cmp(y),
            (Self::UintPtr(x), Self::UintPtr(y)) => x.cmp(y),
            (Self::Uint8(x), Self::Uint8(y)) => x.cmp(y),
            (Self::Uint16(x), Self::Uint16(y)) => x.cmp(y),
            (Self::Uint32(x), Self::Uint32(y)) => x.cmp(y),
            (Self::Uint64(x), Self::Uint64(y)) => x.cmp(y),
            (Self::Float32(x), Self::Float32(y)) => x.cmp(y),
            (Self::Float64(x), Self::Float64(y)) => x.cmp(y),
            (Self::Str(x), Self::Str(y)) => x.cmp(y),
            _ => {
                dbg!(self, b);
                unreachable!()
            }
        }
    }
}

impl Display for GosValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GosValue::Nil(_) => f.write_str("<nil>"),
            GosValue::Bool(true) => f.write_str("true"),
            GosValue::Bool(false) => f.write_str("false"),
            GosValue::Int(i) => write!(f, "{}", i),
            GosValue::Int8(i) => write!(f, "{}", i),
            GosValue::Int16(i) => write!(f, "{}", i),
            GosValue::Int32(i) => write!(f, "{}", i),
            GosValue::Int64(i) => write!(f, "{}", i),
            GosValue::Uint(i) => write!(f, "{}", i),
            GosValue::UintPtr(i) => write!(f, "{}", i),
            GosValue::Uint8(i) => write!(f, "{}", i),
            GosValue::Uint16(i) => write!(f, "{}", i),
            GosValue::Uint32(i) => write!(f, "{}", i),
            GosValue::Uint64(i) => write!(f, "{}", i),
            GosValue::Float32(fl) => write!(f, "{}", fl),
            GosValue::Float64(fl) => write!(f, "{}", fl),
            GosValue::Complex64(r, i) => write!(f, "({}, {})", r, i),
            GosValue::Complex128(b) => write!(f, "({}, {})", b.0, b.1),
            GosValue::Str(s) => f.write_str(s.as_ref().as_str()),
            GosValue::Array(a) => write!(f, "{}", a.0),
            GosValue::Pointer(p) => p.fmt(f),
            GosValue::Closure(_) => f.write_str("<closure>"),
            GosValue::Slice(s) => write!(f, "{}", s.0),
            GosValue::Map(m) => write!(f, "{}", m.0),
            GosValue::Interface(i) => write!(f, "{}", i.borrow()),
            GosValue::Struct(s) => write!(f, "{}", s.0.borrow()),
            GosValue::Channel(_) => f.write_str("<channel>"),
            GosValue::Function(_) => f.write_str("<function>"),
            GosValue::Package(_) => f.write_str("<package>"),
            GosValue::Metadata(_) => f.write_str("<metadata>"),
            GosValue::Named(v) => write!(f, "{}", v.0),
        }
    }
}

// ----------------------------------------------------------------------------
// GosValue64
// nil is only allowed on the stack as a rhs value
// never as a lhs var, because when it's assigned to
// we wouldn't know we should release it or not
#[derive(Copy, Clone)]
pub union V64Union {
    nil: (),
    ubool: bool,
    int: isize,
    int8: i8,
    int16: i16,
    int32: i32,
    int64: i64,
    uint: usize,
    uint_ptr: usize,
    uint8: u8,
    uint16: u16,
    uint32: u32,
    uint64: u64,
    float32: F32,
    float64: F64,
    complex64: (F32, F32),
    function: FunctionKey,
    package: PackageKey,
}

impl fmt::Debug for V64Union {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_fmt(format_args!("{:x}", unsafe { self.uint64 }))
    }
}

/// GosValue64 is a 64bit struct for VM stack to get better performance, when converting
/// to GosValue64, the type info is lost, Opcode is responsible for providing type info
/// when converting back to GosValue
#[derive(Copy, Clone, Debug)]
pub struct GosValue64 {
    data: V64Union,
    //pub debug_type: ValueType, // to be removed in release build
}

impl GosValue64 {
    #[inline]
    pub fn from_v128(v: &GosValue) -> Option<GosValue64> {
        match v {
            GosValue::Bool(b) => Some(GosValue64 {
                data: V64Union { ubool: *b },
            }),
            GosValue::Int(i) => Some(GosValue64 {
                data: V64Union { int: *i },
            }),
            GosValue::Int8(i) => Some(GosValue64 {
                data: V64Union { int8: *i },
            }),
            GosValue::Int16(i) => Some(GosValue64 {
                data: V64Union { int16: *i },
            }),
            GosValue::Int32(i) => Some(GosValue64 {
                data: V64Union { int32: *i },
            }),
            GosValue::Int64(i) => Some(GosValue64 {
                data: V64Union { int64: *i },
            }),
            GosValue::Uint(i) => Some(GosValue64 {
                data: V64Union { uint: *i },
            }),
            GosValue::UintPtr(i) => Some(GosValue64 {
                data: V64Union { uint_ptr: *i },
            }),
            GosValue::Uint8(i) => Some(GosValue64 {
                data: V64Union { uint8: *i },
            }),
            GosValue::Uint16(i) => Some(GosValue64 {
                data: V64Union { uint16: *i },
            }),
            GosValue::Uint32(i) => Some(GosValue64 {
                data: V64Union { uint32: *i },
            }),
            GosValue::Uint64(i) => Some(GosValue64 {
                data: V64Union { uint64: *i },
            }),
            GosValue::Float32(f) => Some(GosValue64 {
                data: V64Union { float32: *f },
            }),
            GosValue::Float64(f) => Some(GosValue64 {
                data: V64Union { float64: *f },
            }),
            GosValue::Complex64(f1, f2) => Some(GosValue64 {
                data: V64Union {
                    complex64: (*f1, *f2),
                },
            }),
            GosValue::Function(k) => Some(GosValue64 {
                data: V64Union { function: *k },
            }),
            GosValue::Package(k) => Some(GosValue64 {
                data: V64Union { package: *k },
            }),
            _ => None,
        }
    }

    #[inline]
    pub fn nil() -> GosValue64 {
        GosValue64 {
            data: V64Union { nil: () },
            //debug_type: ValueType::Nil,
        }
    }

    #[inline]
    pub fn from_bool(b: bool) -> GosValue64 {
        GosValue64 {
            data: V64Union { ubool: b },
            //debug_type: ValueType::Bool,
        }
    }

    #[inline]
    pub fn from_int(i: isize) -> GosValue64 {
        GosValue64 {
            data: V64Union { int: i },
            //debug_type: ValueType::Int,
        }
    }

    #[inline]
    pub fn from_int32_as(i: i32, t: ValueType) -> GosValue64 {
        let u = match t {
            ValueType::Int => V64Union { int: i as isize },
            ValueType::Int8 => V64Union { int8: i as i8 },
            ValueType::Int16 => V64Union { int16: i as i16 },
            ValueType::Int32 => V64Union { int32: i as i32 },
            ValueType::Int64 => V64Union { int64: i as i64 },
            ValueType::Uint => V64Union { uint: i as usize },
            ValueType::UintPtr => V64Union {
                uint_ptr: i as usize,
            },
            ValueType::Uint8 => V64Union { uint8: i as u8 },
            ValueType::Uint16 => V64Union { uint16: i as u16 },
            ValueType::Uint32 => V64Union { uint32: i as u32 },
            ValueType::Uint64 => V64Union { uint64: i as u64 },
            ValueType::Float32 => V64Union {
                float32: F32::from(i as f32),
            },
            ValueType::Float64 => V64Union {
                float64: F64::from(i as f64),
            },
            _ => unreachable!(),
        };
        GosValue64 { data: u }
    }

    #[inline]
    pub fn from_float64(f: F64) -> GosValue64 {
        GosValue64 {
            data: V64Union { float64: f },
            //debug_type: ValueType::Float64,
        }
    }

    #[inline]
    pub fn from_complex64(r: F32, i: F32) -> GosValue64 {
        GosValue64 {
            data: V64Union { complex64: (r, i) },
            //debug_type: ValueType::Complex64,
        }
    }

    /// returns GosValue and increases RC
    #[inline]
    pub fn get_v128(&self, t: ValueType) -> GosValue {
        //debug_assert!(t == self.debug_type);
        unsafe {
            match t {
                ValueType::Bool => GosValue::Bool(self.data.ubool),
                ValueType::Int => GosValue::Int(self.data.int),
                ValueType::Int8 => GosValue::Int8(self.data.int8),
                ValueType::Int16 => GosValue::Int16(self.data.int16),
                ValueType::Int32 => GosValue::Int32(self.data.int32),
                ValueType::Int64 => GosValue::Int64(self.data.int64),
                ValueType::Uint => GosValue::Uint(self.data.uint),
                ValueType::UintPtr => GosValue::UintPtr(self.data.uint_ptr),
                ValueType::Uint8 => GosValue::Uint8(self.data.uint8),
                ValueType::Uint16 => GosValue::Uint16(self.data.uint16),
                ValueType::Uint32 => GosValue::Uint32(self.data.uint32),
                ValueType::Uint64 => GosValue::Uint64(self.data.uint64),
                ValueType::Float32 => GosValue::Float32(self.data.float32),
                ValueType::Float64 => GosValue::Float64(self.data.float64),
                ValueType::Complex64 => {
                    GosValue::Complex64(self.data.complex64.0, self.data.complex64.1)
                }
                ValueType::Function => GosValue::Function(self.data.function),
                ValueType::Package => GosValue::Package(self.data.package),
                _ => unreachable!(),
            }
        }
    }

    #[inline]
    pub fn get_bool(&self) -> bool {
        //debug_assert_eq!(self.debug_type, ValueType::Bool);
        unsafe { self.data.ubool }
    }

    #[inline]
    pub fn get_int(&self) -> isize {
        //debug_assert_eq!(self.debug_type, ValueType::Int);
        unsafe { self.data.int }
    }

    #[inline]
    pub fn get_int32(&self) -> i32 {
        unsafe { self.data.int32 }
    }

    #[inline]
    pub fn get_uint(&self) -> usize {
        unsafe { self.data.uint }
    }

    #[inline]
    pub fn get_uint32(&self) -> u32 {
        unsafe { self.data.uint32 }
    }

    #[inline]
    pub fn get_float64(&self) -> F64 {
        //debug_assert_eq!(self.debug_type, ValueType::Float64);
        unsafe { self.data.float64 }
    }

    #[inline]
    pub fn get_complex64(&self) -> (F32, F32) {
        //debug_assert_eq!(self.debug_type, ValueType::Complex64);
        unsafe { self.data.complex64 }
    }

    #[inline]
    pub fn to_uint(&mut self, t: ValueType) {
        convert_to_int!(self, t, uint, usize);
    }

    #[inline]
    pub fn to_uint_ptr(&mut self, t: ValueType) {
        convert_to_int!(self, t, uint_ptr, usize);
    }

    #[inline]
    pub fn to_uint8(&mut self, t: ValueType) {
        convert_to_int!(self, t, uint8, u8);
    }

    #[inline]
    pub fn to_uint16(&mut self, t: ValueType) {
        convert_to_int!(self, t, uint16, u16);
    }

    #[inline]
    pub fn to_uint32(&mut self, t: ValueType) {
        convert_to_int!(self, t, uint32, u32);
    }

    #[inline]
    pub fn to_uint64(&mut self, t: ValueType) {
        convert_to_int!(self, t, uint64, u64);
    }

    #[inline]
    pub fn to_int(&mut self, t: ValueType) {
        convert_to_int!(self, t, int, isize);
    }

    #[inline]
    pub fn to_int8(&mut self, t: ValueType) {
        convert_to_int!(self, t, int8, i8);
    }

    #[inline]
    pub fn to_int16(&mut self, t: ValueType) {
        convert_to_int!(self, t, int16, i16);
    }

    #[inline]
    pub fn to_int32(&mut self, t: ValueType) {
        convert_to_int!(self, t, int32, i32);
    }

    #[inline]
    pub fn to_int64(&mut self, t: ValueType) {
        convert_to_int!(self, t, int64, i64);
    }

    #[inline]
    pub fn to_float32(&mut self, t: ValueType) {
        convert_to_float!(self, t, float32, F32, f32);
    }

    #[inline]
    pub fn to_float64(&mut self, t: ValueType) {
        convert_to_float!(self, t, float64, F64, f64);
    }

    #[inline]
    pub fn unary_negate(&mut self, t: ValueType) {
        match t {
            ValueType::Int => self.data.int = -unsafe { self.data.int },
            ValueType::Int8 => self.data.int8 = -unsafe { self.data.int8 },
            ValueType::Int16 => self.data.int16 = -unsafe { self.data.int16 },
            ValueType::Int32 => self.data.int32 = -unsafe { self.data.int32 },
            ValueType::Int64 => self.data.int64 = -unsafe { self.data.int64 },
            ValueType::Float32 => self.data.float32 = -unsafe { self.data.float32 },
            ValueType::Float64 => self.data.float64 = -unsafe { self.data.float64 },
            ValueType::Uint => self.data.uint = unsafe { (!0) ^ self.data.uint } + 1,
            ValueType::Uint8 => self.data.uint8 = unsafe { (!0) ^ self.data.uint8 } + 1,
            ValueType::Uint16 => self.data.uint16 = unsafe { (!0) ^ self.data.uint16 } + 1,
            ValueType::Uint32 => self.data.uint32 = unsafe { (!0) ^ self.data.uint32 } + 1,
            ValueType::Uint64 => self.data.uint64 = unsafe { (!0) ^ self.data.uint64 } + 1,
            _ => unreachable!(),
        }
    }

    #[inline]
    pub fn unary_xor(&mut self, t: ValueType) {
        match t {
            ValueType::Uint => self.data.uint = unsafe { (!0) ^ self.data.uint },
            ValueType::Uint8 => self.data.uint8 = unsafe { (!0) ^ self.data.uint8 },
            ValueType::Uint16 => self.data.uint16 = unsafe { (!0) ^ self.data.uint16 },
            ValueType::Uint32 => self.data.uint32 = unsafe { (!0) ^ self.data.uint32 },
            ValueType::Uint64 => self.data.uint64 = unsafe { (!0) ^ self.data.uint64 },
            ValueType::Int => self.data.int = unsafe { -1 ^ self.data.int },
            ValueType::Int8 => self.data.int8 = unsafe { -1 ^ self.data.int8 },
            ValueType::Int16 => self.data.int16 = unsafe { -1 ^ self.data.int16 },
            ValueType::Int32 => self.data.int32 = unsafe { -1 ^ self.data.int32 },
            ValueType::Int64 => self.data.int64 = unsafe { -1 ^ self.data.int64 },
            _ => unreachable!(),
        }
    }

    #[inline]
    pub fn unary_not(&mut self, t: ValueType) {
        debug_assert!(t == ValueType::Bool);
        self.data.ubool = unsafe { !self.data.ubool };
    }

    #[inline]
    pub fn binary_op_add(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_float!(t, a, b, +) }
    }

    #[inline]
    pub fn binary_op_sub(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_float!(t, a, b, -) }
    }

    #[inline]
    pub fn binary_op_mul(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_float!(t, a, b, *) }
    }

    #[inline]
    pub fn binary_op_quo(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_float!(t, a, b, /) }
    }

    #[inline]
    pub fn binary_op_rem(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_no_wrap!(t, a, b, %) }
    }

    #[inline]
    pub fn binary_op_and(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_no_wrap!(t, a, b, &) }
    }

    #[inline]
    pub fn binary_op_or(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_no_wrap!(t, a, b, |) }
    }

    #[inline]
    pub fn binary_op_xor(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        unsafe { binary_op_int_no_wrap!(t, a, b, ^) }
    }

    #[inline]
    pub fn binary_op_shl(&mut self, b: u32, t: ValueType) {
        unsafe { shift_int!(t, self, b, checked_shl) }
    }

    #[inline]
    pub fn binary_op_shr(&mut self, b: u32, t: ValueType) {
        unsafe { shift_int!(t, self, b, checked_shr) }
    }

    #[inline]
    pub fn binary_op_and_not(a: &GosValue64, b: &GosValue64, t: ValueType) -> GosValue64 {
        GosValue64 {
            //debug_type: t,
            data: unsafe {
                match t {
                    ValueType::Int => V64Union {
                        int: a.data.int & !b.data.int,
                    },
                    ValueType::Int8 => V64Union {
                        int8: a.data.int8 & !b.data.int8,
                    },
                    ValueType::Int16 => V64Union {
                        int16: a.data.int16 & !b.data.int16,
                    },
                    ValueType::Int32 => V64Union {
                        int32: a.data.int32 & !b.data.int32,
                    },
                    ValueType::Int64 => V64Union {
                        int64: a.data.int64 & !b.data.int64,
                    },
                    ValueType::Uint => V64Union {
                        uint: a.data.uint & !b.data.uint,
                    },
                    ValueType::Uint8 => V64Union {
                        uint8: a.data.uint8 & !b.data.uint8,
                    },
                    ValueType::Uint16 => V64Union {
                        uint16: a.data.uint16 & !b.data.uint16,
                    },
                    ValueType::Uint32 => V64Union {
                        uint32: a.data.uint32 & !b.data.uint32,
                    },
                    ValueType::Uint64 => V64Union {
                        uint64: a.data.uint64 & !b.data.uint64,
                    },
                    _ => unreachable!(),
                }
            },
        }
    }

    #[inline]
    pub fn binary_op(a: &GosValue64, b: &GosValue64, t: ValueType, op: Opcode) -> GosValue64 {
        match op {
            Opcode::ADD => GosValue64::binary_op_add(a, b, t),
            Opcode::SUB => GosValue64::binary_op_sub(a, b, t),
            Opcode::MUL => GosValue64::binary_op_mul(a, b, t),
            Opcode::QUO => GosValue64::binary_op_quo(a, b, t),
            Opcode::REM => GosValue64::binary_op_rem(a, b, t),
            Opcode::AND => GosValue64::binary_op_and(a, b, t),
            Opcode::OR => GosValue64::binary_op_or(a, b, t),
            Opcode::XOR => GosValue64::binary_op_xor(a, b, t),
            Opcode::AND_NOT => GosValue64::binary_op_and_not(a, b, t),
            Opcode::SHL => {
                let mut v = a.clone();
                v.binary_op_shl(b.get_uint32(), t);
                v
            }
            Opcode::SHR => {
                let mut v = a.clone();
                v.binary_op_shr(b.get_uint32(), t);
                v
            }
            _ => {
                dbg!(t, op);
                unreachable!()
            }
        }
    }

    #[inline]
    pub fn compare_eql(a: &GosValue64, b: &GosValue64, t: ValueType) -> bool {
        unsafe { cmp_bool_int_float!(t, a, b, ==) }
    }

    #[inline]
    pub fn compare_neq(a: &GosValue64, b: &GosValue64, t: ValueType) -> bool {
        unsafe { cmp_bool_int_float!(t, a, b, !=) }
    }

    #[inline]
    pub fn compare_lss(a: &GosValue64, b: &GosValue64, t: ValueType) -> bool {
        unsafe { cmp_int_float!(t, a, b, <) }
    }

    #[inline]
    pub fn compare_gtr(a: &GosValue64, b: &GosValue64, t: ValueType) -> bool {
        unsafe { cmp_int_float!(t, a, b, >) }
    }

    #[inline]
    pub fn compare_leq(a: &GosValue64, b: &GosValue64, t: ValueType) -> bool {
        unsafe { cmp_int_float!(t, a, b, <=) }
    }

    #[inline]
    pub fn compare_geq(a: &GosValue64, b: &GosValue64, t: ValueType) -> bool {
        unsafe { cmp_int_float!(t, a, b, >=) }
    }
}

#[cfg(test)]
mod test {
    use super::super::value::*;
    use std::collections::HashMap;
    use std::mem;

    #[test]
    fn test_types() {
        let _t1: Vec<GosValue> = vec![
            GosValue::new_str("Norway".to_string()),
            GosValue::Int(100),
            GosValue::new_str("Denmark".to_string()),
            GosValue::Int(10),
        ];

        let _t2: Vec<GosValue> = vec![
            GosValue::new_str("Norway".to_string()),
            GosValue::Int(100),
            GosValue::new_str("Denmark".to_string()),
            GosValue::Int(10),
        ];
    }

    #[test]
    fn test_size() {
        dbg!(mem::size_of::<HashMap<GosValue, GosValue>>());
        dbg!(mem::size_of::<String>());
        dbg!(mem::size_of::<Rc<String>>());
        dbg!(mem::size_of::<SliceObj>());
        dbg!(mem::size_of::<RefCell<GosValue>>());
        dbg!(mem::size_of::<GosValue>());
        dbg!(mem::size_of::<GosValue64>());

        let mut h: HashMap<isize, isize> = HashMap::new();
        h.insert(0, 1);
        let mut h2 = h.clone();
        h2.insert(0, 3);
        dbg!(h[&0]);
        dbg!(h2[&0]);
    }
}
