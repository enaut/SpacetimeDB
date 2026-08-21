//! Code generation for Dioxus v0.7 signals and hooks with SpacetimeDB v2.0.
//!
//! Generates a `dioxus.rs` module that wraps the standard Rust SDK types with Dioxus
//! reactive signals and hooks. This provides:
//!
//! - `use_spacetimedb_context_provider` — Root-level hook that establishes the connection
//!   and creates `SyncSignal`s for all tables.
//! - `use_table_*` — Per-table hooks returning `SyncSignal<Vec<Row>>`.
//! - `use_reducer_*` — Per-reducer hooks returning callable closures.
//! - `use_procedure_*` — Per-procedure hooks returning `(invoke, SyncSignal<Option<Result<T, String>>>)`.
//! - `use_subscription` — Hook for subscribing to SQL queries.
//! - Connection state hooks for monitoring connection status.

use super::code_indenter::{CodeIndenter, Indenter};
use crate::rust::type_name;
use crate::util::{
    collect_case, is_reducer_invokable, iter_procedures, iter_reducers, iter_table_names_and_types, iter_tables,
    iter_views, print_auto_generated_file_comment, print_auto_generated_version_comment, type_ref_name,
    CodegenVisibility,
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
    if arg_names.is_empty() {
        writeln!(out, "        self.{func_name}_then(|_, _| {{}})");
    } else {
        writeln!(out, "        self.{func_name}_then({arg_names}, |_, _| {{}})");
    }
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
    if !arglist.is_empty() {
        writeln!(out, "        {arglist},");
    }
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
    if !arglist.is_empty() {
        writeln!(out, "        {arglist},");
    }
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
    /// Whether the table/view has a primary key (enables on_update callback)
    has_primary_key: bool,
}

fn collect_tables(module: &ModuleDef, visibility: CodegenVisibility) -> Vec<TableInfo> {
    // Real tables with a primary key column.
    let tables_with_pk: std::collections::HashSet<String> = iter_tables(module, visibility)
        .filter(|t| t.primary_key.is_some())
        .map(|t| t.accessor_name.deref().to_string())
        .collect();
    // Views also carry their own `primary_key`, set by the schema validator when a
    // query-builder view's underlying table has a primary key. Client-side table
    // handles for such views implement `TableWithPrimaryKey`/`on_update` just like
    // real tables do, so they must be considered here too.
    let views_with_pk: std::collections::HashSet<String> = iter_views(module)
        .filter(|v| v.primary_key.is_some())
        .map(|v| v.accessor_name.deref().to_string())
        .collect();

    iter_table_names_and_types(module, visibility)
        .map(|(_, accessor_name, product_type_ref)| {
            let accessor_snake = accessor_name.deref().to_case(Case::Snake);
            let row_type = type_ref_name(module, product_type_ref);
            let has_primary_key =
                tables_with_pk.contains(accessor_name.deref()) || views_with_pk.contains(accessor_name.deref());
            TableInfo {
                accessor_snake,
                row_type,
                has_primary_key,
            }
        })
        .collect()
}

