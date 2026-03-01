//! Code generation for Dioxus v0.7 signals and hooks with SpacetimeDB v2.0.
//!
//! Generates a `dioxus.rs` module that wraps the standard Rust SDK types with Dioxus
//! reactive signals and hooks. This provides:
//!
//! - `use_spacetimedb_context_provider` — Root-level hook that establishes the connection
//!   and creates `SyncSignal`s for all tables.
//! - `use_table_*` — Per-table hooks returning `SyncSignal<Vec<Row>>`.
//! - `use_reducer_*` — Per-reducer hooks returning callable closures.
//! - `use_subscription` — Hook for subscribing to SQL queries.
//! - Connection state hooks for monitoring connection status.

use super::code_indenter::{CodeIndenter, Indenter};
use crate::rust::type_name;
use crate::util::{
    collect_case, is_reducer_invokable, iter_reducers, iter_table_names_and_types, iter_tables,
    print_auto_generated_file_comment, print_auto_generated_version_comment, type_ref_name, CodegenVisibility,
};
use crate::{CodegenOptions, Lang, OutputFile};
use convert_case::{Case, Casing};
use spacetimedb_lib::sats::AlgebraicTypeRef;
use spacetimedb_schema::def::{ModuleDef, ProcedureDef, ReducerDef, TableDef, TypeDef};
use spacetimedb_schema::schema::TableSchema;
use spacetimedb_schema::type_for_generate::AlgebraicTypeUse;
use std::collections::HashMap;
use std::ops::Deref;

const INDENT: &str = "    ";

/// Get the module name for a type reference (e.g., "create_event_args_type").
/// This is a local implementation to avoid depending on private functions in rust.rs.
fn get_type_module_name(module: &ModuleDef, type_ref: AlgebraicTypeRef) -> String {
    let (name, _) = module.type_def_from_ref(type_ref).unwrap();
    collect_case(Case::Snake, name.name_segments()) + "_type"
}

