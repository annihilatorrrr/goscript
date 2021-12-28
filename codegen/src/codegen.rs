use std::collections::HashMap;
use std::convert::TryFrom;
use std::iter::FromIterator;
use std::rc::Rc;

use super::branch::*;
use super::call::CallHelper;
use super::emit::*;
use super::interface::IfaceMapping;
use super::package::PkgHelper;
use super::types::{TypeCache, TypeLookup};

use goscript_vm::gc::GcoVec;
use goscript_vm::instruction::*;
use goscript_vm::metadata::*;
use goscript_vm::objects::EntIndex;
use goscript_vm::value::*;
use goscript_vm::zero_val;

use goscript_parser::ast::*;
use goscript_parser::objects::Objects as AstObjects;
use goscript_parser::objects::*;
use goscript_parser::position::Pos;
use goscript_parser::token::Token;
use goscript_parser::visitor::{walk_decl, walk_expr, walk_stmt, ExprVisitor, StmtVisitor};
use goscript_types::{
    identical, Builtin, OperandMode, PackageKey as TCPackageKey, TCObjects, TypeInfo,
    TypeKey as TCTypeKey,
};

macro_rules! current_func_mut {
    ($owner:ident) => {
        &mut $owner.objects.functions[*$owner.func_stack.last().unwrap()]
    };
}

macro_rules! current_func {
    ($owner:ident) => {
        &$owner.objects.functions[*$owner.func_stack.last().unwrap()]
    };
}

macro_rules! current_func_emitter {
    ($owner:ident) => {
        Emitter::new(current_func_mut!($owner))
    };
}

enum ReceiverPreprocess {
    Default,
    Ref,   // take ref of receiver before binding method
    Deref, // deref receiver before binding method
}

/// CodeGen implements the code generation logic.
pub struct CodeGen<'a> {
    objects: &'a mut VMObjects,
    ast_objs: &'a AstObjects,
    tc_objs: &'a TCObjects,
    dummy_gcv: &'a mut GcoVec,
    tlookup: TypeLookup<'a>,
    iface_mapping: &'a mut IfaceMapping,
    call_helper: &'a mut CallHelper,
    pkg_helper: PkgHelper<'a>,
    branch: BranchHelper,
    pkg_key: PackageKey,
    func_stack: Vec<FunctionKey>,
    func_t_stack: Vec<TCTypeKey>, // for casting return values to interfaces
    blank_ident: IdentKey,
}

