use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::builder::Builder;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::OptimizationLevel;
use sage_parser::Program;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodegenError {
    #[error("failed to initialise native target: {0}")]
    TargetInit(String),
    #[error("failed to look up target triple '{0}': {1}")]
    TargetLookup(String, String),
    #[error("failed to create target machine")]
    MachineCreation,
    #[error("failed to write object file: {0}")]
    WriteObject(String),
}

/// Compile a Sage program to a native object file at `output`.
pub fn compile(program: &Program, output: &Path) -> Result<(), CodegenError> {
    let context = Context::create();
    let cg = Codegen::new(&context, "sage_main");
    cg.emit_program(program);
    cg.write_object(output)
}

struct Codegen<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
}

impl<'ctx> Codegen<'ctx> {
    fn new(context: &'ctx Context, module_name: &str) -> Self {
        Self {
            module: context.create_module(module_name),
            builder: context.create_builder(),
            context,
        }
    }

    fn emit_program(&self, _program: &Program) {
        // TODO: walk the AST here. For now, emit an empty main.
        self.emit_main();
    }

    /// Emit an empty `main` that returns 0.
    /// This is the starting point — real codegen will walk the AST here.
    fn emit_main(&self) {
        let i32_type = self.context.i32_type();
        let fn_type = i32_type.fn_type(&[], false);
        let function = self.module.add_function("main", fn_type, None);
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        self.builder
            .build_return(Some(&i32_type.const_int(0, false)))
            .expect("build_return");
    }

    fn write_object(&self, output: &Path) -> Result<(), CodegenError> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|e| CodegenError::TargetInit(e))?;

        let triple = TargetMachine::get_default_triple();
        let cpu = TargetMachine::get_host_cpu_name();
        let features = TargetMachine::get_host_cpu_features();

        let target = Target::from_triple(&triple).map_err(|e| {
            CodegenError::TargetLookup(triple.to_string(), e.to_string())
        })?;

        let machine = target
            .create_target_machine(
                &triple,
                cpu.to_str().unwrap_or("generic"),
                features.to_str().unwrap_or(""),
                OptimizationLevel::Default,
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or(CodegenError::MachineCreation)?;

        machine
            .write_to_file(&self.module, FileType::Object, output)
            .map_err(|e| CodegenError::WriteObject(e.to_string()))
    }
}