/// Generate a reducer file with proper handling of name conflicts via aliases.
/// This is a complete reimplementation that avoids name shadowing issues.
fn generate_reducer_file_with_aliases(module: &ModuleDef, reducer: &ReducerDef) -> OutputFile {
    use std::collections::{BTreeSet, HashMap};
    use std::fmt::Write as _;

    const INDENT: &str = "    ";
    const STRUCT_DERIVES: &[&str] = &[
        "#[derive(__lib::ser::Serialize, __lib::de::Deserialize, Clone, PartialEq, Debug)]",
        "#[sats(crate = __lib)]",
    ];

    let mut output = CodeIndenter::new(String::new(), INDENT);
    let out = &mut output;

    // File header
    print_auto_generated_file_comment(out);
    print_auto_generated_version_comment(out);
    writeln!(out, "#![allow(unused, clippy::all)]");
    writeln!(out, "use spacetimedb_sdk::__codegen::{{");
    writeln!(out, "    self as __sdk,");
    writeln!(out, "    __lib,");
    writeln!(out, "    __sats,");
    writeln!(out, "    __ws,");
    writeln!(out, "}};");
    writeln!(out);

    // Calculate the wrapper struct name
    let args_type_name = reducer.accessor_name.deref().to_case(Case::Pascal) + "Args";

    // Collect imports and check for conflicts
    let mut imports = BTreeSet::new();
    for (_, ty) in &reducer.params_for_generate.elements {
        ty.for_each_ref(|r| {
            imports.insert(r);
        });
    }

    // Track which types need aliases
    let mut type_aliases = HashMap::new();
    for type_ref in &imports {
        let type_name = type_ref_name(module, *type_ref);
        if type_name == args_type_name {
            // This type conflicts with our wrapper struct name
            let alias_name = type_name.clone() + "Type";
            type_aliases.insert(*type_ref, alias_name);
        }
    }

    // Print imports with aliases where needed
    for type_ref in &imports {
        let module_name = get_type_module_name(module, *type_ref);
        let type_name = type_ref_name(module, *type_ref);

        if let Some(alias) = type_aliases.get(type_ref) {
            writeln!(out, "use super::{module_name}::{type_name} as {alias};");
        } else {
            writeln!(out, "use super::{module_name}::{type_name};");
        }
    }
    writeln!(out);

    // Print struct derives
    for derive in STRUCT_DERIVES {
        writeln!(out, "{derive}");
    }

    // Define struct
    writeln!(out, "pub(super) struct {args_type_name} {{");
    for (ident, ty) in &reducer.params_for_generate.elements {
        let field_name = ident.deref().to_case(Case::Snake);
        write!(out, "    pub {field_name}: ");

        // Use aliased name if this type conflicts
        match ty {
            AlgebraicTypeUse::Ref(r) if type_aliases.contains_key(r) => {
                write!(out, "{}", type_aliases.get(r).unwrap());
            }
            _ => {
                write_type_inline(module, out, ty, &type_aliases);
            }
        }
        writeln!(out, ",");
    }
    writeln!(out, "}}");
    writeln!(out);

    // Generate From impl
    let enum_variant_name = reducer.accessor_name.deref().to_case(Case::Pascal);
    writeln!(out, "impl From<{args_type_name}> for super::Reducer {{");
    writeln!(out, "    fn from(args: {args_type_name}) -> Self {{");
    write!(out, "        Self::{enum_variant_name}");
    if !reducer.params_for_generate.elements.is_empty() {
        writeln!(out, " {{");
        for (ident, _) in &reducer.params_for_generate.elements {
            let arg_name = ident.deref().to_case(Case::Snake);
            writeln!(out, "            {arg_name}: args.{arg_name},");
        }
        writeln!(out, "        }}");
    }
    writeln!(out, "    }}");
    writeln!(out, "}}");
    writeln!(out);

    // InModule impl
    writeln!(out, "impl __sdk::InModule for {args_type_name} {{");
    writeln!(out, "    type Module = super::RemoteModule;");
    writeln!(out, "}}");
    writeln!(out);

    // Generate trait and impl
    let reducer_name = reducer.name.deref();
    let func_name = reducer.accessor_name.deref().to_case(Case::Snake);

    // Build arglist
    let mut arglist = String::new();
    let mut arg_names = String::new();
    for (i, (ident, ty)) in reducer.params_for_generate.elements.iter().enumerate() {
        if i > 0 {
            arglist.push_str(", ");
            arg_names.push_str(", ");
        }
        let arg_name = ident.deref().to_case(Case::Snake);
        arglist.push_str(&arg_name);
        arglist.push_str(": ");

        match ty {
            AlgebraicTypeUse::Ref(r) if type_aliases.contains_key(r) => {
                arglist.push_str(type_aliases.get(r).unwrap());
            }
            _ => {
                write_type_to_string(&mut arglist, module, ty, &type_aliases);
            }
        }

        arg_names.push_str(&arg_name);
    }

    writeln!(out, "#[allow(non_camel_case_types)]");
    writeln!(out, "/// Extension trait for access to the reducer `{reducer_name}`.");
    writeln!(out, "///");
    writeln!(out, "/// Implemented for [`super::RemoteReducers`].");
    writeln!(out, "pub trait {func_name} {{");
    writeln!(
        out,
        "    /// Request that the remote module invoke the reducer `{reducer_name}` to run as soon as possible."
    );
    writeln!(out, "    ///");
    writeln!(
        out,
        "    /// This method returns immediately, and errors only if we are unable to send the request."
    );
    writeln!(out, "    /// The reducer will run asynchronously in the future,");
    writeln!(
        out,
        "    ///  and this method provides no way to listen for its completion status."
    );
    writeln!(out, "    ///");
    writeln!(
        out,
        "    /// Use [`{func_name}::{func_name}_then`] to run a callback after the reducer completes."
    );
    writeln!(out, "    fn {func_name}(&self, {arglist}) -> __sdk::Result<()> {{");
    writeln!(out, "        self.{func_name}_then({arg_names}, |_, _| {{}})");
    writeln!(out, "    }}");
    writeln!(out);
    writeln!(
        out,
        "    /// Request that the remote module invoke the reducer `{reducer_name}` to run as soon as possible,"
    );
    writeln!(
        out,
        "    /// registering `callback` to run when we are notified that the reducer completed."
    );
    writeln!(out, "    ///");
    writeln!(
        out,
        "    /// This method returns immediately, and errors only if we are unable to send the request."
    );
    writeln!(out, "    /// The reducer will run asynchronously in the future,");
    writeln!(out, "    ///  and its status can be observed with the `callback`.");
    writeln!(out, "    fn {func_name}_then(");
    writeln!(out, "        &self,");
    writeln!(out, "        {arglist},");
    writeln!(
        out,
        "        callback: impl FnOnce(&super::ReducerEventContext, Result<Result<(), String>, __sdk::InternalError>)"
    );
    writeln!(out, "            + Send");
    writeln!(out, "            + 'static,");
    writeln!(out, "    ) -> __sdk::Result<()>;");
    writeln!(out, "}}");
    writeln!(out);

    writeln!(out, "impl {func_name} for super::RemoteReducers {{");
    writeln!(out, "    fn {func_name}_then(");
    writeln!(out, "        &self,");
    writeln!(out, "        {arglist},");
    writeln!(
        out,
        "        callback: impl FnOnce(&super::ReducerEventContext, Result<Result<(), String>, __sdk::InternalError>)"
    );
    writeln!(out, "            + Send");
    writeln!(out, "            + 'static,");
    writeln!(out, "    ) -> __sdk::Result<()> {{");
    writeln!(
        out,
        "        self.imp.invoke_reducer_with_callback({args_type_name} {{ {arg_names} }}, callback)"
    );
    writeln!(out, "    }}");
    writeln!(out, "}}");

    OutputFile {
        filename: reducer.accessor_name.deref().to_case(Case::Snake) + "_reducer.rs",
        code: output.into_inner(),
    }
}