impl<'a> CodeGen<'a> {
    pub fn new(
        vmo: &'a mut VMObjects,
        asto: &'a AstObjects,
        tco: &'a TCObjects,
        dummy_gcv: &'a mut GcoVec,
        ti: &'a TypeInfo,
        type_cache: &'a mut TypeCache,
        mapping: &'a mut IfaceMapping,
        call_helper: &'a mut CallHelper,
        pkg_indices: &'a HashMap<TCPackageKey, OpIndex>,
        pkgs: &'a Vec<PackageKey>,
        pkg: PackageKey,
        bk: IdentKey,
    ) -> CodeGen<'a> {
        let unsafe_ptr_meta = vmo.metadata.unsafe_ptr.clone();
        CodeGen {
            objects: vmo,
            ast_objs: asto,
            tc_objs: tco,
            dummy_gcv: dummy_gcv,
            tlookup: TypeLookup::new(tco, ti, type_cache, unsafe_ptr_meta),
            iface_mapping: mapping,
            call_helper: call_helper,
            pkg_helper: PkgHelper::new(asto, tco, pkg_indices, pkgs),
            branch: BranchHelper::new(),
            pkg_key: pkg,
            func_stack: Vec::new(),
            func_t_stack: Vec::new(),
            blank_ident: bk,
        }
    }

    pub fn pkg_helper(&mut self) -> &mut PkgHelper<'a> {
        &mut self.pkg_helper
    }

    fn resolve_any_ident(&mut self, ident: &IdentKey, expr: Option<&Expr>) -> EntIndex {
        let id = &self.ast_objs.idents[*ident];
        match id.entity_key() {
            None => match expr.map_or(&OperandMode::Value, |x| self.tlookup.get_expr_mode(x)) {
                OperandMode::TypeExpr => {
                    let lookup = &self.tlookup;
                    let tctype = lookup.underlying_tc(lookup.get_use_tc_type(*ident));
                    let meta = lookup.basic_type_from_tc(tctype, self.objects);
                    EntIndex::BuiltInType(meta)
                }
                OperandMode::Value => match &*id.name {
                    "true" => EntIndex::BuiltInVal(Opcode::PUSH_TRUE),
                    "false" => EntIndex::BuiltInVal(Opcode::PUSH_FALSE),
                    "nil" => EntIndex::BuiltInVal(Opcode::PUSH_NIL),
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            },
            Some(_) => self.resolve_var_ident(ident),
        }
    }

    fn resolve_var_ident(&mut self, ident: &IdentKey) -> EntIndex {
        let entity_key = &self.ast_objs.idents[*ident].entity_key().unwrap();
        // 1. try local first
        if let Some(index) = current_func!(self).entity_index(&entity_key).map(|x| *x) {
            return index;
        }
        // 2. try upvalue
        let upvalue = self
            .func_stack
            .clone()
            .iter()
            .skip(1) // skip package constructor
            .rev()
            .skip(1) // skip itself
            .find_map(|ifunc| {
                let f = &mut self.objects.functions[*ifunc];
                let index = f.entity_index(&entity_key).map(|x| *x);
                if let Some(ind) = index {
                    let desc = ValueDesc::new(
                        *ifunc,
                        ind.into(),
                        self.tlookup.get_use_value_type(*ident),
                        true,
                    );
                    Some(desc)
                } else {
                    None
                }
            });
        if let Some(uv) = upvalue {
            let func = current_func_mut!(self);
            let index = func.try_add_upvalue(&entity_key, uv);
            return index;
        }
        // 3. must be package member
        EntIndex::PackageMember(self.pkg_key, *ident)
    }

    fn add_local_or_resolve_ident(
        &mut self,
        ikey: &IdentKey,
        is_def: bool,
    ) -> (EntIndex, Option<TCTypeKey>, usize) {
        let ident = &self.ast_objs.idents[*ikey];
        let pos = ident.pos;
        if ident.is_blank() {
            return (EntIndex::Blank, None, pos);
        }
        if is_def {
            let meta = self
                .tlookup
                .gen_def_type_meta(*ikey, self.objects, self.dummy_gcv);
            let zero_val = zero_val!(meta, self.objects, self.dummy_gcv);
            let func = current_func_mut!(self);
            let ident_key = ident.entity.clone().into_key();
            let index = func.add_local(ident_key);
            func.add_local_zero(zero_val);
            if func.is_ctor() {
                let pkg_key = func.package;
                let pkg = &mut self.objects.packages[pkg_key];
                pkg.add_var_mapping(ident.name.clone(), index.into());
            }
            let t = self.tlookup.get_def_tc_type(*ikey);
            (index, Some(t), pos)
        } else {
            let index = self.resolve_var_ident(ikey);
            let t = self.tlookup.get_use_tc_type(*ikey);
            (index, Some(t), pos)
        }
    }

    fn gen_def_var(&mut self, vs: &ValueSpec) {
        let lhs = vs
            .names
            .iter()
            .map(|n| -> (LeftHandSide, Option<TCTypeKey>, usize) {
                let (index, t, pos) = self.add_local_or_resolve_ident(n, true);
                (LeftHandSide::Primitive(index), t, pos)
            })
            .collect::<Vec<(LeftHandSide, Option<TCTypeKey>, usize)>>();
        let rhs = if vs.values.is_empty() {
            RightHandSide::Nothing
        } else {
            RightHandSide::Values(&vs.values)
        };
        self.gen_assign_def_var(&lhs, &vs.typ, &rhs);
    }

    fn gen_def_const(&mut self, names: &Vec<IdentKey>, values: &Vec<Expr>) {
        assert!(names.len() == values.len());
        for i in 0..names.len() {
            let ident = self.ast_objs.idents[names[i]].clone();
            let val = self.tlookup.get_const_value(values[i].id());
            self.current_func_add_const_def(&ident, val);
        }
    }

    /// entrance for all assign related stmts
    /// var x
    /// x := 0
    /// x += 1
    /// x++
    /// for x := range xxx
    /// recv clause of select stmt
    fn gen_assign(
        &mut self,
        token: &Token,
        lhs_exprs: &Vec<&Expr>,
        rhs: RightHandSide,
    ) -> Option<usize> {
        let lhs = lhs_exprs
            .iter()
            .map(|expr| {
                match expr {
                    Expr::Ident(ident) => {
                        // cannot only determined by token, because it could be a mixture
                        let mut is_def = *token == Token::DEFINE;
                        if is_def {
                            let entity = &self.ast_objs.idents[*ident].entity_key();
                            is_def = entity.is_some()
                                && current_func!(self)
                                    .entity_index(entity.as_ref().unwrap())
                                    .is_none();
                        }
                        let (idx, t, p) = self.add_local_or_resolve_ident(ident, is_def);
                        (LeftHandSide::Primitive(idx), t, p)
                    }
                    Expr::Index(ind_expr) => {
                        let obj = &ind_expr.as_ref().expr;
                        self.visit_expr(obj);
                        let obj_typ = self.tlookup.get_expr_value_type(obj);
                        let ind = &ind_expr.as_ref().index;
                        let pos = ind_expr.as_ref().l_brack;

                        let mut index_const = None;
                        let mut index_typ = None;
                        if let Some(const_val) = self.tlookup.get_tc_const_value(ind.id()) {
                            let (ival, _) = const_val.to_int().int_as_i64();
                            if let Ok(i) = OpIndex::try_from(ival) {
                                index_const = Some(i);
                            }
                        }
                        if index_const.is_none() {
                            self.visit_expr(ind);
                            index_typ = Some(self.tlookup.get_expr_value_type(ind));
                        }
                        (
                            LeftHandSide::IndexSelExpr(IndexSelInfo::new(
                                0,
                                index_const,
                                obj_typ,
                                index_typ,
                                IndexSelType::Indexing,
                            )), // the true index will be calculated later
                            Some(self.tlookup.get_expr_tc_type(expr)),
                            pos,
                        )
                    }
                    Expr::Selector(sexpr) => {
                        let pos = self.ast_objs.idents[sexpr.sel].pos;
                        match self.tlookup.try_get_pkg_key(&sexpr.expr) {
                            Some(key) => {
                                let pkg = self.pkg_helper.get_vm_pkg(key);
                                //let t = self.tlookup.get_use_value_type(sexpr.sel);
                                (
                                    // the true index will be calculated later
                                    LeftHandSide::Primitive(EntIndex::PackageMember(
                                        pkg, sexpr.sel,
                                    )),
                                    Some(self.tlookup.get_expr_tc_type(expr)),
                                    pos,
                                )
                            }
                            None => {
                                let t = self.tlookup.get_meta_by_node_id(
                                    sexpr.expr.id(),
                                    self.objects,
                                    self.dummy_gcv,
                                );
                                let name = &self.ast_objs.idents[sexpr.sel].name;
                                let i = t.field_index(name, &self.objects.metas);

                                self.visit_expr(&sexpr.expr);
                                let obj_typ = self.tlookup.get_expr_value_type(&sexpr.expr);
                                (
                                    // the true index will be calculated later
                                    LeftHandSide::IndexSelExpr(IndexSelInfo::new(
                                        0,
                                        Some(i),
                                        obj_typ,
                                        None,
                                        IndexSelType::StructField,
                                    )),
                                    Some(self.tlookup.get_expr_tc_type(expr)),
                                    pos,
                                )
                            }
                        }
                    }
                    Expr::Star(sexpr) => {
                        self.visit_expr(&sexpr.expr);
                        (
                            LeftHandSide::Deref(0), // the true index will be calculated later
                            Some(self.tlookup.get_expr_tc_type(expr)),
                            sexpr.star,
                        )
                    }
                    _ => unreachable!(),
                }
            })
            .collect::<Vec<(LeftHandSide, Option<TCTypeKey>, usize)>>();

        match rhs {
            RightHandSide::Nothing => {
                let code = match token {
                    Token::INC => Opcode::ADD,
                    Token::DEC => Opcode::SUB,
                    _ => unreachable!(),
                };
                let typ = self.tlookup.get_expr_value_type(&lhs_exprs[0]);
                self.gen_op_assign(&lhs[0].0, (code, None), None, typ, lhs[0].2);
                None
            }
            RightHandSide::Values(rhs_exprs) => {
                let simple_op = match token {
                    Token::ADD_ASSIGN => Some(Opcode::ADD),         // +=
                    Token::SUB_ASSIGN => Some(Opcode::SUB),         // -=
                    Token::MUL_ASSIGN => Some(Opcode::MUL),         // *=
                    Token::QUO_ASSIGN => Some(Opcode::QUO),         // /=
                    Token::REM_ASSIGN => Some(Opcode::REM),         // %=
                    Token::AND_ASSIGN => Some(Opcode::AND),         // &=
                    Token::OR_ASSIGN => Some(Opcode::OR),           // |=
                    Token::XOR_ASSIGN => Some(Opcode::XOR),         // ^=
                    Token::SHL_ASSIGN => Some(Opcode::SHL),         // <<=
                    Token::SHR_ASSIGN => Some(Opcode::SHR),         // >>=
                    Token::AND_NOT_ASSIGN => Some(Opcode::AND_NOT), // &^=
                    Token::ASSIGN | Token::DEFINE => None,
                    _ => unreachable!(),
                };
                if let Some(code) = simple_op {
                    assert_eq!(lhs_exprs.len(), 1);
                    assert_eq!(rhs_exprs.len(), 1);
                    let ltyp = self.tlookup.get_expr_value_type(&lhs_exprs[0]);
                    let rtyp = match code {
                        Opcode::SHL | Opcode::SHR => {
                            Some(self.tlookup.get_expr_value_type(&rhs_exprs[0]))
                        }
                        _ => None,
                    };
                    self.gen_op_assign(
                        &lhs[0].0,
                        (code, rtyp),
                        Some(&rhs_exprs[0]),
                        ltyp,
                        lhs[0].2,
                    );
                    None
                } else {
                    self.gen_assign_def_var(&lhs, &None, &rhs)
                }
            }
            _ => self.gen_assign_def_var(&lhs, &None, &rhs),
        }
    }

    fn gen_assign_def_var(
        &mut self,
        lhs: &Vec<(LeftHandSide, Option<TCTypeKey>, usize)>,
        typ: &Option<Expr>,
        rhs: &RightHandSide,
    ) -> Option<usize> {
        let mut range_marker = None;
        // handle the right hand side
        let types = match rhs {
            RightHandSide::Nothing => {
                // define without values
                let (val, t) = self.get_type_default(&typ.as_ref().unwrap());
                let mut types = Vec::with_capacity(lhs.len());
                for (_, _, pos) in lhs.iter() {
                    let mut emitter = current_func_emitter!(self);
                    let i = emitter.add_const(None, val.clone());
                    emitter.emit_load(i, None, self.tlookup.value_type_from_tc(t), Some(*pos));
                    types.push(t);
                }
                types
            }
            RightHandSide::Values(values) => {
                let val0 = &values[0];
                let val0_mode = self.tlookup.get_expr_mode(val0);
                if values.len() == 1
                    && (val0_mode == &OperandMode::CommaOk || val0_mode == &OperandMode::MapIndex)
                {
                    let comma_ok = lhs.len() == 2;
                    match val0 {
                        Expr::TypeAssert(tae) => {
                            self.visit_expr(&tae.expr);
                            let t = self.tlookup.get_expr_tc_type(tae.typ.as_ref().unwrap());
                            let meta = self.tlookup.meta_from_tc(t, self.objects, self.dummy_gcv);
                            let func = current_func_mut!(self);
                            let index = func.add_const(None, GosValue::Metadata(meta));
                            func.emit_code_with_flag_imm(
                                Opcode::TYPE_ASSERT,
                                comma_ok,
                                index.into(),
                                Some(tae.l_paren),
                            );
                        }
                        Expr::Index(ie) => {
                            self.gen_map_index(&ie.expr, &ie.index, comma_ok);
                        }
                        Expr::Unary(recv_expr) => {
                            assert_eq!(recv_expr.op, Token::ARROW);
                            self.visit_expr(&recv_expr.expr);
                            let t = self.tlookup.get_expr_value_type(&recv_expr.expr);
                            assert_eq!(t, ValueType::Channel);
                            let comma_ok_flag = comma_ok.then(|| ValueType::FlagA);
                            current_func_mut!(self).emit_code_with_type2(
                                Opcode::RECV,
                                t,
                                comma_ok_flag,
                                Some(recv_expr.op_pos),
                            );
                        }
                        _ => {
                            dbg!(val0, val0_mode);
                            unreachable!()
                        }
                    }
                    if comma_ok {
                        self.tlookup.get_tuple_tc_types(val0)
                    } else {
                        vec![self.tlookup.get_expr_tc_type(val0)]
                    }
                } else if values.len() == lhs.len() {
                    // define or assign with values
                    let mut types = Vec::with_capacity(values.len());
                    for val in values.iter() {
                        self.visit_expr(val);
                        let rhs_type = self.tlookup.get_expr_tc_type(val);
                        types.push(rhs_type);
                    }
                    types
                } else if values.len() == 1 {
                    let expr = val0;
                    // define or assign with function call on the right
                    if let Expr::Call(_) = expr {
                        self.visit_expr(expr);
                    } else {
                        unreachable!()
                    }
                    self.tlookup.get_tuple_tc_types(expr)
                } else {
                    unreachable!();
                }
            }
            RightHandSide::Range(r) => {
                // the range statement
                self.visit_expr(r);
                let tkv = self.tlookup.get_range_tc_types(r);
                let types = [
                    Some(self.tlookup.value_type_from_tc(tkv[0])),
                    Some(self.tlookup.value_type_from_tc(tkv[1])),
                    Some(self.tlookup.value_type_from_tc(tkv[2])),
                ];
                let pos = Some(r.pos(&self.ast_objs));
                //current_func_emitter!(self).emit_push_imm(ValueType::Int, -1, pos);
                let func = current_func_mut!(self);
                func.emit_inst(Opcode::RANGE_INIT, types, None, pos);
                range_marker = Some(func.next_code_index());
                // the block_end address to be set
                func.emit_inst(Opcode::RANGE, types, None, pos);
                tkv[1..].to_vec()
            }
            RightHandSide::SelectRecv(rhs) => {
                let comma_ok =
                    lhs.len() == 2 && self.tlookup.get_expr_mode(rhs) == &OperandMode::CommaOk;
                if comma_ok {
                    self.tlookup.get_tuple_tc_types(rhs)
                } else {
                    vec![self.tlookup.get_expr_tc_type(rhs)]
                }
            }
        };

        // now the values should be on stack, generate code to set them to the lhs
        let total_lhs_stack_space = lhs.iter().fold(0, |acc, (x, _, _)| match x {
            LeftHandSide::Primitive(_) => acc,
            LeftHandSide::IndexSelExpr(info) => acc + info.stack_space(),
            LeftHandSide::Deref(_) => acc + 1,
        });
        // only when in select stmt, lhs in stack is on top of the rhs
        let lhs_on_stack_top = if let RightHandSide::SelectRecv(_) = rhs {
            true
        } else {
            false
        };

        assert_eq!(lhs.len(), types.len());
        let total_val = types.len() as OpIndex;
        let total_stack_space = (total_lhs_stack_space + total_val) as OpIndex;
        let mut current_indexing_deref_index = -if lhs_on_stack_top {
            total_lhs_stack_space
        } else {
            total_stack_space
        };
        for (i, (l, _, p)) in lhs.iter().enumerate() {
            let rhs_index = i as OpIndex
                - if lhs_on_stack_top {
                    total_stack_space
                } else {
                    total_val
                };
            let typ = self.try_cast_to_iface(lhs[i].1, Some(types[i]), rhs_index, *p);
            let pos = Some(*p);
            match l {
                LeftHandSide::Primitive(_) => {
                    current_func_emitter!(self).emit_store(l, rhs_index, None, None, typ, pos);
                }
                LeftHandSide::IndexSelExpr(info) => {
                    current_func_emitter!(self).emit_store(
                        &LeftHandSide::IndexSelExpr(info.with_index(current_indexing_deref_index)),
                        rhs_index,
                        None,
                        None,
                        typ,
                        pos,
                    );
                    // the lhs of IndexSelExpr takes two spots
                    current_indexing_deref_index += 2;
                }
                LeftHandSide::Deref(_) => {
                    current_func_emitter!(self).emit_store(
                        &LeftHandSide::Deref(current_indexing_deref_index),
                        rhs_index,
                        None,
                        None,
                        typ,
                        pos,
                    );
                    current_indexing_deref_index += 1;
                }
            }
        }

        // pop rhs
        let mut total_pop = types.iter().count() as OpIndex;
        // pop lhs
        for (i, _, _) in lhs.iter().rev() {
            match i {
                LeftHandSide::Primitive(_) => {}
                LeftHandSide::IndexSelExpr(info) => {
                    if let Some(_t) = info.t2 {
                        total_pop += 1;
                    }
                    total_pop += 1;
                }
                LeftHandSide::Deref(_) => total_pop += 1,
            }
        }
        let pos = Some(lhs[0].2);
        current_func_emitter!(self).emit_pop(total_pop, pos);
        range_marker
    }

    fn gen_op_assign(
        &mut self,
        left: &LeftHandSide,
        op: (Opcode, Option<ValueType>),
        right: Option<&Expr>,
        typ: ValueType,
        p: usize,
    ) {
        let pos = Some(p);
        if let Some(e) = right {
            self.visit_expr(e);
        } else {
            // it's inc/dec
            current_func_emitter!(self).emit_push_imm(typ, 1, pos);
        }
        match left {
            LeftHandSide::Primitive(_) => {
                // why no magic number?
                // local index is resolved in gen_assign
                let mut emitter = current_func_emitter!(self);
                let fkey = self.func_stack.last().unwrap();
                emitter.emit_store(
                    left,
                    -1,
                    Some(op),
                    Some((self.pkg_helper.pairs_mut(), *fkey)),
                    typ,
                    pos,
                );
                emitter.emit_pop(1, pos);
            }
            LeftHandSide::IndexSelExpr(info) => {
                // stack looks like this(bottom to top) :
                //  [... target, index, value] or [... target, value]
                current_func_emitter!(self).emit_store(
                    &LeftHandSide::IndexSelExpr(info.with_index(-info.stack_space() - 1)),
                    -1,
                    Some(op),
                    None,
                    typ,
                    pos,
                );
                let mut total_pop = 2;
                if let Some(_) = info.t2 {
                    total_pop += 1;
                }
                current_func_emitter!(self).emit_pop(total_pop, pos);
            }
            LeftHandSide::Deref(_) => {
                // why -2?  stack looks like this(bottom to top) :
                //  [... target, value]
                let mut emitter = current_func_emitter!(self);
                emitter.emit_store(&LeftHandSide::Deref(-2), -1, Some(op), None, typ, pos);
                emitter.emit_pop(2, pos);
            }
        }
    }

    fn gen_switch_body(&mut self, body: &BlockStmt, tag_type: ValueType) {
        let mut helper = SwitchHelper::new();
        let mut has_default = false;
        for (i, stmt) in body.list.iter().enumerate() {
            helper.add_case_clause();
            let cc = SwitchHelper::to_case_clause(stmt);
            match &cc.list {
                Some(l) => {
                    for c in l.iter() {
                        let pos = Some(stmt.pos(&self.ast_objs));
                        self.visit_expr(c);
                        let func = current_func_mut!(self);
                        helper.tags.add_case(i, func.next_code_index());
                        func.emit_code_with_type(Opcode::SWITCH, tag_type, pos);
                    }
                }
                None => has_default = true,
            }
        }
        if has_default {
            let func = current_func_mut!(self);
            helper.tags.add_default(func.next_code_index());
            func.emit_code(Opcode::JUMP, None);
        }

        for (i, stmt) in body.list.iter().enumerate() {
            let cc = SwitchHelper::to_case_clause(stmt);
            let func = current_func_mut!(self);
            let default = cc.list.is_none();
            if default {
                helper.tags.patch_default(func, func.next_code_index());
            } else {
                helper.tags.patch_case(func, i, func.next_code_index());
            }
            for s in cc.body.iter() {
                self.visit_stmt(s);
            }
            if !SwitchHelper::has_fall_through(stmt) {
                let func = current_func_mut!(self);
                if default {
                    helper.ends.add_default(func.next_code_index());
                } else {
                    helper.ends.add_case(i, func.next_code_index());
                }
                func.emit_code(Opcode::JUMP, None);
            }
        }
        let end = current_func!(self).next_code_index();
        helper.patch_ends(current_func_mut!(self), end);

        // pop the tag
        current_func_emitter!(self).emit_pop(1, None);
    }

    fn gen_func_def(
        &mut self,
        tc_type: TCTypeKey, // GosMetadata,
        fkey: FuncTypeKey,
        recv: Option<FieldList>,
        body: &BlockStmt,
    ) -> FunctionKey {
        let typ = &self.ast_objs.ftypes[fkey];
        let fmeta = self
            .tlookup
            .meta_from_tc(tc_type, &mut self.objects, self.dummy_gcv);
        let f = GosValue::new_function(
            self.pkg_key,
            fmeta,
            self.objects,
            self.dummy_gcv,
            FuncFlag::Default,
        );
        let fkey = *f.as_function();
        let mut emitter = Emitter::new(&mut self.objects.functions[fkey]);
        if let Some(fl) = &typ.results {
            emitter.add_params(&fl, self.ast_objs);
        }
        match recv {
            Some(recv) => {
                let mut fields = recv;
                fields.list.append(&mut typ.params.list.clone());
                emitter.add_params(&fields, self.ast_objs)
            }
            None => emitter.add_params(&typ.params, self.ast_objs),
        };
        self.func_stack.push(fkey);
        self.func_t_stack.push(tc_type);
        // process function body
        self.visit_stmt_block(body);
        // it will not be executed if it's redundant
        Emitter::new(&mut self.objects.functions[fkey]).emit_return(None, Some(body.r_brace));

        self.func_stack.pop();
        self.func_t_stack.pop();
        fkey
    }

    fn gen_call(&mut self, func_expr: &Expr, params: &Vec<Expr>, ellipsis: bool, style: CallStyle) {
        let pos = Some(func_expr.pos(&self.ast_objs));
        match *self.tlookup.get_expr_mode(func_expr) {
            // built in function
            OperandMode::Builtin(builtin) => {
                let opcode = match builtin {
                    Builtin::New => Opcode::NEW,
                    Builtin::Make => Opcode::MAKE,
                    Builtin::Len => Opcode::LEN,
                    Builtin::Cap => Opcode::CAP,
                    Builtin::Append => Opcode::APPEND,
                    Builtin::Close => Opcode::CLOSE,
                    Builtin::Panic => Opcode::PANIC,
                    Builtin::Recover => Opcode::RECOVER,
                    Builtin::Assert => Opcode::ASSERT,
                    Builtin::Ffi => Opcode::FFI,
                    _ => unimplemented!(),
                };
                for e in params.iter() {
                    self.visit_expr(e);
                }
                // some of the built in funcs are not recorded
                if let Some(t) = self.tlookup.try_get_expr_tc_type(func_expr) {
                    self.try_cast_params_to_iface(t, params, ellipsis);
                    if opcode == Opcode::FFI {
                        // FFI needs the signature of the call
                        let meta = self.tlookup.meta_from_tc(t, self.objects, self.dummy_gcv);
                        let mut emitter = current_func_emitter!(self);
                        let i = emitter.add_const(None, GosValue::Metadata(meta));
                        emitter.emit_load(i, None, ValueType::Metadata, pos);
                    }
                }
                let (param0t, param_last_t) = if params.len() > 0 {
                    (
                        Some(self.tlookup.get_expr_value_type(&params[0])),
                        Some(self.tlookup.get_expr_value_type(params.last().unwrap())),
                    )
                } else {
                    (None, None)
                };
                let bf = self.tc_objs.universe().builtins()[&builtin];
                let param_count = params.len() as OpIndex;
                let (t_variadic, count) = if bf.variadic {
                    if ellipsis {
                        (None, Some(0)) // do not pack params if there is ellipsis
                    } else {
                        (param_last_t, Some(bf.arg_count as OpIndex - param_count))
                    }
                } else {
                    (None, Some(param_count as OpIndex))
                };
                let func = current_func_mut!(self);
                func.emit_inst(opcode, [param0t, t_variadic, None], count, pos);
            }
            // conversion
            // from the specs:
            /*
            A non-constant value x can be converted to type T in any of these cases:
                x is assignable to T.
                ignoring struct tags (see below), x's type and T have identical underlying types.
                ignoring struct tags (see below), x's type and T are pointer types that are not defined types, and their pointer base types have identical underlying types.
                x's type and T are both integer or floating point types.
                x's type and T are both complex types.
                x is an integer or a slice of bytes or runes and T is a string type.
                x is a string and T is a slice of bytes or runes.
            A value x is assignable to a variable of type T ("x is assignable to T") if one of the following conditions applies:
                x's type is identical to T.
                x's type V and T have identical underlying types and at least one of V or T is not a defined type.
                T is an interface type and x implements T.
                x is a bidirectional channel value, T is a channel type, x's type V and T have identical element types, and at least one of V or T is not a defined type.
                x is the predeclared identifier nil and T is a pointer, function, slice, map, channel, or interface type.
                x is an untyped constant representable by a value of type T.
            */
            OperandMode::TypeExpr => {
                assert!(params.len() == 1);
                self.visit_expr(&params[0]);
                let tct0 = self.tlookup.get_expr_tc_type(func_expr);
                let utct0 = self.tlookup.underlying_tc(tct0);
                let t0 = self.tlookup.value_type_from_tc(utct0);
                let tct1 = self.tlookup.get_expr_tc_type(&params[0]);
                let utct1 = self.tlookup.underlying_tc(tct1);
                let t1 = self.tlookup.value_type_from_tc(utct1);
                // just ignore conversion if it's nil or types are identical
                if t1 != ValueType::Nil && !identical(utct0, utct1, self.tc_objs) {
                    let iface_index = match t0 {
                        ValueType::Interface => {
                            if t1 != ValueType::Nil {
                                self.iface_mapping.get_index(
                                    &(tct0, Some(tct1)),
                                    &mut self.tlookup,
                                    self.objects,
                                    self.dummy_gcv,
                                )
                            } else {
                                0
                            }
                        }
                        _ => 0,
                    };
                    // get the type of slice element if we are converting to or from a slice
                    let tct2 = if t0 == ValueType::Slice {
                        Some(utct0)
                    } else if t1 == ValueType::Slice {
                        Some(utct1)
                    } else {
                        None
                    };
                    let t2 = tct2.map(|x| {
                        self.tlookup.value_type_from_tc(
                            self.tc_objs.types[x].try_as_slice().unwrap().elem(),
                        )
                    });
                    current_func_emitter!(self).emit_cast(t0, t1, t2, -1, iface_index, pos);
                }
            }
            // normal goscript function
            _ => {
                self.visit_expr(func_expr);
                current_func_emitter!(self).emit_pre_call(pos);
                let _ = params.iter().map(|e| self.visit_expr(e)).count();
                let t = self.tlookup.get_expr_tc_type(func_expr);
                self.try_cast_params_to_iface(t, params, ellipsis);

                // do not pack params if there is ellipsis
                let ftc = self
                    .tlookup
                    .underlying_tc(self.tlookup.get_expr_tc_type(func_expr));
                let func_detail = self.tc_objs.types[ftc].try_as_signature().unwrap();
                let variadic = func_detail.variadic();
                let pack = variadic && !ellipsis;
                current_func_emitter!(self).emit_call(style, pack, pos);
            }
        }
    }

    fn gen_map_index(&mut self, expr: &Expr, index: &Expr, comma_ok: bool) {
        let t0 = self.tlookup.get_expr_value_type(expr);
        let t1 = self.tlookup.get_expr_value_type(index);
        self.visit_expr(expr);
        let pos = Some(expr.pos(&self.ast_objs));
        if let Some(const_val) = self.tlookup.get_tc_const_value(index.id()) {
            let (ival, _) = const_val.to_int().int_as_i64();
            if let Ok(i) = OpIndex::try_from(ival) {
                current_func_emitter!(self).emit_load_index_imm(i, t0, comma_ok, pos);
                return;
            }
        }
        self.visit_expr(index);
        current_func_emitter!(self).emit_load_index(t0, t1, comma_ok, pos);
    }

    fn try_cast_to_iface(
        &mut self,
        lhs: Option<TCTypeKey>,
        rhs: Option<TCTypeKey>,
        rhs_index: OpIndex,
        pos: usize,
    ) -> ValueType {
        let mut ret_type = None;
        if let Some(t0) = lhs {
            if self.tlookup.underlying_value_type_from_tc(t0) == ValueType::Interface {
                let (cast, typ) = match rhs {
                    Some(t1) => {
                        let vt1 = self.tlookup.underlying_value_type_from_tc(t1);
                        (vt1 != ValueType::Interface && vt1 != ValueType::Nil, vt1)
                    }
                    None => (true, ValueType::Slice), // it must be a variadic parameter
                };
                if cast {
                    let index = self.iface_mapping.get_index(
                        &(t0, rhs),
                        &mut self.tlookup,
                        self.objects,
                        self.dummy_gcv,
                    );
                    current_func_emitter!(self).emit_cast(
                        ValueType::Interface,
                        typ,
                        None,
                        rhs_index,
                        index,
                        Some(pos),
                    );
                    ret_type = Some(ValueType::Interface);
                }
            }
        }
        ret_type.unwrap_or(self.tlookup.value_type_from_tc(rhs.unwrap()))
    }

    fn try_cast_params_to_iface(&mut self, func: TCTypeKey, params: &Vec<Expr>, ellipsis: bool) {
        let (sig_params, variadic) = self.tlookup.get_sig_params_tc_types(func);
        let non_variadic_params = variadic.map_or(sig_params.len(), |_| sig_params.len() - 1);
        for (i, v) in sig_params[..non_variadic_params].iter().enumerate() {
            let rhs_index = i as OpIndex - params.len() as OpIndex;
            let rhs = if i == params.len() - 1 && ellipsis {
                None
            } else {
                Some(self.tlookup.get_expr_tc_type(&params[i]))
            };
            let pos = params[i].pos(&self.ast_objs);
            self.try_cast_to_iface(Some(*v), rhs, rhs_index, pos);
        }
        if !ellipsis {
            if let Some((_, t)) = variadic {
                if self.tlookup.underlying_value_type_from_tc(t) == ValueType::Interface {
                    for (i, p) in params.iter().enumerate().skip(non_variadic_params) {
                        let rhs_index = i as OpIndex - params.len() as OpIndex;
                        let rhs = self.tlookup.get_expr_tc_type(p);
                        let pos = p.pos(&self.ast_objs);
                        self.try_cast_to_iface(Some(t), Some(rhs), rhs_index, pos);
                    }
                }
            }
        }
    }

    fn get_type_default(&mut self, expr: &Expr) -> (GosValue, TCTypeKey) {
        let t = self.tlookup.get_expr_tc_type(expr);
        let meta = self.tlookup.meta_from_tc(t, self.objects, self.dummy_gcv);
        let zero_val = zero_val!(meta, self.objects, self.dummy_gcv);
        (zero_val, t)
    }

    fn visit_composite_expr(&mut self, expr: &Expr, tctype: TCTypeKey) {
        match expr {
            Expr::CompositeLit(clit) => self.gen_composite_literal(clit, tctype),
            _ => self.visit_expr(expr),
        }
        let t = self.tlookup.get_expr_tc_type(expr);
        self.try_cast_to_iface(Some(tctype), Some(t), -1, expr.pos(self.ast_objs));
    }

    fn gen_composite_literal(&mut self, clit: &CompositeLit, tctype: TCTypeKey) {
        let meta = self
            .tlookup
            .meta_from_tc(tctype, &mut self.objects, self.dummy_gcv);
        let pos = Some(clit.l_brace);
        let typ = &self.tc_objs.types[tctype].underlying_val(&self.tc_objs);
        let (mkey, mc) = meta.get_underlying(&self.objects.metas).unwrap_non_ptr();
        let mtype = &self.objects.metas[mkey].clone();
        match mtype {
            MetadataType::SliceOrArray(_, _) => {
                let elem = match mc {
                    MetaCategory::Default => typ.try_as_slice().unwrap().elem(),
                    MetaCategory::Array => typ.try_as_array().unwrap().elem(),
                    _ => unreachable!(),
                };
                for expr in clit.elts.iter().rev() {
                    match expr {
                        Expr::KeyValue(kv) => {
                            self.visit_composite_expr(&kv.val, elem);
                            // the key is a constant
                            self.visit_expr(&kv.key);
                        }
                        _ => {
                            self.visit_composite_expr(expr, elem);
                            // -1 as a placeholder for when the index is missing
                            current_func_emitter!(self).emit_push_imm(ValueType::Int, -1, None);
                        }
                    };
                }
            }
            MetadataType::Map(_, _) => {
                let map_type = typ.try_as_map().unwrap();
                for expr in clit.elts.iter() {
                    match expr {
                        Expr::KeyValue(kv) => {
                            self.visit_composite_expr(&kv.val, map_type.elem());
                            self.visit_composite_expr(&kv.key, map_type.key());
                        }
                        _ => unreachable!(),
                    }
                }
            }
            MetadataType::Struct(f, _) => {
                let struct_type = typ.try_as_struct().unwrap();
                for (i, expr) in clit.elts.iter().enumerate() {
                    let field_type = self.tc_objs.lobjs[struct_type.fields()[i]].typ().unwrap();
                    let index = match expr {
                        Expr::KeyValue(kv) => {
                            self.visit_composite_expr(&kv.val, field_type);
                            let ident = kv.key.try_as_ident().unwrap();
                            f.mapping[&self.ast_objs.idents[*ident].name]
                        }
                        _ => {
                            self.visit_composite_expr(expr, field_type);
                            i as OpIndex
                        }
                    };
                    current_func_emitter!(self).emit_push_imm(ValueType::Uint, index, pos);
                }
            }
            _ => {
                dbg!(&mtype);
                unreachable!()
            }
        }
        current_func_emitter!(self).emit_push_imm(
            ValueType::Int32,
            clit.elts.len() as OpIndex,
            pos,
        );

        let mut emitter = current_func_emitter!(self);
        let i = emitter.add_const(None, GosValue::Metadata(meta));
        emitter.emit_literal(ValueType::Metadata, i.into(), pos);
    }

    fn gen_type_meta(&mut self, typ: &Expr) {
        let m = self
            .tlookup
            .get_meta_by_node_id(typ.id(), self.objects, self.dummy_gcv);
        let mut emitter = current_func_emitter!(self);
        let i = emitter.add_const(None, GosValue::Metadata(m));
        let pos = Some(typ.pos(&self.ast_objs));
        emitter.emit_load(i, None, ValueType::Metadata, pos);
    }

    fn gen_const(&mut self, node: NodeId, pos: Option<Pos>) {
        let val = self.tlookup.get_const_value(node);
        let mut emitter = current_func_emitter!(self);
        let t = val.get_type();
        let i = emitter.add_const(None, val);
        emitter.emit_load(i, None, t, pos);
    }

    fn gen_load_embedded_member(
        &mut self,
        indices: &[usize],
        mdata: GosMetadata,
        typ: ValueType,
        pos: Option<usize>,
    ) -> (GosMetadata, ValueType) {
        let mut lhs_meta = mdata;
        let mut lhs_type = typ;
        for &i in indices.iter() {
            let embed_index = i as OpIndex;
            current_func_emitter!(self).emit_load_struct_field(embed_index, lhs_type, pos);
            lhs_meta = self.get_embedded_member_meta(&lhs_meta, i);
            lhs_type = lhs_meta.get_value_type(&self.objects.metas);
        }
        (lhs_meta, lhs_type)
    }

    fn get_embedded_member_meta(&self, parent: &GosMetadata, index: usize) -> GosMetadata {
        let (meta_key, _) = parent.unwrap_non_ptr_or_prt1();
        match &self.objects.metas[meta_key] {
            MetadataType::Named(_, m) => {
                let (key2, _) = m.unwrap_non_ptr_or_prt1();
                self.objects.metas[key2].as_struct().0.fields[index]
            }
            MetadataType::Struct(f, _) => f.fields[index],
            _ => unreachable!(),
        }
    }

    fn current_func_add_const_def(&mut self, ident: &Ident, cst: GosValue) -> EntIndex {
        let func = current_func_mut!(self);
        let entity = ident.entity.clone().into_key().unwrap();
        let index = func.add_const(Some(entity), cst.clone());
        if func.is_ctor() {
            let pkg_key = func.package;
            drop(func);
            let pkg = &mut self.objects.packages[pkg_key];
            pkg.add_member(ident.name.clone(), cst);
        }
        index
    }

    fn add_pkg_var_member(&mut self, pkey: PackageKey, vars: &Vec<Rc<ValueSpec>>) {
        for v in vars.iter() {
            for n in v.names.iter() {
                let ident = &self.ast_objs.idents[*n];
                let meta = self
                    .tlookup
                    .gen_def_type_meta(*n, self.objects, self.dummy_gcv);
                let val = zero_val!(meta, self.objects, self.dummy_gcv);
                self.objects.packages[pkey].add_member(ident.name.clone(), val);
            }
        }
    }

    pub fn gen_with_files(&mut self, files: &Vec<File>, tcpkg: TCPackageKey, index: OpIndex) {
        let pkey = self.pkg_key;
        let fmeta = self.objects.metadata.default_sig;
        let f =
            GosValue::new_function(pkey, fmeta, self.objects, self.dummy_gcv, FuncFlag::PkgCtor);
        let fkey = *f.as_function();
        // the 0th member is the constructor
        self.objects.packages[pkey].add_member(
            String::new(),
            GosValue::new_closure(fkey, &self.objects.functions),
        );
        self.pkg_key = pkey;
        self.func_stack.push(fkey);

        let vars = self
            .pkg_helper
            .sort_var_decls(files, self.tlookup.type_info());
        self.add_pkg_var_member(pkey, &vars);

        self.pkg_helper.gen_imports(tcpkg, current_func_mut!(self));

        for f in files.iter() {
            for d in f.decls.iter() {
                self.visit_decl(d)
            }
        }
        for v in vars.iter() {
            self.gen_def_var(v);
        }

        let mut emitter = Emitter::new(&mut self.objects.functions[fkey]);
        emitter.emit_return(Some(index), None);
        self.func_stack.pop();
    }
}

