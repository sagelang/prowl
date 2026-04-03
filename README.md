# prowl

Bare metal native compiler for the [Sage language](https://github.com/sagelang/sage), built on LLVM.

The main Sage compiler transpiles to Rust and lets `rustc` do the heavy lifting. Prowl skips the middleman — `.sg` source goes straight to native machine code via LLVM IR.

```
hello.sg  →  prowl  →  LLVM IR  →  native binary
```

## Status

Early stage. The pipeline is wired end-to-end (parse → type check → LLVM codegen → link), but codegen currently emits a stub `main`. The roadmap below tracks what's next.

## Usage

Requires LLVM 20 (`brew install llvm@20`).

```sh
cargo install --path crates/prowl
prowl build hello.sg
```

## Roadmap

### Phase 1 — Scalar types & arithmetic
- [ ] Integer literals → LLVM `i64` constants
- [ ] Arithmetic expressions (`+`, `-`, `*`, `/`, `%`)
- [ ] Boolean literals & logical operators
- [ ] Float literals → `f64`

### Phase 2 — Functions
- [ ] Top-level `fn` declarations
- [ ] Function calls
- [ ] Return values
- [ ] Basic recursion

### Phase 3 — Control flow
- [ ] `if` / `else`
- [ ] `while` loops
- [ ] `loop` + `break`
- [ ] Pattern matching on scalars

### Phase 4 — Strings & I/O
- [ ] String literals
- [ ] `print` / `println` via libc
- [ ] String concatenation

### Phase 5 — Compound types
- [ ] Records (structs) — stack allocation
- [ ] Enums with data
- [ ] Pattern matching on enums
- [ ] `Option<T>` and `Result<T, E>`

### Phase 6 — Memory & ownership
- [ ] Heap allocation
- [ ] Lists / arrays
- [ ] Closures

### Phase 7 — Agents
- [ ] Agent state as structs
- [ ] Message passing via native threads
- [ ] `on start` / `on message` handlers
- [ ] Supervision trees

### Phase 8 — LLM & tools
- [ ] `divine` (LLM inference) via Rust or C FFI
- [ ] Built-in tools (Http, Fs, Shell)
- [ ] MCP client

## Architecture

```
crates/
├── prowl/           # CLI binary (prowl build <file.sg>)
└── prowl-codegen/   # LLVM IR emission via inkwell
```

Prowl reuses [`sage-parser`](https://crates.io/crates/sage-parser) and [`sage-checker`](https://crates.io/crates/sage-checker) from the main Sage toolchain. Only the backend is new.