/// Helper to write type inline with alias support
fn write_type_inline<W: std::fmt::Write>(
    module: &ModuleDef,
    out: &mut W,
    ty: &AlgebraicTypeUse,
    aliases: &HashMap<AlgebraicTypeRef, String>,
) {
    match ty {
        AlgebraicTypeUse::Unit => write!(out, "()").unwrap(),
        AlgebraicTypeUse::Never => write!(out, "std::convert::Infallible").unwrap(),
        AlgebraicTypeUse::Identity => write!(out, "__sdk::Identity").unwrap(),
        AlgebraicTypeUse::ConnectionId => write!(out, "__sdk::ConnectionId").unwrap(),
        AlgebraicTypeUse::Timestamp => write!(out, "__sdk::Timestamp").unwrap(),
        AlgebraicTypeUse::TimeDuration => write!(out, "__sdk::TimeDuration").unwrap(),
        AlgebraicTypeUse::Uuid => write!(out, "__sdk::Uuid").unwrap(),
        AlgebraicTypeUse::ScheduleAt => write!(out, "__sdk::ScheduleAt").unwrap(),
        AlgebraicTypeUse::Option(inner) => {
            write!(out, "Option<").unwrap();
            write_type_inline(module, out, inner, aliases);
            write!(out, ">").unwrap();
        }
        AlgebraicTypeUse::Result { ok_ty, err_ty } => {
            write!(out, "Result<").unwrap();
            write_type_inline(module, out, ok_ty, aliases);
            write!(out, ", ").unwrap();
            write_type_inline(module, out, err_ty, aliases);
            write!(out, ">").unwrap();
        }
        AlgebraicTypeUse::Primitive(prim) => {
            use spacetimedb_lib::sats::layout::PrimitiveType;
            let name = match prim {
                PrimitiveType::Bool => "bool",
                PrimitiveType::I8 => "i8",
                PrimitiveType::U8 => "u8",
                PrimitiveType::I16 => "i16",
                PrimitiveType::U16 => "u16",
                PrimitiveType::I32 => "i32",
                PrimitiveType::U32 => "u32",
                PrimitiveType::I64 => "i64",
                PrimitiveType::U64 => "u64",
                PrimitiveType::I128 => "i128",
                PrimitiveType::U128 => "u128",
                PrimitiveType::I256 => "__sats::i256",
                PrimitiveType::U256 => "__sats::u256",
                PrimitiveType::F32 => "f32",
                PrimitiveType::F64 => "f64",
            };
            write!(out, "{name}").unwrap();
        }
        AlgebraicTypeUse::String => write!(out, "String").unwrap(),
        AlgebraicTypeUse::Array(elem) => {
            write!(out, "Vec<").unwrap();
            write_type_inline(module, out, elem, aliases);
            write!(out, ">").unwrap();
        }
        AlgebraicTypeUse::Ref(r) => {
            if let Some(alias) = aliases.get(r) {
                write!(out, "{alias}").unwrap();
            } else {
                write!(out, "{}", type_ref_name(module, *r)).unwrap();
            }
        }
    }
}