impl<'a> ExprVisitor for CodeGen<'a> {
    type Result = ();

    fn visit_expr(&mut self, expr: &Expr) {
        if let Some(mode) = self.tlookup.try_get_expr_mode(expr) {
            if let OperandMode::Constant(_) = mode {
                self.gen_const(expr.id(), Some(expr.pos(&self.ast_objs)));
                return;
            }
        }
        walk_expr(self, expr);
    }

    fn visit_expr_ident(&mut self, expr: &Expr, ident: &IdentKey) {
        let index = self.resolve_any_ident(ident, Some(expr));
        let t = self.tlookup.get_use_value_type(*ident);
        let fkey = self.func_stack.last().unwrap();
        let p = Some(self.ast_objs.idents[*ident].pos);
        current_func_emitter!(self).emit_load(
            index,
            Some((self.pkg_helper.pairs_mut(), *fkey)),
            t,
            p,
        );
    }

    fn visit_expr_ellipsis(&mut self, _: &Expr, _els: &Option<Expr>) {
        unreachable!();
    }

    fn visit_expr_basic_lit(&mut self, this: &Expr, blit: &BasicLit) {
        self.gen_const(this.id(), Some(blit.pos));
    }

    /// Add function as a const and then generate a closure of it
    fn visit_expr_func_lit(&mut self, this: &Expr, flit: &FuncLit) {
        let tc_type = self.tlookup.get_node_tc_type(this.id());
        let fkey = self.gen_func_def(tc_type, flit.typ, None, &flit.body);
        let mut emitter = current_func_emitter!(self);
        let i = emitter.add_const(None, GosValue::Function(fkey));
        let pos = Some(flit.body.l_brace);
        emitter.emit_literal(ValueType::Function, i.into(), pos);
    }