/// Collect procedure metadata for code generation.
struct ProcedureInfo {
    /// snake_case accessor name (e.g. "provision_message_category")
    name_snake: String,
    /// Original procedure name as declared in the module
    name_orig: String,
    /// Parameter names formatted as "name, name"
    arg_names: String,
    /// Closure parameter list formatted as "name: Type, name: Type"
    closure_params: String,
    /// Fn trait parameter list (types only) formatted as "Type, Type"
    fn_trait_params: String,
    /// Return type as a Rust type expression string
    return_type: String,
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

fn collect_procedures(module: &ModuleDef, visibility: CodegenVisibility) -> Vec<ProcedureInfo> {
    iter_procedures(module, visibility)
        .map(|proc| {
            let name_snake = proc.accessor_name.deref().to_case(Case::Snake);
            let name_orig = proc.name.deref().to_string();

            // The wrapper struct name used in the generated procedure file.
            // We pass it to type_name_for_hook to handle any potential name conflicts.
            let wrapper_struct_name = proc.accessor_name.deref().to_case(Case::Pascal) + "Args";

            let mut arg_names = String::new();
            let mut closure_params = String::new();
            let mut fn_trait_params = String::new();

            for (i, (arg_ident, arg_ty)) in proc.params_for_generate.elements.iter().enumerate() {
                let arg_name = arg_ident.deref().to_case(Case::Snake);
                let arg_type = type_name_for_hook(module, arg_ty, &wrapper_struct_name);

                if i > 0 {
                    arg_names.push_str(", ");
                    closure_params.push_str(", ");
                    fn_trait_params.push_str(", ");
                }
                arg_names.push_str(&arg_name);
                closure_params.push_str(&format!("{arg_name}: {arg_type}"));
                fn_trait_params.push_str(&arg_type.to_string());
            }

            let mut return_type = String::new();
            write_type_to_string(
                &mut return_type,
                module,
                &proc.return_type_for_generate,
                &HashMap::new(),
            );

            ProcedureInfo {
                name_snake,
                name_orig,
                arg_names,
                closure_params,
                fn_trait_params,
                return_type,
            }
        })
        .collect()
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
    let procedures = collect_procedures(module, visibility);

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
    writeln!(out, "use std::sync::{{Arc, Mutex}};");
    writeln!(out, "use futures_channel::oneshot;");
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
    writeln!(out, "    Reconnecting {{ attempt: u32, delay_ms: u64 }},");
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

    write_reconnect_helpers(out);
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

    // Procedure hooks
    if !procedures.is_empty() {
        writeln!(out, "// --- Procedure hooks ---");
        writeln!(out);
        for proc in &procedures {
            write_procedure_hook(out, proc);
            writeln!(out);
        }
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

fn write_reconnect_helpers(out: &mut Indenter) {
    writeln!(out, "const RECONNECT_BASE_DELAY_MS: u64 = 400;");
    writeln!(out, "const RECONNECT_MAX_DELAY_MS: u64 = 10_000;");
    writeln!(out, "const RECONNECT_JITTER_MS: u64 = 300;");
    writeln!(out, "const MAX_RECONNECT_ATTEMPTS: u32 = 0;");
    writeln!(out);
    writeln!(out, "#[must_use]");
    writeln!(out, "fn should_retry_reconnect(attempt: u32) -> bool {{");
    writeln!(
        out,
        "    MAX_RECONNECT_ATTEMPTS == 0 || attempt <= MAX_RECONNECT_ATTEMPTS"
    );
    writeln!(out, "}}");
    writeln!(out);
    writeln!(out, "#[must_use]");
    writeln!(out, "fn reconnect_delay_ms(attempt: u32) -> u64 {{");
    writeln!(out, "    let shift = attempt.min(8);");
    writeln!(out, "    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);");
    writeln!(
        out,
        "    let base_ms = RECONNECT_BASE_DELAY_MS.saturating_mul(factor).min(RECONNECT_MAX_DELAY_MS);"
    );
    writeln!(
        out,
        "    let jitter = (u64::from(attempt).saturating_mul(137)) % RECONNECT_JITTER_MS.max(1);"
    );
    writeln!(out, "    base_ms.saturating_add(jitter)");
    writeln!(out, "}}");
    writeln!(out);
    writeln!(out, "#[cfg(not(target_arch = \"wasm32\"))]");
    writeln!(out, "struct ThreadSleep {{");
    writeln!(out, "    done: Arc<std::sync::atomic::AtomicBool>,");
    writeln!(out, "    started: bool,");
    writeln!(out, "    delay_ms: u64,");
    writeln!(out, "}}");
    writeln!(out);
    writeln!(out, "#[cfg(not(target_arch = \"wasm32\"))]");
    writeln!(out, "impl ThreadSleep {{");
    writeln!(out, "    fn new(delay_ms: u64) -> Self {{");
    writeln!(out, "        Self {{");
    writeln!(
        out,
        "            done: Arc::new(std::sync::atomic::AtomicBool::new(false)),"
    );
    writeln!(out, "            started: false,");
    writeln!(out, "            delay_ms,");
    writeln!(out, "        }}");
    writeln!(out, "    }}");
    writeln!(out, "}}");
    writeln!(out);
    writeln!(out, "#[cfg(not(target_arch = \"wasm32\"))]");
    writeln!(out, "impl std::future::Future for ThreadSleep {{");
    writeln!(out, "    type Output = ();");
    writeln!(out);
    writeln!(
        out,
        "    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {{"
    );
    writeln!(
        out,
        "        if self.done.load(std::sync::atomic::Ordering::Acquire) {{"
    );
    writeln!(out, "            return std::task::Poll::Ready(());");
    writeln!(out, "        }}");
    writeln!(out);
    writeln!(out, "        if !self.started {{");
    writeln!(out, "            self.started = true;");
    writeln!(out, "            let done = Arc::clone(&self.done);");
    writeln!(out, "            let delay_ms = self.delay_ms;");
    writeln!(out, "            let waker = cx.waker().clone();");
    writeln!(out, "            std::thread::spawn(move || {{");
    writeln!(
        out,
        "                std::thread::sleep(std::time::Duration::from_millis(delay_ms));"
    );
    writeln!(
        out,
        "                done.store(true, std::sync::atomic::Ordering::Release);"
    );
    writeln!(out, "                waker.wake();");
    writeln!(out, "            }});");
    writeln!(out, "        }}");
    writeln!(out);
    writeln!(out, "        std::task::Poll::Pending");
    writeln!(out, "    }}");
    writeln!(out, "}}");
    writeln!(out);
    writeln!(out, "async fn reconnect_sleep(delay_ms: u64) {{");
    writeln!(out, "    #[cfg(not(target_arch = \"wasm32\"))]");
    writeln!(out, "    {{");
    writeln!(out, "        ThreadSleep::new(delay_ms).await;");
    writeln!(out, "    }}");
    writeln!(out, "    #[cfg(target_arch = \"wasm32\")]");
    writeln!(out, "    {{");
    writeln!(out, "        let _ = delay_ms;");
    writeln!(out, "    }}");
    writeln!(out, "}}");
    writeln!(out);
    writeln!(out, "#[must_use]");
    writeln!(
        out,
        "fn is_fatal_connection_error(err: &spacetimedb_sdk::Error) -> bool {{"
    );
    writeln!(out, "    let msg = err.to_string().to_ascii_lowercase();");
    writeln!(out, "    msg.contains(\"unauthorized\")");
    writeln!(out, "        || msg.contains(\"forbidden\")");
    writeln!(out, "        || msg.contains(\"invalid credentials\")");
    writeln!(out, "        || msg.contains(\"invalid token\")");
    writeln!(out, "        || msg.contains(\"token expired\")");
    writeln!(out, "}}");
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
    writeln!(
        out,
        "    let active_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(token.clone()));"
    );
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
    writeln!(out, "        let module_name = module_name.clone();");
    writeln!(out, "        let active_token = active_token.clone();");
    writeln!(out, "        let table_signals = table_signals.clone();");
    writeln!(out);
    writeln!(out, "        spawn(async move {{");
    writeln!(out, "            let mut reconnect_attempt: u32 = 0;");
    writeln!(out);
    writeln!(out, "            loop {{");
    writeln!(out, "                if reconnect_attempt == 0 {{");
    writeln!(out, "                    state.set(ConnectionState::Connecting);");
    writeln!(out, "                }} else {{");
    writeln!(
        out,
        "                    let delay_ms = reconnect_delay_ms(reconnect_attempt);"
    );
    writeln!(
        out,
        "                    state.set(ConnectionState::Reconnecting {{ attempt: reconnect_attempt, delay_ms }});"
    );
    writeln!(out, "                }}");
    writeln!(out);
    writeln!(out, "                let token_for_build = active_token");
    writeln!(out, "                    .lock()");
    writeln!(out, "                    .ok()");
    writeln!(out, "                    .and_then(|token| token.clone());");
    writeln!(out);
    writeln!(
        out,
        "                let disconnect_fatal: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));"
    );
    writeln!(
        out,
        "                let disconnect_fatal_on_disconnect = disconnect_fatal.clone();"
    );
    writeln!(out);
    writeln!(
        out,
        "                let mut connection_on_disconnect = connection.clone();"
    );
    writeln!(out, "                let mut state_on_disconnect = state.clone();");
    writeln!(out, "                let mut error_on_disconnect = error.clone();");
    writeln!(out, "                let mut state_on_connect = state.clone();");
    writeln!(out, "                let mut error_on_connect = error.clone();");
    writeln!(
        out,
        "                let mut table_signals_on_connect = table_signals.clone();"
    );
    writeln!(
        out,
        "                let active_token_on_connect = active_token.clone();"
    );
    writeln!(out);
    writeln!(out, "                let conn = match DbConnection::builder()");
    writeln!(out, "                    .with_uri(&uri)");
    writeln!(out, "                    .with_database_name(&module_name)");
    writeln!(out, "                    .with_token(token_for_build)");
    writeln!(out, "                    .on_connect(move |conn, identity, token| {{");

    // Generate on_connect body: populate initial rows and register callbacks for each table
    for table in tables {
        let snake = &table.accessor_snake;
        writeln!(out, "                    // Populate initial rows for {snake}");
        writeln!(
            out,
            "                    let current: Vec<{}> = conn.db.{snake}().iter().collect();",
            table.row_type,
        );
        writeln!(
            out,
            "                    table_signals_on_connect.{snake}.set(current);"
        );
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
        writeln!(
            out,
            "                        table_signals_on_connect.{snake}.set(updated);"
        );
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
            writeln!(
                out,
                "                        table_signals_on_connect.{snake}.set(updated);"
            );
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
        writeln!(
            out,
            "                        table_signals_on_connect.{snake}.set(updated);"
        );
        writeln!(out, "                    }});");
    }

    writeln!(
        out,
        "                        if let Ok(mut token_store) = active_token_on_connect.lock() {{"
    );
    writeln!(
        out,
        "                            *token_store = Some(token.to_string());"
    );
    writeln!(out, "                        }}");
    writeln!(out);
    writeln!(
        out,
        "                        // Store the assigned Identity and private access token in state so the app"
    );
    writeln!(
        out,
        "                        // can persist the token (JWT) and reuse it for future reconnections via `with_token`."
    );
    writeln!(out, "                        error_on_connect.set(None);");
    writeln!(
        out,
        "                        state_on_connect.set(ConnectionState::Connected(identity, token.to_string()));"
    );
    writeln!(out, "                    }})");
    writeln!(
        out,
        "                    .on_disconnect(move |_ctx, err: Option<spacetimedb_sdk::Error>| {{"
    );
    writeln!(out, "                        connection_on_disconnect.set(None);");
    writeln!(out, "                        if let Some(e) = err {{");
    writeln!(
        out,
        "                            error_on_disconnect.set(Some::<String>(e.to_string()));"
    );
    writeln!(out, "                            if is_fatal_connection_error(&e) {{");
    writeln!(
        out,
        "                                if let Ok(mut fatal) = disconnect_fatal_on_disconnect.lock() {{"
    );
    writeln!(out, "                                    *fatal = true;");
    writeln!(out, "                                }}");
    writeln!(out, "                            }}");
    writeln!(out, "                        }}");
    writeln!(
        out,
        "                        state_on_disconnect.set(ConnectionState::Disconnected);"
    );
    writeln!(out, "                    }})");
    writeln!(out, "                    .build().await {{");
    writeln!(out, "                    Ok(conn) => conn,");
    writeln!(out, "                    Err(e) => {{");
    writeln!(out, "                        connection.set(None);");
    writeln!(out, "                        error.set(Some::<String>(e.to_string()));");
    writeln!(out, "                        if is_fatal_connection_error(&e) {{");
    writeln!(out, "                            state.set(ConnectionState::Error);");
    writeln!(out, "                            break;");
    writeln!(out, "                        }}");
    writeln!(
        out,
        "                        reconnect_attempt = reconnect_attempt.saturating_add(1);"
    );
    writeln!(
        out,
        "                        if !should_retry_reconnect(reconnect_attempt) {{"
    );
    writeln!(out, "                            state.set(ConnectionState::Error);");
    writeln!(out, "                            break;");
    writeln!(out, "                        }}");
    writeln!(
        out,
        "                        let delay_ms = reconnect_delay_ms(reconnect_attempt);"
    );
    writeln!(
        out,
        "                        state.set(ConnectionState::Reconnecting {{ attempt: reconnect_attempt, delay_ms }});"
    );
    writeln!(out, "                        reconnect_sleep(delay_ms).await;");
    writeln!(out, "                        continue;");
    writeln!(out, "                    }}");
    writeln!(out, "                }};");
    writeln!(out);
    writeln!(
        out,
        "                let shared_conn: Arc<DbConnection> = Arc::new(conn);"
    );
    writeln!(out, "                connection.set(Some(shared_conn.clone()));");
    writeln!(out, "                reconnect_attempt = 0;");
    writeln!(out);
    writeln!(out, "                let run_result = shared_conn.run_async().await;");
    writeln!(out, "                connection.set(None);");
    writeln!(out);
    writeln!(
        out,
        "                let disconnected_with_fatal_error = disconnect_fatal"
    );
    writeln!(out, "                    .lock()");
    writeln!(out, "                    .ok()");
    writeln!(out, "                    .map(|fatal| *fatal)");
    writeln!(out, "                    .unwrap_or(false);");
    writeln!(out, "                if disconnected_with_fatal_error {{");
    writeln!(out, "                    state.set(ConnectionState::Error);");
    writeln!(out, "                    break;");
    writeln!(out, "                }}");
    writeln!(out);
    writeln!(out, "                if let Err(e) = run_result {{");
    writeln!(out, "                    error.set(Some::<String>(e.to_string()));");
    writeln!(out, "                    if is_fatal_connection_error(&e) {{");
    writeln!(out, "                        state.set(ConnectionState::Error);");
    writeln!(out, "                        break;");
    writeln!(out, "                    }}");
    writeln!(out, "                }}");
    writeln!(out);
    writeln!(
        out,
        "                reconnect_attempt = reconnect_attempt.saturating_add(1);"
    );
    writeln!(out, "                if !should_retry_reconnect(reconnect_attempt) {{");
    writeln!(out, "                    state.set(ConnectionState::Error);");
    writeln!(out, "                    break;");
    writeln!(out, "                }}");
    writeln!(out);
    writeln!(
        out,
        "                let delay_ms = reconnect_delay_ms(reconnect_attempt);"
    );
    writeln!(
        out,
        "                state.set(ConnectionState::Reconnecting {{ attempt: reconnect_attempt, delay_ms }});"
    );
    writeln!(out, "                reconnect_sleep(delay_ms).await;");
    writeln!(out, "            }}");
    writeln!(out, "        }});");
    writeln!(out, "    }});");
    writeln!(out);
    writeln!(out, "    ctx");
    writeln!(out, "}}");
}

fn write_subscription_hook(out: &mut Indenter) {
    writeln!(out, "/// Subscribe to a set of SQL queries.");
    writeln!(out, "///");
    writeln!(
        out,
        "/// Re-subscribes automatically whenever the connection instance changes"
    );
    writeln!(out, "/// (initial connect, or reconnect after a network interruption).");
    writeln!(
        out,
        "/// Avoids duplicate subscriptions while the connection is stable."
    );
    writeln!(out, "/// Subscription errors are printed to stderr for diagnosis.");
    writeln!(out, "pub fn use_subscription(queries: &[&str]) {{");
    writeln!(
        out,
        "    let queries: Vec<String> = queries.iter().map(|s| s.to_string()).collect();"
    );
    writeln!(out, "    let conn_signal = use_connection();");
    writeln!(out);
    writeln!(
        out,
        "    // Stores the Arc pointer of the last successfully-subscribed connection."
    );
    writeln!(
        out,
        "    // Using peek() inside the effect keeps this read non-reactive (no infinite loop)."
    );
    writeln!(
        out,
        "    let last_conn: SyncSignal<Option<SharedConnection>> = use_signal_sync(|| None);"
    );
    writeln!(out);
    writeln!(out, "    use_effect(move || {{");
    writeln!(
        out,
        "        // transition (None->Some on connect, Some->None on disconnect, Some(a)->Some(b) on reconnect)."
    );
    writeln!(out, "        let current = conn_signal();");
    writeln!(out, "        let mut last = last_conn;");
    writeln!(out);
    writeln!(out, "        match current.as_ref() {{");
    writeln!(out, "            None => {{");
    writeln!(
        out,
        "                // Connection lost: clear the tracker so the next connection"
    );
    writeln!(out, "                // instance will trigger a fresh subscribe.");
    writeln!(
        out,
        "                // peek() avoids creating a reactive dependency that would loop."
    );
    writeln!(out, "                if last.peek().is_some() {{");
    writeln!(out, "                    last.set(None);");
    writeln!(out, "                }}");
    writeln!(out, "            }}");
    writeln!(out, "            Some(conn) => {{");
    writeln!(
        out,
        "                // peek() reads last without subscribing to it reactively."
    );
    writeln!(
        out,
        "                if last.peek().as_ref().map(|prev| Arc::ptr_eq(prev, conn)).unwrap_or(false) {{"
    );
    writeln!(
        out,
        "                    // Same connection instance – already subscribed, nothing to do."
    );
    writeln!(out, "                    return;");
    writeln!(out, "                }}");
    writeln!(
        out,
        "                // New or reconnected instance – store it and subscribe."
    );
    writeln!(out, "                last.set(Some(conn.clone()));");
    writeln!(out, "                let queries = queries.clone();");
    writeln!(out, "                conn.subscription_builder()");
    writeln!(out, "                    .on_applied(|_ctx| {{}})");
    writeln!(out, "                    .on_error(|_ctx, err| {{");
    writeln!(
        out,
        "                        eprintln!(\"[spacetimedb] subscription error: {{err}}\");"
    );
    writeln!(out, "                    }})");
    writeln!(out, "                    .subscribe(queries);");
    writeln!(out, "            }}");
    writeln!(out, "        }}");
    writeln!(out, "    }});");
    writeln!(out, "}}");
}

fn write_procedure_hook(out: &mut Indenter, proc: &ProcedureInfo) {
    let name_orig = &proc.name_orig;
    let name_snake = &proc.name_snake;
    let ret = &proc.return_type;
    let fn_trait_params = &proc.fn_trait_params;

    writeln!(
        out,
        "/// Invoke the `{name_orig}` procedure and get a reactive signal for its result.",
    );
    writeln!(out, "///");
    writeln!(
        out,
        "/// Returns `(invoke, result)`. Calling `invoke(...)` sends the procedure call to the server.",
    );
    writeln!(
        out,
        "/// The `result` signal is updated to `Some(Ok(value))` on success or `Some(Err(message))`",
    );
    writeln!(out, "/// on failure once the server responds.");
    writeln!(out, "#[must_use]");

    let fn_type = if fn_trait_params.is_empty() {
        "impl Fn() + Clone + 'static".to_string()
    } else {
        format!("impl Fn({fn_trait_params}) + Clone + 'static")
    };
    writeln!(
        out,
        "pub fn use_procedure_{name_snake}() -> ({fn_type}, SyncSignal<Option<Result<{ret}, String>>>) {{",
    );
    writeln!(out, "    let conn_signal = use_connection();");
    writeln!(
        out,
        "    let mut result: SyncSignal<Option<Result<{ret}, String>>> = use_signal_sync(|| None);",
    );
    writeln!(out);
    writeln!(out, "    let invoke = move |{}| {{", proc.closure_params);
    writeln!(out, "        let mut result = result;");
    writeln!(out, "        result.set(None);");
    writeln!(out, "        if let Some(conn) = conn_signal().as_ref() {{");
    writeln!(out, "            let (tx, rx) = oneshot::channel();");
    if proc.arg_names.is_empty() {
        writeln!(out, "            conn.procedures.{name_snake}_then(move |_ctx, res| {{");
    } else {
        writeln!(
            out,
            "            conn.procedures.{name_snake}_then({}, move |_ctx, res| {{",
            proc.arg_names,
        );
    }
    writeln!(out, "                let _ = tx.send(res);");
    writeln!(out, "            }});");
    writeln!(out, "            spawn(async move {{");
    writeln!(out, "                if let Ok(res) = rx.await {{");
    writeln!(out, "                    result.set(Some(res.map_err(|e| e.to_string())));");
    writeln!(out, "                }}");
    writeln!(out, "            }});");
    writeln!(out, "        }} else {{");
    writeln!(out, "            result.set(Some(Err(\"Disconnected from SpacetimeDB\".to_string())));");
    writeln!(out, "        }}");
    writeln!(out, "    }};");
    writeln!(out);
    writeln!(out, "    (invoke, result)");
    writeln!(out, "}}");
    writeln!(out);

    // use_procedure_{name}_async
    writeln!(
        out,
        "/// Invoke the `{name_orig}` procedure asynchronously.",
    );
    writeln!(out, "///");
    writeln!(
        out,
        "/// Returns a closure that can be called to invoke the procedure and `await` the response directly.",
    );
    writeln!(out, "#[must_use]");
    let async_fn_type = if fn_trait_params.is_empty() {
        format!("impl Fn() -> impl std::future::Future<Output = Result<{ret}, String>> + Clone + 'static")
    } else {
        format!("impl Fn({fn_trait_params}) -> impl std::future::Future<Output = Result<{ret}, String>> + Clone + 'static")
    };
    writeln!(
        out,
        "pub fn use_procedure_{name_snake}_async() -> {async_fn_type} {{",
    );
    writeln!(out, "    let conn_signal = use_connection();");
    writeln!(out);
    writeln!(out, "    move |{}| {{", proc.closure_params);
    writeln!(out, "        let conn = conn_signal();");
    writeln!(out, "        async move {{");
    writeln!(out, "            let Some(conn) = conn.as_ref() else {{");
    writeln!(out, "                return Err(\"Disconnected from SpacetimeDB\".to_string());");
    writeln!(out, "            }};");
    writeln!(out, "            let (tx, rx) = oneshot::channel();");
    if proc.arg_names.is_empty() {
        writeln!(out, "            conn.procedures.{name_snake}_then(move |_ctx, res| {{");
    } else {
        writeln!(
            out,
            "            conn.procedures.{name_snake}_then({}, move |_ctx, res| {{",
            proc.arg_names,
        );
    }
    writeln!(out, "                let _ = tx.send(res);");
    writeln!(out, "            }});");
    writeln!(out, "            match rx.await {{");
    writeln!(out, "                Ok(res) => res.map_err(|e| e.to_string()),");
    writeln!(out, "                Err(_) => Err(\"Request cancelled\".to_string()),");
    writeln!(out, "            }}");
    writeln!(out, "        }}");
    writeln!(out, "    }}");
    writeln!(out, "}}");
}

fn write_reducer_hook(out: &mut Indenter, reducer: &ReducerInfo) {
    let name_orig = &reducer.name_orig;
    let name_snake = &reducer.name_snake;
    let fn_trait_params = &reducer.fn_trait_params;
    let closure_params = &reducer.closure_params;
    let arg_names = &reducer.arg_names;

    writeln!(out, "/// Get a callback to invoke the `{name_orig}` reducer.");
    writeln!(out, "#[must_use]");

    if reducer.arglist.is_empty() {
        // No-argument reducer
        writeln!(
            out,
            "pub fn use_reducer_{name_snake}() -> impl Fn() -> spacetimedb_sdk::Result<()> + Clone + 'static {{",
        );
        writeln!(out, "    let conn_signal = use_connection();");
        writeln!(out);
        writeln!(out, "    move || {{");
        writeln!(out, "        if let Some(conn) = conn_signal().as_ref() {{",);
        writeln!(out, "            conn.reducers.{name_snake}()");
        writeln!(out, "        }} else {{");
        writeln!(out, "            Err(spacetimedb_sdk::Error::Disconnected)");
        writeln!(out, "        }}");
        writeln!(out, "    }}");
        writeln!(out, "}}");
    } else {
        // Reducer with arguments
        writeln!(
            out,
            "pub fn use_reducer_{name_snake}() -> impl Fn({fn_trait_params}) -> spacetimedb_sdk::Result<()> + Clone + 'static {{",
        );
        writeln!(out, "    let conn_signal = use_connection();");
        writeln!(out);
        writeln!(out, "    move |{closure_params}| {{");
        writeln!(out, "        if let Some(conn) = conn_signal().as_ref() {{",);
        writeln!(
            out,
            "            conn.reducers.{name_snake}({arg_names})",
        );
        writeln!(out, "        }} else {{");
        writeln!(out, "            Err(spacetimedb_sdk::Error::Disconnected)");
        writeln!(out, "        }}");
        writeln!(out, "    }}");
        writeln!(out, "}}");
    }

    writeln!(out);

    // use_reducer_{name}_then
    writeln!(
        out,
        "/// Invoke the `{name_orig}` reducer and get a reactive signal for its completion status.",
    );
    writeln!(out, "///");
    writeln!(
        out,
        "/// Returns `(invoke, result)`. Calling `invoke(...)` sends the reducer invocation to the server.",
    );
    writeln!(
        out,
        "/// The `result` signal is updated to `Some(Ok(()))` on success or `Some(Err(message))`",
    );
    writeln!(out, "/// on failure once the server notifies completion.");
    writeln!(out, "#[must_use]");

    let fn_type = if fn_trait_params.is_empty() {
        "impl Fn() + Clone + 'static".to_string()
    } else {
        format!("impl Fn({fn_trait_params}) + Clone + 'static")
    };
    writeln!(
        out,
        "pub fn use_reducer_{name_snake}_then() -> ({fn_type}, SyncSignal<Option<Result<(), String>>>) {{",
    );
    writeln!(out, "    let conn_signal = use_connection();");
    writeln!(
        out,
        "    let mut result: SyncSignal<Option<Result<(), String>>> = use_signal_sync(|| None);",
    );
    writeln!(out);
    writeln!(out, "    let invoke = move |{closure_params}| {{");
    writeln!(out, "        let mut result = result;");
    writeln!(out, "        result.set(None);");
    writeln!(out, "        if let Some(conn) = conn_signal().as_ref() {{");
    writeln!(out, "            let (tx, rx) = oneshot::channel();");
    if arg_names.is_empty() {
        writeln!(
            out,
            "            if let Err(e) = conn.reducers.{name_snake}_then(move |_ctx, res| {{"
        );
    } else {
        writeln!(
            out,
            "            if let Err(e) = conn.reducers.{name_snake}_then({arg_names}, move |_ctx, res| {{"
        );
    }
    writeln!(out, "                let _ = tx.send(res);");
    writeln!(out, "            }}) {{");
    writeln!(out, "                result.set(Some(Err(e.to_string())));");
    writeln!(out, "                return;");
    writeln!(out, "            }}");
    writeln!(out, "            spawn(async move {{");
    writeln!(out, "                if let Ok(res) = rx.await {{");
    writeln!(out, "                    let flattened = match res {{");
    writeln!(out, "                        Ok(Ok(())) => Ok(()),");
    writeln!(out, "                        Ok(Err(module_err)) => Err(module_err),");
    writeln!(out, "                        Err(sdk_err) => Err(sdk_err.to_string()),");
    writeln!(out, "                    }};");
    writeln!(out, "                    result.set(Some(flattened));");
    writeln!(out, "                }}");
    writeln!(out, "            }});");
    writeln!(out, "        }} else {{");
    writeln!(
        out,
        "            result.set(Some(Err(\"Disconnected from SpacetimeDB\".to_string())));"
    );
    writeln!(out, "        }}");
    writeln!(out, "    }};");
    writeln!(out);
    writeln!(out, "    (invoke, result)");
    writeln!(out, "}}");
    writeln!(out);

    // use_reducer_{name}_async
    writeln!(
        out,
        "/// Invoke the `{name_orig}` reducer asynchronously and await its completion.",
    );
    writeln!(out, "///");
    writeln!(
        out,
        "/// Returns a closure that can be called to invoke the reducer and `await` its completion directly.",
    );
    writeln!(out, "#[must_use]");
    let async_fn_type = if fn_trait_params.is_empty() {
        "impl Fn() -> impl std::future::Future<Output = Result<(), String>> + Clone + 'static".to_string()
    } else {
        format!("impl Fn({fn_trait_params}) -> impl std::future::Future<Output = Result<(), String>> + Clone + 'static")
    };
    writeln!(
        out,
        "pub fn use_reducer_{name_snake}_async() -> {async_fn_type} {{",
    );
    writeln!(out, "    let conn_signal = use_connection();");
    writeln!(out);
    writeln!(out, "    move |{closure_params}| {{");
    writeln!(out, "        let conn = conn_signal();");
    writeln!(out, "        async move {{");
    writeln!(out, "            let Some(conn) = conn.as_ref() else {{");
    writeln!(
        out,
        "                return Err(\"Disconnected from SpacetimeDB\".to_string());"
    );
    writeln!(out, "            }};");
    writeln!(out, "            let (tx, rx) = oneshot::channel();");
    if arg_names.is_empty() {
        writeln!(
            out,
            "            if let Err(e) = conn.reducers.{name_snake}_then(move |_ctx, res| {{"
        );
    } else {
        writeln!(
            out,
            "            if let Err(e) = conn.reducers.{name_snake}_then({arg_names}, move |_ctx, res| {{"
        );
    }
    writeln!(out, "                let _ = tx.send(res);");
    writeln!(out, "            }}) {{");
    writeln!(out, "                return Err(e.to_string());");
    writeln!(out, "            }}");
    writeln!(out, "            match rx.await {{");
    writeln!(out, "                Ok(Ok(Ok(()))) => Ok(()),");
    writeln!(out, "                Ok(Ok(Err(err))) => Err(err),");
    writeln!(out, "                Ok(Err(sdk_err)) => Err(sdk_err.to_string()),");
    writeln!(out, "                Err(_) => Err(\"Request cancelled\".to_string()),");
    writeln!(out, "            }}");
    writeln!(out, "        }}");
    writeln!(out, "    }}");
    writeln!(out, "}}");
}
