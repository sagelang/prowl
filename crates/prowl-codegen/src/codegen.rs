use std::collections::HashMap;

use inkwell::{
    FloatPredicate, IntPredicate,
    basic_block::BasicBlock,
    builder::Builder,
    context::Context,
    module::Module,
    types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum},
    values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue},
};
use sage_parser::{
    TypeExpr,
    ast::{BinOp, Block, ElseBranch, Expr, FnDecl, Literal, Program, Stmt, UnaryOp},
};

/// Maps variable names to their alloca pointer and element type.
/// We carry the element type explicitly because LLVM 15+ uses opaque pointers —
/// a `ptr` has no intrinsic element type, so we must remember it ourselves when
/// emitting `load` instructions.
type Vars<'ctx> = HashMap<String, (PointerValue<'ctx>, BasicTypeEnum<'ctx>)>;

pub struct Codegen<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    builder: Builder<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    pub fn new(context: &'ctx Context, name: &str) -> Self {
        Self {
            module: context.create_module(name),
            builder: context.create_builder(),
            context,
        }
    }

    // =========================================================================
    // Program entry point
    // =========================================================================

    pub fn emit_program(&self, program: &Program) {
        // Forward-declare every function before emitting bodies, so that
        // mutually recursive calls and forward references resolve correctly.
        for f in &program.functions {
            self.declare_fn(f);
        }

        // Emit each function body.
        for f in &program.functions {
            self.emit_fn(f);
        }

        // Emit the C `main` that the OS calls.
        self.emit_c_main(program);
    }

    // =========================================================================
    // Function declaration & body
    // =========================================================================

    /// Mangle a Sage function name to avoid clashing with the C `main` symbol
    /// that prowl generates as the OS entry point.
    fn mangle(name: &str) -> String {
        if name == "main" {
            "sage_main".to_string()
        } else {
            name.to_string()
        }
    }

    /// Forward-declare a function: emit the LLVM function signature with no body.
    fn declare_fn(&self, f: &FnDecl) -> FunctionValue<'ctx> {
        let params: Vec<BasicMetadataTypeEnum<'ctx>> = f
            .params
            .iter()
            .flat_map(|p| self.basic_metadata_type(&p.ty))
            .collect();

        let fn_type = match self.basic_type(&f.return_ty) {
            Some(ret) => ret.fn_type(&params, false),
            None => self.context.void_type().fn_type(&params, false),
        };

        self.module
            .add_function(&Self::mangle(&f.name.to_string()), fn_type, None)
    }

    /// Emit the full body of a function.
    fn emit_fn(&self, f: &FnDecl) {
        let function = self
            .module
            .get_function(&Self::mangle(&f.name.to_string()))
            .expect("function was forward-declared");

        // All allocas go into a dedicated `alloca` block at the top of the function.
        // This lets LLVM's mem2reg pass promote them to SSA values efficiently.
        let alloca_bb = self.context.append_basic_block(function, "alloca");
        let body_bb = self.context.append_basic_block(function, "body");

        // Emit parameter allocas in the alloca block.
        self.builder.position_at_end(alloca_bb);
        let mut vars: Vars<'ctx> = HashMap::new();

        for (i, param) in f.params.iter().enumerate() {
            let llvm_ty = self
                .basic_type(&param.ty)
                .expect("parameter type must be a non-void basic type");
            let alloca = self
                .builder
                .build_alloca(llvm_ty, &param.name.to_string())
                .expect("alloca for param");
            let param_val = function.get_nth_param(i as u32).expect("param exists");
            self.builder
                .build_store(alloca, param_val)
                .expect("store param");
            vars.insert(param.name.to_string(), (alloca, llvm_ty));
        }

        // Jump from the alloca block into the body block.
        self.builder
            .build_unconditional_branch(body_bb)
            .expect("branch to body");

        // Emit the function body. No enclosing loop, so break_bb = None.
        self.builder.position_at_end(body_bb);
        self.emit_block(&f.body, &mut vars, None);

        // If control falls off the end without a `return`, emit a default terminator.
        // (The type checker should have caught missing returns, but LLVM requires every
        // basic block to end with a terminator instruction.)
        if !self.is_terminated() {
            match self.basic_type(&f.return_ty) {
                None => {
                    self.builder.build_return(None).expect("implicit ret void");
                }
                Some(BasicTypeEnum::IntType(t)) => {
                    self.builder
                        .build_return(Some(&t.const_int(0, false)))
                        .expect("implicit ret 0");
                }
                Some(BasicTypeEnum::FloatType(t)) => {
                    self.builder
                        .build_return(Some(&t.const_float(0.0)))
                        .expect("implicit ret 0.0");
                }
                Some(_) => {
                    self.builder.build_unreachable().expect("unreachable");
                }
            }
        }
    }

    // =========================================================================
    // Statements
    // =========================================================================

    /// Emit all statements in a block.
    ///
    /// `break_bb` is the basic block to jump to on `break`, if we are inside
    /// a loop.  It is `None` at function top-level.
    fn emit_block(&self, block: &Block, vars: &mut Vars<'ctx>, break_bb: Option<BasicBlock<'ctx>>) {
        for stmt in &block.stmts {
            if self.is_terminated() {
                break;
            }
            self.emit_stmt(stmt, vars, break_bb);
        }
    }

    fn emit_stmt(&self, stmt: &Stmt, vars: &mut Vars<'ctx>, break_bb: Option<BasicBlock<'ctx>>) {
        match stmt {
            // let x: T = expr;
            Stmt::Let { name, ty, value, .. } => {
                let val = self
                    .emit_expr(value, vars)
                    .expect("let binding value must not be void");
                let llvm_ty = ty
                    .as_ref()
                    .and_then(|t| self.basic_type(t))
                    .unwrap_or_else(|| val.get_type());
                let alloca = self
                    .builder
                    .build_alloca(llvm_ty, &name.to_string())
                    .expect("alloca for let");
                self.builder.build_store(alloca, val).expect("store let");
                vars.insert(name.to_string(), (alloca, llvm_ty));
            }

            // x = expr;
            Stmt::Assign { name, value, .. } => {
                let val = self
                    .emit_expr(value, vars)
                    .expect("assignment value must not be void");
                if let Some(&(ptr, _)) = vars.get(&name.to_string()) {
                    self.builder.build_store(ptr, val).expect("store assign");
                }
            }

            // return expr?;
            Stmt::Return { value, .. } => match value {
                Some(expr) => {
                    let val = self
                        .emit_expr(expr, vars)
                        .expect("return value must not be void");
                    self.builder.build_return(Some(&val)).expect("return");
                }
                None => {
                    self.builder.build_return(None).expect("return void");
                }
            },

            // if cond { ... } else { ... }
            Stmt::If { condition, then_block, else_block, .. } => {
                self.emit_if(condition, then_block, else_block.as_ref(), vars, break_bb);
            }

            // while cond { ... }
            Stmt::While { condition, body, .. } => {
                self.emit_while(condition, body, vars);
            }

            // loop { ... }
            Stmt::Loop { body, .. } => {
                self.emit_loop(body, vars);
            }

            // break;
            Stmt::Break { .. } => {
                let exit_bb = break_bb.expect("break outside of a loop");
                self.builder
                    .build_unconditional_branch(exit_bb)
                    .expect("break branch");
            }

            // expr;  (value discarded)
            Stmt::Expr { expr, .. } => {
                self.emit_expr(expr, vars);
            }

            // Remaining statement kinds deferred to later phases.
            other => {
                let _ = other;
            }
        }
    }

    // =========================================================================
    // Control flow
    // =========================================================================

    /// Emit an `if`/`else` statement.
    ///
    /// LLVM IR shape:
    /// ```text
    ///   br i1 %cond, label %then, label %else
    /// then:
    ///   ...
    ///   br label %merge        ; only if not already terminated
    /// else:
    ///   ...
    ///   br label %merge        ; only if not already terminated
    /// merge:
    ///   ...                    ; execution continues here
    /// ```
    fn emit_if(
        &self,
        condition: &Expr,
        then_block: &Block,
        else_branch: Option<&ElseBranch>,
        vars: &mut Vars<'ctx>,
        break_bb: Option<BasicBlock<'ctx>>,
    ) {
        let func = self.current_fn();
        let then_bb  = self.context.append_basic_block(func, "then");
        let else_bb  = self.context.append_basic_block(func, "else");
        let merge_bb = self.context.append_basic_block(func, "merge");

        // Emit the condition and branch.
        let cond = self
            .emit_expr(condition, vars)
            .expect("if condition must be a value")
            .into_int_value();
        self.builder
            .build_conditional_branch(cond, then_bb, else_bb)
            .expect("cond branch");

        // Emit the `then` branch.
        self.builder.position_at_end(then_bb);
        self.emit_block(then_block, vars, break_bb);
        if !self.is_terminated() {
            self.builder
                .build_unconditional_branch(merge_bb)
                .expect("then → merge");
        }

        // Emit the `else` branch (or just fall through to merge).
        self.builder.position_at_end(else_bb);
        match else_branch {
            None => {
                // No else clause — else block goes straight to merge.
                self.builder
                    .build_unconditional_branch(merge_bb)
                    .expect("else → merge");
            }
            Some(ElseBranch::Block(block)) => {
                self.emit_block(block, vars, break_bb);
                if !self.is_terminated() {
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .expect("else → merge");
                }
            }
            Some(ElseBranch::ElseIf(stmt)) => {
                // `else if` — emit the nested if directly into this block.
                self.emit_stmt(stmt, vars, break_bb);
                if !self.is_terminated() {
                    self.builder
                        .build_unconditional_branch(merge_bb)
                        .expect("else-if → merge");
                }
            }
        }

        // Continue emitting after the if/else into the merge block.
        self.builder.position_at_end(merge_bb);
    }

    /// Emit a `while` loop.
    ///
    /// LLVM IR shape:
    /// ```text
    ///   br label %cond
    /// cond:
    ///   %t = <condition>
    ///   br i1 %t, label %body, label %exit
    /// body:
    ///   ...
    ///   br label %cond         ; loop back (only if not terminated)
    /// exit:
    ///   ...                    ; execution continues here
    /// ```
    fn emit_while(&self, condition: &Expr, body: &Block, vars: &mut Vars<'ctx>) {
        let func = self.current_fn();
        let cond_bb = self.context.append_basic_block(func, "while_cond");
        let body_bb = self.context.append_basic_block(func, "while_body");
        let exit_bb = self.context.append_basic_block(func, "while_exit");

        // Jump into the condition check.
        self.builder
            .build_unconditional_branch(cond_bb)
            .expect("→ while_cond");

        // Emit condition.
        self.builder.position_at_end(cond_bb);
        let cond = self
            .emit_expr(condition, vars)
            .expect("while condition must be a value")
            .into_int_value();
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .expect("while cond branch");

        // Emit body. Pass exit_bb so `break` inside can jump out.
        self.builder.position_at_end(body_bb);
        self.emit_block(body, vars, Some(exit_bb));
        if !self.is_terminated() {
            self.builder
                .build_unconditional_branch(cond_bb)
                .expect("while_body → while_cond");
        }

        // Continue after the loop.
        self.builder.position_at_end(exit_bb);
    }

    /// Emit an infinite `loop { ... }`.
    ///
    /// LLVM IR shape:
    /// ```text
    ///   br label %loop
    /// loop:
    ///   ...
    ///   br label %loop         ; only if not terminated / broken
    /// exit:
    ///   ...                    ; reachable only via `break`
    /// ```
    fn emit_loop(&self, body: &Block, vars: &mut Vars<'ctx>) {
        let func = self.current_fn();
        let loop_bb = self.context.append_basic_block(func, "loop");
        let exit_bb = self.context.append_basic_block(func, "loop_exit");

        self.builder
            .build_unconditional_branch(loop_bb)
            .expect("→ loop");

        self.builder.position_at_end(loop_bb);
        self.emit_block(body, vars, Some(exit_bb));
        if !self.is_terminated() {
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("loop → loop");
        }

        self.builder.position_at_end(exit_bb);
    }

    // =========================================================================
    // Expressions
    // =========================================================================

    /// Emit an expression and return its LLVM value.
    /// Returns `None` for void/unit expressions (e.g. calls to Unit-returning fns).
    fn emit_expr(&self, expr: &Expr, vars: &Vars<'ctx>) -> Option<BasicValueEnum<'ctx>> {
        match expr {
            Expr::Literal { value, .. } => Some(self.emit_literal(value)),

            Expr::Var { name, .. } => {
                let (ptr, ty) = vars
                    .get(&name.to_string())
                    .unwrap_or_else(|| panic!("undefined variable '{name}'"));
                Some(
                    self.builder
                        .build_load(*ty, *ptr, &name.to_string())
                        .expect("load var"),
                )
            }

            Expr::Binary { op, left, right, .. } => {
                Some(self.emit_binary(*op, left, right, vars))
            }

            Expr::Unary { op, operand, .. } => Some(self.emit_unary(*op, operand, vars)),

            Expr::Call { name, args, .. } => self.emit_call(&name.to_string(), args, vars),

            Expr::Paren { inner, .. } => self.emit_expr(inner, vars),

            other => {
                panic!("expression not yet supported: {other:?}");
            }
        }
    }

    fn emit_literal(&self, lit: &Literal) -> BasicValueEnum<'ctx> {
        match lit {
            Literal::Int(n) => self
                .context
                .i64_type()
                .const_int(*n as u64, true)
                .into(),
            Literal::Float(f) => self.context.f64_type().const_float(*f).into(),
            Literal::Bool(b) => self
                .context
                .bool_type()
                .const_int(*b as u64, false)
                .into(),
            Literal::String(_) => panic!("string literals are deferred to Phase 4"),
        }
    }

    fn emit_binary(
        &self,
        op: BinOp,
        left: &Expr,
        right: &Expr,
        vars: &Vars<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let lhs = self
            .emit_expr(left, vars)
            .expect("binary lhs must have a value");
        let rhs = self
            .emit_expr(right, vars)
            .expect("binary rhs must have a value");

        if lhs.is_int_value() {
            let l = lhs.into_int_value();
            let r = rhs.into_int_value();
            match op {
                BinOp::Add => self.builder.build_int_add(l, r, "add").unwrap().into(),
                BinOp::Sub => self.builder.build_int_sub(l, r, "sub").unwrap().into(),
                BinOp::Mul => self.builder.build_int_mul(l, r, "mul").unwrap().into(),
                BinOp::Div => self.builder.build_int_signed_div(l, r, "div").unwrap().into(),
                BinOp::Rem => self.builder.build_int_signed_rem(l, r, "rem").unwrap().into(),
                BinOp::Eq  => self.builder.build_int_compare(IntPredicate::EQ,  l, r, "eq").unwrap().into(),
                BinOp::Ne  => self.builder.build_int_compare(IntPredicate::NE,  l, r, "ne").unwrap().into(),
                BinOp::Lt  => self.builder.build_int_compare(IntPredicate::SLT, l, r, "lt").unwrap().into(),
                BinOp::Gt  => self.builder.build_int_compare(IntPredicate::SGT, l, r, "gt").unwrap().into(),
                BinOp::Le  => self.builder.build_int_compare(IntPredicate::SLE, l, r, "le").unwrap().into(),
                BinOp::Ge  => self.builder.build_int_compare(IntPredicate::SGE, l, r, "ge").unwrap().into(),
                BinOp::And => self.builder.build_and(l, r, "and").unwrap().into(),
                BinOp::Or  => self.builder.build_or(l, r, "or").unwrap().into(),
                BinOp::Concat => panic!("string concatenation deferred to Phase 4"),
            }
        } else if lhs.is_float_value() {
            let l = lhs.into_float_value();
            let r = rhs.into_float_value();
            match op {
                BinOp::Add => self.builder.build_float_add(l, r, "fadd").unwrap().into(),
                BinOp::Sub => self.builder.build_float_sub(l, r, "fsub").unwrap().into(),
                BinOp::Mul => self.builder.build_float_mul(l, r, "fmul").unwrap().into(),
                BinOp::Div => self.builder.build_float_div(l, r, "fdiv").unwrap().into(),
                BinOp::Rem => self.builder.build_float_rem(l, r, "frem").unwrap().into(),
                BinOp::Eq  => self.builder.build_float_compare(FloatPredicate::OEQ, l, r, "feq").unwrap().into(),
                BinOp::Ne  => self.builder.build_float_compare(FloatPredicate::ONE, l, r, "fne").unwrap().into(),
                BinOp::Lt  => self.builder.build_float_compare(FloatPredicate::OLT, l, r, "flt").unwrap().into(),
                BinOp::Gt  => self.builder.build_float_compare(FloatPredicate::OGT, l, r, "fgt").unwrap().into(),
                BinOp::Le  => self.builder.build_float_compare(FloatPredicate::OLE, l, r, "fle").unwrap().into(),
                BinOp::Ge  => self.builder.build_float_compare(FloatPredicate::OGE, l, r, "fge").unwrap().into(),
                op => panic!("operator {op:?} not valid for floats"),
            }
        } else {
            panic!("unsupported operand types for binary op");
        }
    }

    fn emit_unary(&self, op: UnaryOp, operand: &Expr, vars: &Vars<'ctx>) -> BasicValueEnum<'ctx> {
        let val = self
            .emit_expr(operand, vars)
            .expect("unary operand must have a value");
        match op {
            UnaryOp::Neg => {
                if val.is_int_value() {
                    self.builder
                        .build_int_neg(val.into_int_value(), "neg")
                        .unwrap()
                        .into()
                } else {
                    self.builder
                        .build_float_neg(val.into_float_value(), "fneg")
                        .unwrap()
                        .into()
                }
            }
            UnaryOp::Not => self
                .builder
                .build_not(val.into_int_value(), "not")
                .unwrap()
                .into(),
        }
    }

    fn emit_call(
        &self,
        name: &str,
        args: &[Expr],
        vars: &Vars<'ctx>,
    ) -> Option<BasicValueEnum<'ctx>> {
        let mangled = Self::mangle(name);
        let function = self
            .module
            .get_function(&mangled)
            .unwrap_or_else(|| panic!("function '{name}' not declared"));

        let arg_vals: Vec<BasicMetadataValueEnum<'ctx>> = args
            .iter()
            .map(|a| {
                self.emit_expr(a, vars)
                    .expect("function argument must not be void")
                    .into()
            })
            .collect();

        let call = self
            .builder
            .build_call(function, &arg_vals, name)
            .expect("build_call");

        call.try_as_basic_value().basic()
    }

    // =========================================================================
    // C `main` entry point
    // =========================================================================

    fn emit_c_main(&self, program: &Program) {
        let i32_type = self.context.i32_type();
        let main_fn = self
            .module
            .add_function("main", i32_type.fn_type(&[], false), None);
        let entry_bb = self.context.append_basic_block(main_fn, "entry");
        self.builder.position_at_end(entry_bb);

        let entry_fn = program.functions.iter().find(|f| {
            let fname = f.name.to_string();
            let is_entry_name = fname == "main"
                || program
                    .run_agent
                    .as_ref()
                    .map(|r| r.to_string() == fname)
                    .unwrap_or(false);
            is_entry_name && f.params.is_empty()
        });

        if let Some(f) = entry_fn {
            let target = self
                .module
                .get_function(&Self::mangle(&f.name.to_string()))
                .expect("entry function was declared");

            let call = self
                .builder
                .build_call(target, &[], "entry_call")
                .expect("call entry fn");

            match call.try_as_basic_value().basic() {
                Some(v) if v.is_int_value() => {
                    let i32_val = self
                        .builder
                        .build_int_truncate(v.into_int_value(), i32_type, "exit_code")
                        .expect("truncate to i32");
                    self.builder
                        .build_return(Some(&i32_val))
                        .expect("return exit code");
                }
                _ => {
                    self.builder
                        .build_return(Some(&i32_type.const_int(0, false)))
                        .expect("return 0");
                }
            }
        } else {
            self.builder
                .build_return(Some(&i32_type.const_int(0, false)))
                .expect("return 0");
        }
    }

    // =========================================================================
    // Type helpers
    // =========================================================================

    pub fn basic_type(&self, ty: &TypeExpr) -> Option<BasicTypeEnum<'ctx>> {
        match ty {
            TypeExpr::Int   => Some(self.context.i64_type().into()),
            TypeExpr::Float => Some(self.context.f64_type().into()),
            TypeExpr::Bool  => Some(self.context.bool_type().into()),
            TypeExpr::Unit  => None,
            other => panic!("type '{other}' not yet supported — see roadmap phases 4-6"),
        }
    }

    fn basic_metadata_type(&self, ty: &TypeExpr) -> Option<BasicMetadataTypeEnum<'ctx>> {
        match ty {
            TypeExpr::Int   => Some(self.context.i64_type().into()),
            TypeExpr::Float => Some(self.context.f64_type().into()),
            TypeExpr::Bool  => Some(self.context.bool_type().into()),
            TypeExpr::Unit  => None,
            other => panic!("type '{other}' not yet supported — see roadmap phases 4-6"),
        }
    }

    // =========================================================================
    // Utilities
    // =========================================================================

    /// Returns the function currently being built.
    fn current_fn(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .expect("no active basic block")
            .get_parent()
            .expect("basic block has no parent function")
    }

    /// Returns true if the current basic block already has a terminator.
    fn is_terminated(&self) -> bool {
        self.builder
            .get_insert_block()
            .and_then(|bb| bb.get_terminator())
            .is_some()
    }
}