    fn visit_expr_composit_lit(&mut self, _: &Expr, clit: &CompositeLit) {
        let tctype = self.tlookup.get_expr_tc_type(clit.typ.as_ref().unwrap());
        self.gen_composite_literal(clit, tctype);
    }

    fn visit_expr_paren(&mut self, _: &Expr, expr: &Expr) {
        self.visit_expr(expr)
    }

    fn visit_expr_selector(&mut self, this: &Expr, expr: &Expr, ident: &IdentKey) {
        let pos = Some(expr.pos(&self.ast_objs));
        if let Some(key) = self.tlookup.try_get_pkg_key(expr) {
            let pkg = self.pkg_helper.get_vm_pkg(key);
            let t = self.tlookup.get_use_value_type(*ident);
            let fkey = self.func_stack.last().unwrap();
            current_func_emitter!(self).emit_load(
                EntIndex::PackageMember(pkg, *ident),
                Some((self.pkg_helper.pairs_mut(), *fkey)),
                t,
                pos,
            );
            return;
        }

        let mut lhs_meta =
            self.tlookup
                .get_meta_by_node_id(expr.id(), self.objects, self.dummy_gcv);
        let (t0, t1, indices, p_recv) = self
            .tlookup
            .get_selection_vtypes_indices_ptr_recv(this.id());
        let index_count = indices.len();
        let index = indices[index_count - 1] as OpIndex; // the final index
        let embedded_indices = Vec::from_iter(indices[..index_count - 1].iter().cloned());
        let lhs_type = t0;
        let lhs_has_embedded = index_count > 1;
        let get_recv_prep = |recv_is_ptr, typ: ValueType| -> ReceiverPreprocess {
            if recv_is_ptr && typ != ValueType::Pointer {
                ReceiverPreprocess::Ref
            } else if !recv_is_ptr && typ == ValueType::Pointer {
                ReceiverPreprocess::Deref
            } else {
                ReceiverPreprocess::Default
            }
        };

        if !lhs_has_embedded {
            let recv_prep = get_recv_prep(p_recv, lhs_type);
            match &recv_prep {
                ReceiverPreprocess::Default => self.visit_expr(expr),
                ReceiverPreprocess::Ref => self.visit_expr_unary(this, expr, &Token::AND),
                ReceiverPreprocess::Deref => {
                    self.visit_expr(expr);
                    current_func_mut!(self).emit_code_with_type(Opcode::DEREF, lhs_type, pos);
                    lhs_meta = lhs_meta.unptr_to();
                }
            };
        } else {
            self.visit_expr(expr);
            let index_count_m1 = embedded_indices.len() - 1;
            let (m, t) = self.gen_load_embedded_member(
                &embedded_indices[0..index_count_m1],
                lhs_meta,
                lhs_type,
                pos,
            );
            let index = embedded_indices[index_count_m1];
            let final_meta = self.get_embedded_member_meta(&m, index);
            let final_typ = final_meta.get_value_type(&self.objects.metas);
            let recv_prep = get_recv_prep(p_recv, final_typ);
            match &recv_prep {
                ReceiverPreprocess::Ref => {
                    current_func_mut!(self).emit_code_with_type_imm(
                        Opcode::REF_STRUCT_FIELD,
                        t,
                        index as OpIndex,
                        pos,
                    );
                    lhs_meta = final_meta.ptr_to();
                }
                ReceiverPreprocess::Deref => {
                    current_func_emitter!(self).emit_load_struct_field(index as OpIndex, t, pos);
                    current_func_mut!(self).emit_code_with_type(Opcode::DEREF, lhs_type, pos);
                    lhs_meta = final_meta.unptr_to();
                }
                ReceiverPreprocess::Default => {
                    current_func_emitter!(self).emit_load_struct_field(index as OpIndex, t, pos);
                    lhs_meta = final_meta;
                }
            }
        }

        let typ = lhs_meta.get_value_type(&self.objects.metas);
        if t1 == ValueType::Closure {
            if lhs_meta
                .get_underlying(&self.objects.metas)
                .get_value_type(&self.objects.metas)
                == ValueType::Interface
            {
                current_func_mut!(self).emit_code_with_type_imm(
                    Opcode::BIND_INTERFACE_METHOD,
                    typ,
                    index,
                    pos,
                );
            } else {
                let func = current_func_mut!(self);
                func.emit_code_with_type(Opcode::BIND_METHOD, typ, pos);
                let point = func.next_code_index();
                func.emit_raw_inst(0, pos); // placeholder for FunctionKey
                let fkey = *self.func_stack.last().unwrap();
                self.call_helper.add_call(fkey, point, lhs_meta, index);
            }
        } else {
            current_func_emitter!(self).emit_load_struct_field(index, typ, pos);
        }
    }