/// Helper to write type to string
fn write_type_to_string(
    s: &mut String,
    module: &ModuleDef,
    ty: &AlgebraicTypeUse,
    aliases: &HashMap<AlgebraicTypeRef, String>,
) {
    write_type_inline(module, s, ty, aliases);
}

pub struct Dioxus;

impl Lang for Dioxus {
    fn generate_type_files(&self, module: &ModuleDef, typ: &TypeDef) -> Vec<OutputFile> {
        crate::rust::Rust.generate_type_files(module, typ)
    }

    fn generate_table_file_from_schema(&self, module: &ModuleDef, table: &TableDef, schema: TableSchema) -> OutputFile {
        crate::rust::Rust.generate_table_file_from_schema(module, table, schema)
    }

    fn generate_reducer_file(&self, module: &ModuleDef, reducer: &ReducerDef) -> OutputFile {
        // Override reducer file generation to handle name conflicts with aliases
        generate_reducer_file_with_aliases(module, reducer)
    }

    fn generate_procedure_file(&self, module: &ModuleDef, procedure: &ProcedureDef) -> OutputFile {
        crate::rust::Rust.generate_procedure_file(module, procedure)
    }

    fn generate_global_files(&self, module: &ModuleDef, options: &CodegenOptions) -> Vec<OutputFile> {
        let mut files = crate::rust::Rust.generate_global_files(module, options);
        files.push(generate_dioxus_hooks(module, options.visibility));

        // Inject `pub mod dioxus;` into mod.rs
        if let Some(mod_file) = files.iter_mut().find(|f| f.filename == "mod.rs") {
            if let Some(pos) = mod_file.code.find("\npub mod ") {
                mod_file.code.insert_str(pos, "\npub mod dioxus;\n");
            }
        }

        files
    }
}

/// Collect table metadata for code generation.
struct TableInfo {
    /// snake_case accessor name (e.g. "todo")
    accessor_snake: String,
    /// PascalCase row type name (e.g. "Todo")
    row_type: String,
    /// Whether the table has a primary key (enables on_update callback)
    has_primary_key: bool,
}

fn collect_tables(module: &ModuleDef, visibility: CodegenVisibility) -> Vec<TableInfo> {
    let tables_with_pk: std::collections::HashSet<String> = iter_tables(module, visibility)
        .filter(|t| t.primary_key.is_some())
        .map(|t| t.accessor_name.deref().to_string())
        .collect();

    iter_table_names_and_types(module, visibility)
        .map(|(_, accessor_name, product_type_ref)| {
            let accessor_snake = accessor_name.deref().to_case(Case::Snake);
            let row_type = type_ref_name(module, product_type_ref);
            let has_primary_key = tables_with_pk.contains(accessor_name.deref());
            TableInfo {
                accessor_snake,
                row_type,
                has_primary_key,
            }
        })
        .collect()
}

/// Collect reducer metadata for code generation.
struct ReducerInfo {
    /// snake_case name (e.g. "add_todo")
    name_snake: String,
    /// Original reducer name
    name_orig: String,
    /// Parameter list formatted as "name: Type, name: Type"
    arglist: String,
    /// Parameter names formatted as "name, name"
    arg_names: String,
    /// Closure parameter list formatted as "name: Type, name: Type"
    closure_params: String,
    /// Fn trait parameter list (types only) formatted as "Type, Type"
    fn_trait_params: String,
}

/// Generate type name for use in dioxus hooks, with proper module qualification
/// to avoid name conflicts with reducer wrapper structs.
fn type_name_for_hook(module: &ModuleDef, ty: &AlgebraicTypeUse, reducer_wrapper_name: &str) -> String {
    match ty {
        AlgebraicTypeUse::Ref(type_ref) => {
            let type_name_str = type_ref_name(module, *type_ref);
            // If this type would conflict with the reducer wrapper struct name,
            // use module-qualified path to disambiguate.
            if type_name_str == reducer_wrapper_name {
                let module_name = get_type_module_name(module, *type_ref);
                format!("{module_name}::{type_name_str}")
            } else {
                type_name_str
            }
        }
        _ => {
            // For non-ref types, use the standard type_name function
            type_name(module, ty)
        }
    }
}

