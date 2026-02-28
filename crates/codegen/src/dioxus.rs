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
    is_reducer_invokable, iter_reducers, iter_table_names_and_types, iter_tables, print_auto_generated_file_comment,
    print_auto_generated_version_comment, type_ref_name, CodegenVisibility,
};
use crate::{CodegenOptions, Lang, OutputFile};
use convert_case::{Case, Casing};
use spacetimedb_schema::def::{ModuleDef, ProcedureDef, ReducerDef, TableDef, TypeDef};
use spacetimedb_schema::schema::TableSchema;
use std::ops::Deref;

const INDENT: &str = "    ";

pub struct Dioxus;

impl Lang for Dioxus {
    fn generate_type_files(&self, module: &ModuleDef, typ: &TypeDef) -> Vec<OutputFile> {
        crate::rust::Rust.generate_type_files(module, typ)
    }

    fn generate_table_file_from_schema(&self, module: &ModuleDef, table: &TableDef, schema: TableSchema) -> OutputFile {
        crate::rust::Rust.generate_table_file_from_schema(module, table, schema)
    }

    fn generate_reducer_file(&self, module: &ModuleDef, reducer: &ReducerDef) -> OutputFile {
        crate::rust::Rust.generate_reducer_file(module, reducer)
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

fn collect_reducers(module: &ModuleDef, visibility: CodegenVisibility) -> Vec<ReducerInfo> {
    iter_reducers(module, visibility)
        .filter(|r| is_reducer_invokable(r))
        .map(|reducer| {
            let name_snake = reducer.name.deref().to_case(Case::Snake);
            let name_orig = reducer.name.deref().to_string();

            let mut arglist = String::new();
            let mut arg_names = String::new();
            let mut closure_params = String::new();
            let mut fn_trait_params = String::new();

            for (i, (arg_ident, arg_ty)) in reducer.params_for_generate.elements.iter().enumerate() {
                let arg_name = arg_ident.deref().to_case(Case::Snake);
                let arg_type = type_name(module, arg_ty);

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
    writeln!(out, "use spacetimedb_sdk::{{DbContext, Table, TableWithPrimaryKey}};");
    writeln!(out, "use std::sync::Arc;");
    writeln!(out);

    // SharedConnection type alias
    writeln!(out, "/// Thread-safe wrapper for the database connection.");
    writeln!(out, "pub type SharedConnection = Arc<DbConnection>;");
    writeln!(out);

    // TableSignals struct
    writeln!(out, "/// Container for all table signals, created at root level.");
    writeln!(out, "#[derive(Clone, Copy)]");
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
    writeln!(out, "#[derive(Clone, Copy)]");
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
    writeln!(out, "#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]");
    writeln!(out, "pub enum ConnectionState {{");
    writeln!(out, "    #[default]");
    writeln!(out, "    Disconnected,");
    writeln!(out, "    Connecting,");
    writeln!(out, "    Connected,");
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
    writeln!(out, "#[must_use]");
    writeln!(
        out,
        "pub fn use_spacetimedb_context_provider(uri: &str, module_name: &str) -> SpacetimeDbContext {{"
    );
    writeln!(out, "    let uri = uri.to_string();");
    writeln!(out, "    let module_name = module_name.to_string();");
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
    writeln!(out, "        tables: table_signals,");
    writeln!(out, "    }};");
    writeln!(out);
    writeln!(out, "    use_context_provider(|| ctx);");
    writeln!(out);

    // use_effect for connection setup
    writeln!(out, "    use_effect(move || {{");
    writeln!(out, "        let mut connection = connection;");
    writeln!(out, "        let mut state = state;");
    writeln!(out, "        let mut error = error;");
    writeln!(out, "        let uri = uri.clone();");
    writeln!(out, "        let module_name = module_name.clone();");
    writeln!(out);
    writeln!(out, "        state.set(ConnectionState::Connecting);");
    writeln!(out);
    writeln!(out, "        spawn(async move {{");
    writeln!(out, "            match DbConnection::builder()");
    writeln!(out, "                .with_uri(&uri)");
    writeln!(out, "                .with_database_name(&module_name)");
    writeln!(out, "                .on_connect(move |conn, _identity, _token| {{");

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
    writeln!(out, "                    state.set(ConnectionState::Connected);");
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