    fn visit_expr_index(&mut self, _: &Expr, expr: &Expr, index: &Expr) {
        self.gen_map_index(expr, index, false);
    }

    fn visit_expr_slice(
        &mut self,
        _: &Expr,
        expr: &Expr,
        low: &Option<Expr>,
        high: &Option<Expr>,
        max: &Option<Expr>,
    ) -> Self::Result {
        self.visit_expr(expr);
        let t = self.tlookup.get_expr_value_type(expr);
        let pos = Some(expr.pos(&self.ast_objs));
        match low {
            None => current_func_emitter!(self).emit_push_imm(ValueType::Int, 0, pos),
            Some(e) => self.visit_expr(e),
        }
        match high {
            None => current_func_emitter!(self).emit_push_imm(ValueType::Int, -1, pos),
            Some(e) => self.visit_expr(e),
        }
        match max {
            None => current_func_mut!(self).emit_code_with_type(Opcode::SLICE, t, pos),
            Some(e) => {
                self.visit_expr(e);
                current_func_mut!(self).emit_code_with_type(Opcode::SLICE_FULL, t, pos);
            }
        }
    }

    fn visit_expr_type_assert(&mut self, _: &Expr, _expr: &Expr, _typ: &Option<Expr>) {
        unimplemented!();
    }