fn collect_reducers(module: &ModuleDef, visibility: CodegenVisibility) -> Vec<ReducerInfo> {
    iter_reducers(module, visibility)
        .filter(|r| is_reducer_invokable(r))
        .map(|reducer| {
            let name_snake = reducer.name.deref().to_case(Case::Snake);
            let name_orig = reducer.name.deref().to_string();

            // The wrapper struct name that will be generated for this reducer
            let wrapper_struct_name = reducer.accessor_name.deref().to_case(Case::Pascal) + "Args";

            let mut arglist = String::new();
            let mut arg_names = String::new();
            let mut closure_params = String::new();
            let mut fn_trait_params = String::new();

            for (i, (arg_ident, arg_ty)) in reducer.params_for_generate.elements.iter().enumerate() {
                let arg_name = arg_ident.deref().to_case(Case::Snake);
                // Use the new function to generate type names that avoid conflicts
                let arg_type = type_name_for_hook(module, arg_ty, &wrapper_struct_name);

                if i > 0 {
                    arglist.push_str(", ");
                    arg_names.push_str(", ");
                    closure_params.push_str(", ");
                    fn_trait_params.push_str(", ");
                }
                arglist.push_str(&format!("{arg_name}: {arg_type}"));
                arg_names.push_str(&arg_name);
                closure_params.push_str(&format!("{arg_name}: {arg_type}"));
                fn_trait_params.push_str(&arg_type.to_string());
            }

            ReducerInfo {
                name_snake,
                name_orig,
                arglist,
                arg_names,
                closure_params,
                fn_trait_params,
            }
        })
        .collect()
}

