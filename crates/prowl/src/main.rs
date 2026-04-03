use clap::{Parser, Subcommand};
use miette::{miette, IntoDiagnostic, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "sage-native", about = "Native LLVM compiler for Sage")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compile a Sage source file to a native binary
    Build {
        /// Path to the .sg source file
        source: PathBuf,
        /// Output binary name (default: source stem)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Cmd::Build { source, output } => build(&source, output.as_deref()),
    }
}

fn build(source: &Path, output: Option<&Path>) -> Result<()> {
    let src = std::fs::read_to_string(source)
        .into_diagnostic()
        .map_err(|e| miette!("could not read '{}': {e}", source.display()))?;

    // Parse
    let lex = sage_parser::lex(&src)
        .map_err(|e| miette!("lex error: {e:?}"))?;
    let src_arc: Arc<str> = Arc::from(src.as_str());
    let (program, parse_errors) = sage_parser::parse(lex.tokens(), src_arc);

    if !parse_errors.is_empty() {
        for e in &parse_errors {
            eprintln!("parse error: {e:?}");
        }
        return Err(miette!("parse failed"));
    }

    let program = program.ok_or_else(|| miette!("empty program"))?;

    // Type check
    let check = sage_checker::check(&program);
    if !check.errors.is_empty() {
        for e in &check.errors {
            eprintln!("type error: {e:?}");
        }
        return Err(miette!("type checking failed"));
    }

    // Codegen → object file
    let obj_path = source.with_extension("o");
    prowl_codegen::compile(&program, &obj_path)
        .map_err(|e| miette!("codegen error: {e}"))?;

    // Link
    let bin_path = output
        .map(PathBuf::from)
        .unwrap_or_else(|| source.with_extension(""));

    let status = Command::new("cc")
        .arg(&obj_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .into_diagnostic()
        .map_err(|e| miette!("linker error: {e}"))?;

    if !status.success() {
        return Err(miette!("linking failed"));
    }

    std::fs::remove_file(&obj_path).ok();

    println!("compiled: {}", bin_path.display());
    Ok(())
}