    fn visit_expr_call(&mut self, _: &Expr, func_expr: &Expr, params: &Vec<Expr>, ellipsis: bool) {
        self.gen_call(func_expr, params, ellipsis, CallStyle::Default);
    }

    fn visit_expr_star(&mut self, _: &Expr, expr: &Expr) {
        let pos = Some(expr.pos(&self.ast_objs));
        match self.tlookup.get_expr_mode(expr) {
            OperandMode::TypeExpr => {
                let m = self
                    .tlookup
                    .meta_from_tc(
                        self.tlookup.get_expr_tc_type(expr),
                        self.objects,
                        self.dummy_gcv,
                    )
                    .ptr_to();
                let mut emitter = current_func_emitter!(self);
                let index = emitter.add_const(None, GosValue::Metadata(m));
                emitter.emit_load(index, None, ValueType::Metadata, pos);
            }
            _ => {
                self.visit_expr(expr);
                let t = self.tlookup.get_expr_value_type(expr);
                current_func_mut!(self).emit_code_with_type(Opcode::DEREF, t, pos);
            }
        }
    }

    fn visit_expr_unary(&mut self, this: &Expr, expr: &Expr, op: &Token) {
        let pos = Some(expr.pos(&self.ast_objs));
        if op == &Token::AND {
            match expr {
                Expr::Ident(ikey) => {
                    let index = self.resolve_any_ident(ikey, None);
                    match index {
                        EntIndex::LocalVar(i) => {
                            let meta = self.tlookup.get_meta_by_node_id(
                                expr.id(),
                                self.objects,
                                self.dummy_gcv,
                            );
                            let t = meta.get_value_type(&self.objects.metas);
                            let ut = meta
                                .get_underlying(&self.objects.metas)
                                .get_value_type(&self.objects.metas);
                            if ut == ValueType::Struct
                                || ut == ValueType::Array
                                || ut == ValueType::Slice
                                || ut == ValueType::Map
                            {
                                let func = current_func_mut!(self);
                                func.emit_inst(
                                    Opcode::REF_LOCAL,
                                    [Some(t), None, None],
                                    Some(i),
                                    pos,
                                );
                            } else {
                                let ident = &self.ast_objs.idents[*ikey];
                                let entity_key = ident.entity_key().unwrap();
                                let func = current_func_mut!(self);
                                let ind = *func.entity_index(&entity_key).unwrap();
                                let desc = ValueDesc::new(
                                    *self.func_stack.last().unwrap(),
                                    ind.into(),
                                    t,
                                    false,
                                );
                                let index = func.try_add_upvalue(&entity_key, desc);
                                func.emit_inst(
                                    Opcode::REF_UPVALUE,
                                    [Some(t), None, None],
                                    Some(index.into()),
                                    pos,
                                );
                            }
                        }
                        EntIndex::UpValue(i) => {
                            let t = self.tlookup.get_expr_value_type(expr);
                            let func = current_func_mut!(self);
                            func.emit_inst(
                                Opcode::REF_UPVALUE,
                                [Some(t), None, None],
                                Some(i),
                                pos,
                            );
                        }
                        EntIndex::PackageMember(pkg, ident) => {
                            let func = current_func_mut!(self);
                            func.emit_inst(
                                Opcode::REF_PKG_MEMBER,
                                [None, None, None],
                                Some(0),
                                pos,
                            );
                            func.emit_raw_inst(key_to_u64(self.pkg_key), pos);
                            let fkey = self.func_stack.last().unwrap();
                            let i = current_func!(self).next_code_index() - 2;
                            self.pkg_helper.add_pair(pkg, ident, *fkey, i, false);
                        }
                        _ => unreachable!(),
                    }
                }
                Expr::Index(iexpr) => {
                    let t0 = self.tlookup.get_expr_value_type(&iexpr.expr);
                    let t1 = self.tlookup.get_expr_value_type(&iexpr.index);
                    self.visit_expr(&iexpr.expr);
                    self.visit_expr(&iexpr.index);
                    let pos = Some(iexpr.index.pos(&self.ast_objs));
                    current_func_mut!(self).emit_inst(
                        Opcode::REF_SLICE_MEMBER,
                        [Some(t0), Some(t1), None],
                        None,
                        pos,
                    );
                }
                Expr::Selector(sexpr) => match self.tlookup.try_get_pkg_key(&sexpr.expr) {
                    Some(key) => {
                        let pkey = self.pkg_helper.get_vm_pkg(key);
                        let func = current_func_mut!(self);
                        func.emit_inst(Opcode::REF_PKG_MEMBER, [None, None, None], Some(0), pos);
                        func.emit_raw_inst(key_to_u64(pkey), pos);
                        let fkey = self.func_stack.last().unwrap();
                        let i = current_func!(self).next_code_index() - 2;
                        self.pkg_helper.add_pair(pkey, sexpr.sel, *fkey, i, false);
                    }
                    None => {
                        self.visit_expr(&sexpr.expr);
                        let lhs_meta = self.tlookup.get_meta_by_node_id(
                            sexpr.expr.id(),
                            self.objects,
                            self.dummy_gcv,
                        );
                        let (t0, _, indices, _) = self
                            .tlookup
                            .get_selection_vtypes_indices_ptr_recv(sexpr.id());
                        let index_count = indices.len();
                        let index = indices[index_count - 1] as OpIndex; // the final index
                        let embedded_indices =
                            Vec::from_iter(indices[..index_count - 1].iter().cloned());
                        let (_, typ) =
                            self.gen_load_embedded_member(&embedded_indices, lhs_meta, t0, pos);
                        current_func_mut!(self).emit_code_with_type_imm(
                            Opcode::REF_STRUCT_FIELD,
                            typ,
                            index,
                            pos,
                        );
                    }
                },
                Expr::CompositeLit(clit) => {
                    self.visit_expr_composit_lit(this, clit);
                    let typ = self.tlookup.get_expr_value_type(expr);
                    current_func_mut!(self).emit_inst(
                        Opcode::REF_LOCAL,
                        [Some(typ), None, None],
                        Some(-1),
                        pos,
                    );
                }
                _ => {
                    dbg!(&expr);
                    unimplemented!()
                }
            }
            return;
        }

        self.visit_expr(expr);
        let code = match op {
            Token::ADD => Opcode::UNARY_ADD,
            Token::SUB => Opcode::UNARY_SUB,
            Token::XOR => Opcode::UNARY_XOR,
            Token::NOT => Opcode::NOT,
            Token::ARROW => Opcode::RECV,
            _ => {
                dbg!(op);
                unreachable!()
            }
        };
        let t = self.tlookup.get_expr_value_type(expr);
        current_func_mut!(self).emit_code_with_type(code, t, pos);
    }