fn generate_dioxus_hooks(module: &ModuleDef, visibility: CodegenVisibility) -> OutputFile {
    let tables = collect_tables(module, visibility);
    let reducers = collect_reducers(module, visibility);

    let mut output = CodeIndenter::new(String::new(), INDENT);
    let out = &mut output;

    print_auto_generated_file_comment(out);
    print_auto_generated_version_comment(out);
    writeln!(out, "#![allow(unused, clippy::all)]");
    writeln!(out);

    // Module doc
    writeln!(out, "//! Dioxus v0.7 signals and hooks for SpacetimeDB integration.");
    writeln!(out);

    // Imports
    writeln!(out, "use super::*;");
    writeln!(out, "use ::dioxus::prelude::*;");
    writeln!(out, "use ::dioxus::signals::SyncSignal;");
    writeln!(
        out,
        "use spacetimedb_sdk::{{DbContext, Table, TableWithPrimaryKey, Identity}};"
    );
    writeln!(out, "use std::sync::Arc;");
    writeln!(out);

    // SharedConnection type alias
    writeln!(out, "/// Thread-safe wrapper for the database connection.");
    writeln!(out, "pub type SharedConnection = Arc<DbConnection>;");
    writeln!(out);

    // TableSignals struct
    writeln!(out, "/// Container for all table signals, created at root level.");
    writeln!(out, "#[derive(Clone)]");
    writeln!(out, "pub struct TableSignals {{");
    for table in &tables {
        writeln!(
            out,
            "    pub {}: SyncSignal<Vec<{}>>,",
            table.accessor_snake, table.row_type
        );
    }
    writeln!(out, "}}");
    writeln!(out);

    // SpacetimeDbContext struct
    writeln!(out, "/// Internal state for managing the SpacetimeDB connection.");
    writeln!(out, "#[derive(Clone)]");
    writeln!(out, "pub struct SpacetimeDbContext {{");
    writeln!(
        out,
        "    /// The database connection (wrapped in Arc for thread-safety)."
    );
    writeln!(out, "    pub connection: SyncSignal<Option<SharedConnection>>,");
    writeln!(out, "    /// The current connection state.");
    writeln!(out, "    pub state: SyncSignal<ConnectionState>,");
    writeln!(out, "    /// Error from the last connection attempt, if any.");
    writeln!(out, "    pub error: SyncSignal<Option<String>>,");
    writeln!(out, "    /// All table signals, created at root level.");
    writeln!(out, "    pub tables: TableSignals,");
    writeln!(out, "}}");
    writeln!(out);

    // ConnectionState enum
    writeln!(out, "/// The current state of the SpacetimeDB connection.");
    writeln!(out, "#[derive(Clone, PartialEq, Eq, Debug, Default)]");
    writeln!(out, "pub enum ConnectionState {{");
    writeln!(out, "    #[default]");
    writeln!(out, "    Disconnected,");
    writeln!(out, "    Connecting,");
    writeln!(
        out,
        "    /// Connected includes the `Identity` assigned by the server and a private access token (JWT)"
    );
    writeln!(
        out,
        "    /// which can be persisted by the application for future reconnection as the same Identity."
    );
    writeln!(out, "    Connected(Identity, String),");
    writeln!(out, "    Error,");
    writeln!(out, "}}");
    writeln!(out);

    // use_spacetimedb_context_provider
    write_context_provider(out, &tables);
    writeln!(out);

    // use_spacetimedb_context
    writeln!(out, "/// Get the SpacetimeDB context from a parent component.");
    writeln!(out, "#[must_use]");
    writeln!(out, "pub fn use_spacetimedb_context() -> SpacetimeDbContext {{");
    writeln!(out, "    use_context::<SpacetimeDbContext>()");
    writeln!(out, "}}");
    writeln!(out);

    // use_connection
    writeln!(out, "/// Get the current database connection, if connected.");
    writeln!(out, "#[must_use]");
    writeln!(
        out,
        "pub fn use_connection() -> SyncSignal<Option<SharedConnection>> {{"
    );
    writeln!(out, "    let ctx = use_spacetimedb_context();");
    writeln!(out, "    ctx.connection");
    writeln!(out, "}}");
    writeln!(out);

    // use_subscription
    write_subscription_hook(out);
    writeln!(out);

    // Table hooks
    writeln!(out, "// --- Table hooks ---");
    writeln!(out);
    for table in &tables {
        writeln!(
            out,
            "/// Get a reactive signal containing all rows of the `{}` table.",
            table.accessor_snake,
        );
        writeln!(out, "#[must_use]");
        writeln!(
            out,
            "pub fn use_table_{}() -> SyncSignal<Vec<{}>> {{",
            table.accessor_snake, table.row_type,
        );
        writeln!(out, "    let ctx = use_spacetimedb_context();");
        writeln!(out, "    ctx.tables.{}", table.accessor_snake);
        writeln!(out, "}}");
        writeln!(out);
    }

    // Reducer hooks
    writeln!(out, "// --- Reducer hooks ---");
    writeln!(out);
    for reducer in &reducers {
        write_reducer_hook(out, reducer);
        writeln!(out);
    }

    // Connection state hooks
    writeln!(out, "// --- Connection state hooks ---");
    writeln!(out);
    writeln!(out, "/// Get a reactive signal for the current connection state.");
    writeln!(out, "#[must_use]");
    writeln!(out, "pub fn use_connection_state() -> SyncSignal<ConnectionState> {{");
    writeln!(out, "    let ctx = use_spacetimedb_context();");
    writeln!(out, "    ctx.state");
    writeln!(out, "}}");
    writeln!(out);

    writeln!(
        out,
        "/// Get a reactive signal for the current connection error, if any."
    );
    writeln!(out, "#[must_use]");
    writeln!(out, "pub fn use_connection_error() -> SyncSignal<Option<String>> {{");
    writeln!(out, "    let ctx = use_spacetimedb_context();");
    writeln!(out, "    ctx.error");
    writeln!(out, "}}");

    OutputFile {
        filename: "dioxus.rs".to_string(),
        code: output.into_inner(),
    }
}

