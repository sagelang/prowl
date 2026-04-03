mod codegen;
mod error;

pub use codegen::Codegen;
pub use error::CodegenError;

use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::OptimizationLevel;
use sage_parser::Program;
use std::path::Path;

/// Compile a Sage program to a native object file at `output`.
pub fn compile(program: &Program, output: &Path) -> Result<(), CodegenError> {
    let context = Context::create();
    let cg = Codegen::new(&context, "sage_main");
    cg.emit_program(program);
    write_object(&cg.module, output)
}

/// Compile a Sage program and return the LLVM IR as a string.
/// Useful for inspecting what prowl actually generates.
pub fn emit_ir(program: &Program) -> String {
    let context = Context::create();
    let cg = Codegen::new(&context, "sage_main");
    cg.emit_program(program);
    cg.module.print_to_string().to_string()
}

fn write_object(
    module: &inkwell::module::Module,
    output: &Path,
) -> Result<(), CodegenError> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| CodegenError::TargetInit(e))?;

    let triple = TargetMachine::get_default_triple();
    let cpu = TargetMachine::get_host_cpu_name();
    let features = TargetMachine::get_host_cpu_features();

    let target = Target::from_triple(&triple)
        .map_err(|e| CodegenError::TargetLookup(triple.to_string(), e.to_string()))?;

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
        .write_to_file(module, FileType::Object, output)
        .map_err(|e| CodegenError::WriteObject(e.to_string()))
}