    fn visit_expr_binary(&mut self, _: &Expr, left: &Expr, op: &Token, right: &Expr) {
        self.visit_expr(left);
        let t = self.tlookup.get_expr_value_type(left);
        let code = match op {
            Token::ADD => Opcode::ADD,
            Token::SUB => Opcode::SUB,
            Token::MUL => Opcode::MUL,
            Token::QUO => Opcode::QUO,
            Token::REM => Opcode::REM,
            Token::AND => Opcode::AND,
            Token::OR => Opcode::OR,
            Token::XOR => Opcode::XOR,
            Token::SHL => Opcode::SHL,
            Token::SHR => Opcode::SHR,
            Token::AND_NOT => Opcode::AND_NOT,
            Token::LAND => Opcode::PUSH_FALSE,
            Token::LOR => Opcode::PUSH_TRUE,
            Token::NOT => Opcode::NOT,
            Token::EQL => Opcode::EQL,
            Token::LSS => Opcode::LSS,
            Token::GTR => Opcode::GTR,
            Token::NEQ => Opcode::NEQ,
            Token::LEQ => Opcode::LEQ,
            Token::GEQ => Opcode::GEQ,
            _ => unreachable!(),
        };
        let pos = Some(left.pos(&self.ast_objs));
        // handles short circuit
        let mark_code = match op {
            Token::LAND => {
                let func = current_func_mut!(self);
                func.emit_code(Opcode::JUMP_IF_NOT, pos);
                Some((func.next_code_index(), code))
            }
            Token::LOR => {
                let func = current_func_mut!(self);
                func.emit_code(Opcode::JUMP_IF, pos);
                Some((func.next_code_index(), code))
            }
            _ => None,
        };
        self.visit_expr(right);

        if let Some((i, c)) = mark_code {
            let func = current_func_mut!(self);
            func.emit_code_with_imm(Opcode::JUMP, 1, pos);
            func.emit_code_with_type(c, t, pos);
            let diff = func.next_code_index() - i - 1;
            func.instruction_mut(i - 1).set_imm(diff as OpIndex);
        } else {
            let t1 = if code == Opcode::SHL || code == Opcode::SHR {
                Some(self.tlookup.get_expr_value_type(right))
            } else {
                None
            };
            current_func_mut!(self).emit_code_with_type2(code, t, t1, pos);
        }
    }

    fn visit_expr_key_value(&mut self, e: &Expr, _key: &Expr, _val: &Expr) {
        dbg!(e);
        unimplemented!();
    }

    fn visit_expr_array_type(&mut self, this: &Expr, _: &Option<Expr>, _: &Expr) {
        self.gen_type_meta(this)
    }

    fn visit_expr_struct_type(&mut self, this: &Expr, _s: &StructType) {
        self.gen_type_meta(this)
    }

    fn visit_expr_func_type(&mut self, this: &Expr, _s: &FuncTypeKey) {
        self.gen_type_meta(this)
    }

    fn visit_expr_interface_type(&mut self, this: &Expr, _s: &InterfaceType) {
        self.gen_type_meta(this)
    }

    fn visit_map_type(&mut self, this: &Expr, _: &Expr, _: &Expr, _map: &Expr) {
        self.gen_type_meta(this)
    }

    fn visit_chan_type(&mut self, this: &Expr, _chan: &Expr, _dir: &ChanDir) {
        self.gen_type_meta(this)
    }

    fn visit_bad_expr(&mut self, _: &Expr, _e: &BadExpr) {
        unreachable!();
    }
}

impl<'a> StmtVisitor for CodeGen<'a> {
    type Result = ();

    fn visit_stmt(&mut self, stmt: &Stmt) {
        walk_stmt(self, stmt)
    }

    fn visit_decl(&mut self, decl: &Decl) {
        walk_decl(self, decl)
    }

    fn visit_stmt_decl_gen(&mut self, gdecl: &GenDecl) {
        for s in gdecl.specs.iter() {
            let spec = &self.ast_objs.specs[*s];
            match spec {
                Spec::Import(_) => {
                    //handled elsewhere
                }
                Spec::Type(ts) => {
                    let ident = self.ast_objs.idents[ts.name].clone();
                    let m = self
                        .tlookup
                        .gen_def_type_meta(ts.name, self.objects, self.dummy_gcv);
                    self.current_func_add_const_def(&ident, GosValue::Metadata(m));
                }
                Spec::Value(vs) => match &gdecl.token {
                    Token::VAR => {
                        // package level vars are handled elsewhere due to ordering
                        if !current_func!(self).is_ctor() {
                            self.gen_def_var(vs);
                        }
                    }
                    Token::CONST => self.gen_def_const(&vs.names, &vs.values),
                    _ => unreachable!(),
                },
            }
        }
    }

    fn visit_stmt_decl_func(&mut self, fdecl: &FuncDeclKey) -> Self::Result {
        let decl = &self.ast_objs.fdecls[*fdecl];
        if decl.body.is_none() {
            unimplemented!()
        }
        let tc_type = self.tlookup.get_def_tc_type(decl.name);
        let stmt = decl.body.as_ref().unwrap();
        let fkey = self.gen_func_def(tc_type, decl.typ, decl.recv.clone(), stmt);
        let cls = GosValue::new_closure(fkey, &self.objects.functions);
        // this is a struct method
        if let Some(self_ident) = &decl.recv {
            let field = &self.ast_objs.fields[self_ident.list[0]];
            let name = &self.ast_objs.idents[decl.name].name;
            let meta =
                self.tlookup
                    .get_meta_by_node_id(field.typ.id(), self.objects, self.dummy_gcv);
            meta.set_method_code(name, fkey, &mut self.objects.metas);
        } else {
            let ident = &self.ast_objs.idents[decl.name];
            let pkg = &mut self.objects.packages[self.pkg_key];
            pkg.add_member(ident.name.clone(), cls);
        }
    }

    fn visit_stmt_labeled(&mut self, lstmt: &LabeledStmtKey) {
        let stmt = &self.ast_objs.l_stmts[*lstmt];
        let offset = current_func!(self).code().len();
        let entity = self.ast_objs.idents[stmt.label].entity_key().unwrap();
        let is_breakable = match &stmt.stmt {
            Stmt::For(_) | Stmt::Range(_) | Stmt::Select(_) | Stmt::Switch(_) => true,
            _ => false,
        };
        self.branch.add_label(entity, offset, is_breakable);
        self.visit_stmt(&stmt.stmt);
    }

    fn visit_stmt_send(&mut self, sstmt: &SendStmt) {
        self.visit_expr(&sstmt.chan);
        self.visit_expr(&sstmt.val);
        let t = self.tlookup.get_expr_value_type(&sstmt.val);
        current_func_mut!(self).emit_code_with_type(Opcode::SEND, t, Some(sstmt.arrow));
    }

    fn visit_stmt_incdec(&mut self, idcstmt: &IncDecStmt) {
        self.gen_assign(&idcstmt.token, &vec![&idcstmt.expr], RightHandSide::Nothing);
    }

    fn visit_stmt_assign(&mut self, astmt: &AssignStmtKey) {
        let stmt = &self.ast_objs.a_stmts[*astmt];
        self.gen_assign(
            &stmt.token,
            &stmt.lhs.iter().map(|x| x).collect(),
            RightHandSide::Values(&stmt.rhs),
        );
    }

    fn visit_stmt_go(&mut self, gostmt: &GoStmt) {
        match &gostmt.call {
            Expr::Call(call) => {
                self.gen_call(
                    &call.func,
                    &call.args,
                    call.ellipsis.is_some(),
                    CallStyle::Async,
                );
            }
            _ => unreachable!(),
        }
    }

    fn visit_stmt_defer(&mut self, dstmt: &DeferStmt) {
        current_func_mut!(self).flag = FuncFlag::HasDefer;
        match &dstmt.call {
            Expr::Call(call) => {
                self.gen_call(
                    &call.func,
                    &call.args,
                    call.ellipsis.is_some(),
                    CallStyle::Defer,
                );
            }
            _ => unreachable!(),
        }
    }

    fn visit_stmt_return(&mut self, rstmt: &ReturnStmt) {
        let pos = Some(rstmt.ret);
        let types = self
            .tlookup
            .get_sig_returns_tc_types(*self.func_t_stack.last().unwrap());
        for (i, expr) in rstmt.results.iter().enumerate() {
            self.visit_expr(expr);
            let tc_type = self.tlookup.get_expr_tc_type(expr);
            let t =
                self.try_cast_to_iface(Some(types[i]), Some(tc_type), -1, expr.pos(&self.ast_objs));
            let mut emitter = current_func_emitter!(self);
            emitter.emit_store(
                &LeftHandSide::Primitive(EntIndex::LocalVar(i as OpIndex)),
                -1,
                None,
                None,
                t,
                pos,
            );
            emitter.emit_pop(1, pos);
        }
        current_func_emitter!(self).emit_return(None, pos);
    }

    fn visit_stmt_branch(&mut self, bstmt: &BranchStmt) {
        match bstmt.token {
            Token::BREAK | Token::CONTINUE => {
                let entity = bstmt
                    .label
                    .map(|x| self.ast_objs.idents[x].entity_key().unwrap());
                self.branch.add_point(
                    current_func_mut!(self),
                    bstmt.token.clone(),
                    entity,
                    bstmt.token_pos,
                );
            }
            Token::GOTO => {
                let func = current_func_mut!(self);
                let label = bstmt.label.unwrap();
                let entity = self.ast_objs.idents[label].entity_key().unwrap();
                self.branch.go_to(func, &entity, bstmt.token_pos);
            }
            Token::FALLTHROUGH => {
                // handled in gen_switch_body
            }
            _ => unreachable!(),
        }
    }

    fn visit_stmt_block(&mut self, bstmt: &BlockStmt) {
        for stmt in bstmt.list.iter() {
            self.visit_stmt(stmt);
        }
    }