fn write_context_provider(out: &mut Indenter, tables: &[TableInfo]) {
    writeln!(
        out,
        "/// Initialize the SpacetimeDB context provider at the root of your application."
    );
    writeln!(out, "///");
    writeln!(
        out,
        "/// This hook must be called at the root component before using any other SpacetimeDB hooks."
    );
    writeln!(
        out,
        "/// It creates all table signals at root level to ensure they outlive child components."
    );
    writeln!(out, "///");
    writeln!(out, "/// # Arguments");
    writeln!(out, "/// * `uri` - The SpacetimeDB server URI");
    writeln!(out, "/// * `module_name` - The module name to connect to");
    writeln!(
        out,
        "/// * `token` - An optional OpenID Connect compliant JSON Web Token (JWT) for authentication."
    );
    writeln!(
        out,
        "///   If `None` is passed or this method is not called, SpacetimeDB will generate a new Identity"
    );
    writeln!(out, "///   and sign a new private access token for the connection.");
    writeln!(out, "#[must_use]");
    writeln!(
        out,
        "pub fn use_spacetimedb_context_provider(uri: &str, module_name: &str, token: Option<impl ToString>) -> SpacetimeDbContext {{"
    );
    writeln!(out, "    let uri = uri.to_string();");
    writeln!(out, "    let module_name = module_name.to_string();");
    writeln!(out, "    let token = token.map(|t| t.to_string());");
    writeln!(out);
    writeln!(
        out,
        "    let connection: SyncSignal<Option<SharedConnection>> = use_signal_sync(|| None);"
    );
    writeln!(
        out,
        "    let state: SyncSignal<ConnectionState> = use_signal_sync(|| ConnectionState::Disconnected);"
    );
    writeln!(
        out,
        "    let error: SyncSignal<Option<String>> = use_signal_sync(|| None);"
    );
    writeln!(out);
    writeln!(out, "    let mut table_signals = TableSignals {{");
    for table in tables {
        writeln!(out, "        {}: use_signal_sync(Vec::new),", table.accessor_snake);
    }
    writeln!(out, "    }};");
    writeln!(out);
    writeln!(out, "    let ctx = SpacetimeDbContext {{");
    writeln!(out, "        connection,");
    writeln!(out, "        state,");
    writeln!(out, "        error,");
    writeln!(out, "        tables: table_signals.clone(),");
    writeln!(out, "    }};");
    writeln!(out);
    writeln!(out, "    use_context_provider(|| ctx.clone());");
    writeln!(out);

    // use_effect for connection setup
    writeln!(out, "    use_effect(move || {{");
    writeln!(out, "        let mut connection = connection;");
    writeln!(out, "        let mut state = state;");
    writeln!(out, "        let mut error = error;");
    writeln!(out, "        let uri = uri.clone();");
    writeln!(out, "        let mut module_name = module_name.clone();");
    writeln!(out, "        let token = token.clone();");
    writeln!(out, "        let mut table_signals = table_signals.clone();");
    writeln!(out);
    writeln!(out, "        state.set(ConnectionState::Connecting);");
    writeln!(out);
    writeln!(out, "        spawn(async move {{");
    writeln!(out, "            match DbConnection::builder()");
    writeln!(out, "                .with_uri(&uri)");
    writeln!(out, "                .with_database_name(&module_name)");
    writeln!(out, "                .with_token(token)");
    writeln!(out, "                .on_connect(move |conn, identity, token| {{");

    // Generate on_connect body: populate initial rows and register callbacks for each table
    for table in tables {
        let snake = &table.accessor_snake;
        writeln!(out, "                    // Populate initial rows for {snake}");
        writeln!(
            out,
            "                    let current: Vec<{}> = conn.db.{snake}().iter().collect();",
            table.row_type,
        );
        writeln!(out, "                    table_signals.{snake}.set(current);");
        writeln!(out);
        writeln!(out, "                    // Keep signal in sync on changes");
        writeln!(
            out,
            "                    conn.db.{snake}().on_insert(move |ctx, _row| {{"
        );
        writeln!(
            out,
            "                        let updated: Vec<{}> = ctx.db.{snake}().iter().collect();",
            table.row_type,
        );
        writeln!(out, "                        table_signals.{snake}.set(updated);");
        writeln!(out, "                    }});");
        if table.has_primary_key {
            writeln!(
                out,
                "                    conn.db.{snake}().on_update(move |ctx, _old, _new| {{"
            );
            writeln!(
                out,
                "                        let updated: Vec<{}> = ctx.db.{snake}().iter().collect();",
                table.row_type,
            );
            writeln!(out, "                        table_signals.{snake}.set(updated);");
            writeln!(out, "                    }});");
        }
        writeln!(
            out,
            "                    conn.db.{snake}().on_delete(move |ctx, _row| {{"
        );
        writeln!(
            out,
            "                        let updated: Vec<{}> = ctx.db.{snake}().iter().collect();",
            table.row_type,
        );
        writeln!(out, "                        table_signals.{snake}.set(updated);");
        writeln!(out, "                    }});");
    }

    writeln!(
        out,
        "                    // Store the assigned Identity and private access token in state so the app"
    );
    writeln!(
        out,
        "                    // can persist the token (JWT) and reuse it for future reconnections via `with_token`."
    );
    writeln!(
        out,
        "                    state.set(ConnectionState::Connected(identity, token.to_string()));"
    );
    writeln!(out, "                }})");
    writeln!(
        out,
        "                .on_disconnect(move |_ctx, err: Option<spacetimedb_sdk::Error>| {{"
    );
    writeln!(out, "                    if let Some(e) = err {{");
    writeln!(out, "                        error.set(Some::<String>(e.to_string()));");
    writeln!(out, "                        state.set(ConnectionState::Error);");
    writeln!(out, "                    }} else {{");
    writeln!(out, "                        state.set(ConnectionState::Disconnected);");
    writeln!(out, "                    }}");
    writeln!(out, "                }})");
    writeln!(out, "                .build()");
    writeln!(out, "            {{");
    writeln!(out, "                Ok(conn) => {{");
    writeln!(
        out,
        "                    let shared_conn: Arc<DbConnection> = Arc::new(conn);"
    );
    writeln!(out, "                    connection.set(Some(shared_conn.clone()));");
    writeln!(out);
    writeln!(out, "                    spawn(async move {{");
    writeln!(out, "                        let _ = shared_conn.run_async().await;");
    writeln!(out, "                    }});");
    writeln!(out, "                }}");
    writeln!(out, "                Err(e) => {{");
    writeln!(out, "                    error.set(Some::<String>(e.to_string()));");
    writeln!(out, "                    state.set(ConnectionState::Error);");
    writeln!(out, "                }}");
    writeln!(out, "            }}");
    writeln!(out, "        }});");
    writeln!(out, "    }});");
    writeln!(out);
    writeln!(out, "    ctx");
    writeln!(out, "}}");
}