    fn visit_stmt_if(&mut self, ifstmt: &IfStmt) {
        if let Some(init) = &ifstmt.init {
            self.visit_stmt(init);
        }
        self.visit_expr(&ifstmt.cond);
        let func = current_func_mut!(self);
        func.emit_code(Opcode::JUMP_IF_NOT, Some(ifstmt.if_pos));
        let top_marker = func.next_code_index();

        drop(func);
        self.visit_stmt_block(&ifstmt.body);
        let marker_if_arm_end = if ifstmt.els.is_some() {
            let func = current_func_mut!(self);
            // imm to be set later
            func.emit_code(Opcode::JUMP, Some(ifstmt.if_pos));
            Some(func.next_code_index())
        } else {
            None
        };

        // set the correct else jump target
        let func = current_func_mut!(self);
        let offset = func.offset(top_marker);
        func.instruction_mut(top_marker - 1).set_imm(offset);

        if let Some(els) = &ifstmt.els {
            self.visit_stmt(els);
            // set the correct if_arm_end jump target
            let func = current_func_mut!(self);
            let marker = marker_if_arm_end.unwrap();
            let offset = func.offset(marker);
            func.instruction_mut(marker - 1).set_imm(offset);
        }
    }

    fn visit_stmt_case(&mut self, _cclause: &CaseClause) {
        unreachable!(); // handled at upper level of the tree
    }

    fn visit_stmt_switch(&mut self, sstmt: &SwitchStmt) {
        self.branch.enter_block();

        if let Some(init) = &sstmt.init {
            self.visit_stmt(init);
        }
        let tag_type = match &sstmt.tag {
            Some(e) => {
                self.visit_expr(e);
                self.tlookup.get_expr_value_type(e)
            }
            None => {
                current_func_mut!(self).emit_code(Opcode::PUSH_TRUE, None);
                ValueType::Bool
            }
        };

        self.gen_switch_body(&*sstmt.body, tag_type);

        self.branch.leave_block(current_func_mut!(self), None);
    }

    fn visit_stmt_type_switch(&mut self, tstmt: &TypeSwitchStmt) {
        if let Some(init) = &tstmt.init {
            self.visit_stmt(init);
        }

        let (ident_expr, assert) = match &tstmt.assign {
            Stmt::Assign(ass_key) => {
                let ass = &self.ast_objs.a_stmts[*ass_key];
                (Some(&ass.lhs[0]), &ass.rhs[0])
            }
            Stmt::Expr(e) => (None, &**e),
            _ => unreachable!(),
        };
        let (v, pos) = match assert {
            Expr::TypeAssert(ta) => (&ta.expr, Some(ta.l_paren)),
            _ => unreachable!(),
        };

        if let Some(iexpr) = ident_expr {
            let ident = &self.ast_objs.idents[*iexpr.try_as_ident().unwrap()];
            let ident_key = ident.entity.clone().into_key();
            let func = current_func_mut!(self);
            let index = func.add_local(ident_key);
            func.add_local_zero(GosValue::new_nil());
            self.visit_expr(v);
            let func = current_func_mut!(self);
            func.emit_code_with_flag_imm(Opcode::TYPE, true, index.into(), pos);
        } else {
            self.visit_expr(v);
            current_func_mut!(self).emit_code(Opcode::TYPE, pos);
        }

        self.gen_switch_body(&*tstmt.body, ValueType::Metadata);
    }

    fn visit_stmt_comm(&mut self, _cclause: &CommClause) {
        unimplemented!();
    }

    fn visit_stmt_select(&mut self, sstmt: &SelectStmt) {
        /*
        Execution of a "select" statement proceeds in several steps:

        1. For all the cases in the statement, the channel operands of receive operations
        and the channel and right-hand-side expressions of send statements are evaluated
        exactly once, in source order, upon entering the "select" statement. The result
        is a set of channels to receive from or send to, and the corresponding values to
        send. Any side effects in that evaluation will occur irrespective of which (if any)
        communication operation is selected to proceed. Expressions on the left-hand side
        of a RecvStmt with a short variable declaration or assignment are not yet evaluated.
        2. If one or more of the communications can proceed, a single one that can proceed
        is chosen via a uniform pseudo-random selection. Otherwise, if there is a default
        case, that case is chosen. If there is no default case, the "select" statement
        blocks until at least one of the communications can proceed.
        3. Unless the selected case is the default case, the respective communication operation
        is executed.
        4. If the selected case is a RecvStmt with a short variable declaration or an assignment,
        the left-hand side expressions are evaluated and the received value (or values)
        are assigned.
        5. The statement list of the selected case is executed.

        Since communication on nil channels can never proceed, a select with only nil
        channels and no default case blocks forever.
        */
        self.branch.enter_block();

        let mut helper = SelectHelper::new();
        let comms: Vec<&CommClause> = sstmt
            .body
            .list
            .iter()
            .map(|s| SelectHelper::to_comm_clause(s))
            .collect();
        for c in comms.iter() {
            let (typ, pos) = match &c.comm {
                Some(comm) => match comm {
                    Stmt::Send(send_stmt) => {
                        self.visit_expr(&send_stmt.chan);
                        self.visit_expr(&send_stmt.val);
                        let t = self.tlookup.get_expr_value_type(&send_stmt.val);
                        (CommType::Send(t), send_stmt.arrow)
                    }
                    Stmt::Assign(ass_key) => {
                        let ass = &self.ast_objs.a_stmts[*ass_key];
                        let (e, pos) = SelectHelper::unwrap_recv(&ass.rhs[0]);
                        self.visit_expr(e);
                        let t = match &ass.lhs.len() {
                            1 => CommType::Recv(&ass),
                            2 => CommType::RecvCommaOk(&ass),
                            _ => unreachable!(),
                        };
                        (t, pos)
                    }
                    Stmt::Expr(expr_stmt) => {
                        let (e, pos) = SelectHelper::unwrap_recv(expr_stmt);
                        self.visit_expr(e);
                        (CommType::RecvNoLhs, pos)
                    }
                    _ => unreachable!(),
                },
                None => (CommType::Default, c.colon),
            };
            helper.add_comm(typ, pos);
        }

        helper.emit_select(current_func_mut!(self));

        let last_index = comms.len() - 1;
        for (i, c) in comms.iter().enumerate() {
            let begin = current_func!(self).next_code_index();

            match helper.comm_type(i) {
                CommType::Recv(ass) | CommType::RecvCommaOk(ass) => {
                    self.gen_assign(
                        &ass.token,
                        &ass.lhs.iter().map(|x| x).collect(),
                        RightHandSide::SelectRecv(&ass.rhs[0]),
                    );
                }
                _ => {}
            }

            for stmt in c.body.iter() {
                self.visit_stmt(stmt);
            }
            let func = current_func_mut!(self);
            let mut end = func.next_code_index();
            // the last block doesn't jump
            if i < last_index {
                func.emit_code(Opcode::JUMP, None);
            } else {
                end -= 1;
            }

            helper.set_block_begin_end(i, begin, end);
        }

        helper.patch_select(current_func_mut!(self));

        self.branch.leave_block(current_func_mut!(self), None);
    }

    fn visit_stmt_for(&mut self, fstmt: &ForStmt) {
        self.branch.enter_block();

        if let Some(init) = &fstmt.init {
            self.visit_stmt(init);
        }
        let top_marker = current_func!(self).next_code_index();
        let out_marker = if let Some(cond) = &fstmt.cond {
            self.visit_expr(&cond);
            let func = current_func_mut!(self);
            func.emit_code(Opcode::JUMP_IF_NOT, Some(fstmt.for_pos));
            Some(func.next_code_index())
        } else {
            None
        };
        self.visit_stmt_block(&fstmt.body);
        let continue_marker = if let Some(post) = &fstmt.post {
            // "continue" jumps to post statements
            let m = current_func!(self).next_code_index();
            self.visit_stmt(post);
            m
        } else {
            // "continue" jumps to top directly if no post statements
            top_marker
        };

        // jump to the top
        let func = current_func_mut!(self);
        let offset = -func.offset(top_marker) - 1;
        func.emit_code_with_imm(Opcode::JUMP, offset, Some(fstmt.for_pos));

        // set the correct else jump out target
        if let Some(m) = out_marker {
            let func = current_func_mut!(self);
            let offset = func.offset(m);
            func.instruction_mut(m - 1).set_imm(offset);
        }

        self.branch
            .leave_block(current_func_mut!(self), Some(continue_marker));
    }

    fn visit_stmt_range(&mut self, rstmt: &RangeStmt) {
        self.branch.enter_block();

        let blank = Expr::Ident(self.blank_ident);
        let lhs = vec![
            rstmt.key.as_ref().unwrap_or(&blank),
            rstmt.val.as_ref().unwrap_or(&blank),
        ];
        let marker = self
            .gen_assign(&rstmt.token, &lhs, RightHandSide::Range(&rstmt.expr))
            .unwrap();

        self.visit_stmt_block(&rstmt.body);
        // jump to the top
        let func = current_func_mut!(self);
        let offset = -func.offset(marker) - 1;
        // tell Opcode::RANGE where to jump after it's done
        let end_offset = func.offset(marker);
        func.instruction_mut(marker).set_imm(end_offset);
        func.emit_code_with_imm(Opcode::JUMP, offset, Some(rstmt.token_pos));

        self.branch
            .leave_block(current_func_mut!(self), Some(marker));
    }

    fn visit_empty_stmt(&mut self, _e: &EmptyStmt) {}

    fn visit_bad_stmt(&mut self, _b: &BadStmt) {
        unreachable!();
    }

    fn visit_bad_decl(&mut self, _b: &BadDecl) {
        unreachable!();
    }
}