fn write_subscription_hook(out: &mut Indenter) {
    writeln!(out, "/// Subscribe to a set of SQL queries.");
    writeln!(out, "pub fn use_subscription(queries: &[&str]) {{");
    writeln!(
        out,
        "    let queries: Vec<String> = queries.iter().map(|s| s.to_string()).collect();"
    );
    writeln!(out, "    let conn_signal = use_connection();");
    writeln!(out);
    writeln!(out, "    use_effect(move || {{");
    writeln!(out, "        let queries = queries.clone();");
    writeln!(out, "        if let Some(conn) = conn_signal.read().as_ref() {{");
    writeln!(out, "            conn.subscription_builder()");
    writeln!(out, "                .on_applied(|_ctx| {{}})");
    writeln!(out, "                .on_error(|_ctx, _err| {{}})");
    writeln!(out, "                .subscribe(queries);");
    writeln!(out, "        }}");
    writeln!(out, "    }});");
    writeln!(out, "}}");
}

fn write_reducer_hook(out: &mut Indenter, reducer: &ReducerInfo) {
    writeln!(out, "/// Get a callback to invoke the `{}` reducer.", reducer.name_orig,);
    writeln!(out, "#[must_use]");

    if reducer.arglist.is_empty() {
        // No-argument reducer
        writeln!(
            out,
            "pub fn use_reducer_{}() -> impl Fn() + Clone + 'static {{",
            reducer.name_snake,
        );
        writeln!(out, "    let conn_signal = use_connection();");
        writeln!(out);
        writeln!(out, "    move || {{");
        writeln!(out, "        if let Some(conn) = conn_signal.read().as_ref() {{",);
        writeln!(out, "            let _ = conn.reducers.{}();", reducer.name_snake);
        writeln!(out, "        }}");
        writeln!(out, "    }}");
    } else {
        // Reducer with arguments
        writeln!(
            out,
            "pub fn use_reducer_{}() -> impl Fn({}) + Clone + 'static {{",
            reducer.name_snake, reducer.fn_trait_params,
        );
        writeln!(out, "    let conn_signal = use_connection();");
        writeln!(out);
        writeln!(out, "    move |{}| {{", reducer.closure_params);
        writeln!(out, "        if let Some(conn) = conn_signal.read().as_ref() {{",);
        writeln!(
            out,
            "            let _ = conn.reducers.{}({});",
            reducer.name_snake, reducer.arg_names,
        );
        writeln!(out, "        }}");
        writeln!(out, "    }}");
    }
    writeln!(out, "}}");
}
