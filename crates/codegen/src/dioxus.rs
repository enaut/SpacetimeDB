//! Code generation for Dioxus signals and hooks.
//!
//! This module generates Dioxus-compatible Rust code that provides reactive signals and hooks
//! for interacting with SpacetimeDB tables and reducers.
//!
//! The generated code provides:
//! - `use_table_*()` hooks that return reactive signals containing table data
//! - `use_reducer_*()` hooks for calling reducers with proper Dioxus integration
//! - `use_spacetimedb_connection()` hook for managing the database connection
//! - `use_subscription()` hook for subscribing to queries
//!
//! # Example usage
//!
//! ```rust,ignore
//! use dioxus::prelude::*;
//! use module_bindings::dioxus::*;
//!
//! fn App() -> Element {
//!     // Connect to the database - this sets up the connection context
//!     let connection = use_spacetimedb_connection("http://localhost:3000", "my-module");
//!     
//!     // Subscribe to tables
//!     use_subscription(&["SELECT * FROM user", "SELECT * FROM message"]);
//!     
//!     // Get reactive access to table data
//!     let users = use_table_user();
//!     
//!     // Call reducers
//!     let send_message = use_reducer_send_message();
//!     
//!     rsx! {
//!         for user in users.read().iter() {
//!             div { "{user.name}" }
//!         }
//!         button {
//!             onclick: move |_| send_message("Hello, world!".to_string()),
//!             "Send Message"
//!         }
//!     }
//! }
//! ```

use super::code_indenter::{CodeIndenter, Indenter};
use crate::util::{
    is_reducer_invokable, iter_procedures, iter_reducers, iter_table_names_and_types,
    print_auto_generated_file_comment, print_auto_generated_version_comment, print_lines, type_ref_name,
    CodegenVisibility,
};
use crate::{CodegenOptions, Lang, OutputFile};
use crate::rust::type_name;
use convert_case::{Case, Casing};
use spacetimedb_lib::sats::AlgebraicTypeRef;
use spacetimedb_schema::def::{ModuleDef, ProcedureDef, ReducerDef, TableDef, TypeDef};
use spacetimedb_schema::identifier::Identifier;
use spacetimedb_schema::schema::TableSchema;
use std::ops::Deref;

const INDENT: &str = "    ";

/// Dioxus code generator for SpacetimeDB.
///
/// This generator produces Dioxus-compatible hooks and signals alongside the standard Rust SDK types.
/// It reuses the existing Rust type definitions and adds a `dioxus` submodule with reactive wrappers.
pub struct Dioxus;

impl Lang for Dioxus {
    fn generate_type_files(&self, module: &ModuleDef, typ: &TypeDef) -> Vec<OutputFile> {
        // Reuse the Rust type generation - types are the same
        crate::rust::Rust.generate_type_files(module, typ)
    }

    fn generate_table_file_from_schema(&self, module: &ModuleDef, table: &TableDef, schema: TableSchema) -> OutputFile {
        // Reuse the Rust table generation - tables are the same
        crate::rust::Rust.generate_table_file_from_schema(module, table, schema)
    }

    fn generate_reducer_file(&self, module: &ModuleDef, reducer: &ReducerDef) -> OutputFile {
        // Reuse the Rust reducer generation - reducers are the same
        crate::rust::Rust.generate_reducer_file(module, reducer)
    }

    fn generate_procedure_file(&self, module: &ModuleDef, procedure: &ProcedureDef) -> OutputFile {
        // Reuse the Rust procedure generation - procedures are the same
        crate::rust::Rust.generate_procedure_file(module, procedure)
    }

    fn generate_global_files(&self, module: &ModuleDef, options: &CodegenOptions) -> Vec<OutputFile> {
        // Generate the standard mod.rs
        let mut files = crate::rust::Rust.generate_global_files(module, options);

        // Add the Dioxus-specific module
        files.push(generate_dioxus_module(module, options.visibility));

        // Modify mod.rs to include the dioxus module
        if let Some(mod_file) = files.iter_mut().find(|f| f.filename == "mod.rs") {
            // Insert the dioxus module declaration before the first type module
            let dioxus_decl = "\npub mod dioxus;\n";
            // Find the location after the imports but before other module declarations
            if let Some(pos) = mod_file.code.find("\npub mod ") {
                mod_file.code.insert_str(pos, dioxus_decl);
            } else {
                // If no other modules, just append
                mod_file.code.push_str(dioxus_decl);
            }
        }

        files
    }
}

fn generate_dioxus_module(module: &ModuleDef, visibility: CodegenVisibility) -> OutputFile {
    let mut output = CodeIndenter::new(String::new(), INDENT);
    let out = &mut output;

    print_file_header(out, true);

    out.newline();

    // Documentation for the module
    writeln!(
        out,
        r#"//! Dioxus signals and hooks for SpacetimeDB integration.
//!
//! This module provides reactive Dioxus hooks and signals for working with SpacetimeDB.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use dioxus::prelude::*;
//! use crate::module_bindings::dioxus::*;
//!
//! fn App() -> Element {{
//!     // Initialize the SpacetimeDB context at the root of your app
//!     // This creates all table signals at the root level to avoid lifetime issues
//!     use_spacetimedb_context_provider("http://localhost:3000", "my-module");
//!
//!     rsx! {{ Child {{}} }}
//! }}
//!
//! fn Child() -> Element {{
//!     // Use reactive table data - the signal is retrieved from root context
//!     let users = use_table_user();
//!
//!     rsx! {{
//!         for user in users.read().iter() {{
//!             div {{ "{{user.name}}" }}
//!         }}
//!     }}
//! }}
//! ```
"#
    );

    out.newline();

    // Print imports
    print_dioxus_imports(out);

    out.newline();

    // Generate the table signals context struct (holds all table signals)
    generate_table_signals_context(module, out, visibility);

    out.newline();

    // Generate the SpacetimeDB context provider hook
    generate_context_provider_hook(module, out, visibility);

    out.newline();

    // Generate the subscription hook
    generate_subscription_hook(out);

    out.newline();

    // Generate table-specific hooks (now just retrieve from context)
    for (_, accessor_name, product_type_ref) in iter_table_names_and_types(module, visibility) {
        generate_table_hook(module, out, accessor_name, product_type_ref);
        out.newline();
    }

    // Generate reducer hooks
    for reducer in iter_reducers(module, visibility) {
        if is_reducer_invokable(reducer) {
            generate_reducer_hook(module, out, reducer);
            out.newline();
        }
    }

    // Generate procedure hooks
    for procedure in iter_procedures(module, visibility) {
        generate_procedure_hook(module, out, procedure);
        out.newline();
    }

    // Generate the connection state signal
    generate_connection_state(out);

    OutputFile {
        filename: "dioxus.rs".to_string(),
        code: output.into_inner(),
    }
}

const ALLOW_LINTS: &str = "#![allow(unused, clippy::all)]";

const DIOXUS_IMPORTS: &[&str] = &[
    "use ::dioxus::prelude::*;",
    "use ::dioxus::signals::SyncSignal;",
    "use std::sync::Arc;",
    "use super::*;",
    "use spacetimedb_sdk::{DbContext, Table, TableWithPrimaryKey};",
    "use spacetimedb_sdk::__codegen::{",
    "\tself as __sdk,",
    "\t__lib,",
    "\t__sats,",
    "\t__ws,",
    "};",
    "",
    "/// Thread-safe wrapper for the database connection.",
    "/// We use SyncSignal (Signal with SyncStorage) for thread-safe access from SpacetimeDB callbacks.",
    "pub type SharedConnection = Arc<DbConnection>;",
];

fn print_dioxus_imports(output: &mut Indenter) {
    print_lines(output, DIOXUS_IMPORTS);
}

fn print_file_header(output: &mut Indenter, include_version: bool) {
    print_auto_generated_file_comment(output);
    if include_version {
        print_auto_generated_version_comment(output);
    }
    writeln!(output, "{ALLOW_LINTS}");
}

fn table_signal_field_name(accessor_name: &Identifier) -> String {
    accessor_name.deref().to_case(Case::Snake)
}

fn generate_table_signals_context(module: &ModuleDef, out: &mut Indenter, visibility: CodegenVisibility) {
    // Generate the struct that holds all table signals
    writeln!(out, "/// Container for all table signals, created at root level.");
    writeln!(
        out,
        "/// This ensures signals outlive any child components that use them."
    );
    writeln!(out, "#[derive(Clone, Copy)]");
    writeln!(out, "pub struct TableSignals {{");
    out.indent(1);

    for (_, accessor_name, product_type_ref) in iter_table_names_and_types(module, visibility) {
        let row_type = type_ref_name(module, product_type_ref);
        let field_name = table_signal_field_name(accessor_name);
        writeln!(out, "pub {field_name}: SyncSignal<Vec<{row_type}>>,");
    }

    out.dedent(1);
    writeln!(out, "}}");
}

fn generate_context_provider_hook(module: &ModuleDef, out: &mut Indenter, visibility: CodegenVisibility) {
    // First, build the list of table fields for initialization
    let mut table_fields_init = String::new();
    // We'll also build the per-table callback registrations to insert into on_connect
    let mut table_callbacks = String::new();

    for (_, accessor_name, product_type_ref) in iter_table_names_and_types(module, visibility) {
        let field_name = table_signal_field_name(accessor_name);
        let row_type = type_ref_name(module, product_type_ref);
        let table_method = accessor_name.deref().to_case(Case::Snake);

        // Field initialization
        table_fields_init.push_str(&format!("        {field_name}: use_signal_sync(Vec::new),\n"));

        // Callback registration for this table: populate initial rows and register insert/update/delete
        table_callbacks.push_str(&format!(
            r#"
                    // Initialize and register callbacks for {accessor_name}
                    let mut {field_name}_signal = table_signals.{field_name};
                    // Populate initial rows
                    let current: Vec<{row_type}> = conn.db.{table_method}().iter().collect();
                    {field_name}_signal.set(current);
                    // Keep in sync on changes
                    conn.db.{table_method}().on_insert(move |ctx, _row| {{
                        let updated: Vec<{row_type}> = ctx.db.{table_method}().iter().collect();
                        {field_name}_signal.set(updated);
                    }});
                    conn.db.{table_method}().on_update(move |ctx, _old, _new| {{
                        let updated: Vec<{row_type}> = ctx.db.{table_method}().iter().collect();
                        {field_name}_signal.set(updated);
                    }});
                    conn.db.{table_method}().on_delete(move |ctx, _row| {{
                        let updated: Vec<{row_type}> = ctx.db.{table_method}().iter().collect();
                        {field_name}_signal.set(updated);
                    }});
"#
        ));
    }

    write!(
        out,
        "{}",
        format!(
            r#"/// Internal state for managing the SpacetimeDB connection.
/// Uses SyncSignal for thread-safe access from SpacetimeDB callbacks.
#[derive(Clone, Copy)]
pub struct SpacetimeDbContext {{
    /// The database connection (wrapped in Arc for thread-safety).
    pub connection: SyncSignal<Option<SharedConnection>>,
    /// The current connection state.
    pub state: SyncSignal<ConnectionState>,
    /// Error from the last connection attempt, if any.
    pub error: SyncSignal<Option<String>>,
    /// All table signals, created at root level.
    pub tables: TableSignals,
}}

/// The current state of the SpacetimeDB connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ConnectionState {{
    /// Not connected to the database.
    #[default]
    Disconnected,
    /// Currently attempting to connect.
    Connecting,
    /// Successfully connected to the database.
    Connected,
    /// An error occurred during connection or while connected.
    Error,
}}

/// Initialize the SpacetimeDB context provider at the root of your application.
///
/// This hook must be called at the root component of your application before using
/// any other SpacetimeDB hooks. It establishes the database connection and provides
/// the context for all child components.
///
/// **Important:** All table signals are created here at the root level to ensure
/// they outlive any child components that use them. This prevents issues with
/// SpacetimeDB callbacks trying to access dropped signals.
///
/// # Arguments
///
/// * `uri` - The URI of the SpacetimeDB host (e.g., "http://localhost:3000")
/// * `module_name` - The name of the module to connect to
///
/// # Returns
///
/// A `SpacetimeDbContext` that can be used to access connection state.
///
/// # Example
///
/// ```rust,ignore
/// fn App() -> Element {{
///     let ctx = use_spacetimedb_context_provider("http://localhost:3000", "my-module");
///
///     match ctx.state() {{
///         ConnectionState::Connected => rsx! {{ Child {{}} }},
///         ConnectionState::Connecting => rsx! {{ "Connecting..." }},
///         ConnectionState::Disconnected => rsx! {{ "Disconnected" }},
///         ConnectionState::Error => rsx! {{ "Error: {{ctx.error().unwrap_or_default()}}" }},
///     }}
/// }}
/// ```
#[must_use]
pub fn use_spacetimedb_context_provider(uri: &str, module_name: &str) -> SpacetimeDbContext {{
    let uri = uri.to_string();
    let module_name = module_name.to_string();

    // Use SyncSignal for thread-safe access from SpacetimeDB callbacks
    let connection: SyncSignal<Option<SharedConnection>> = use_signal_sync(|| None);
    let state: SyncSignal<ConnectionState> = use_signal_sync(|| ConnectionState::Disconnected);
    let error: SyncSignal<Option<String>> = use_signal_sync(|| None);
    
    // Create all table signals at root level - this is crucial!
    // These signals must outlive any child components that use them.
    let table_signals = TableSignals {{
{table_fields_init}    }};

    let ctx = SpacetimeDbContext {{
        connection,
        state,
        error,
        tables: table_signals,
    }};

    // Provide the context to child components
    use_context_provider(|| ctx);

    // Connect on first render
    use_effect(move || {{
        let mut connection = connection;
        let mut state = state;
        let mut error = error;
        let uri = uri.clone();
        let module_name = module_name.clone();

        state.set(ConnectionState::Connecting);

        spawn(async move {{
            match DbConnection::builder()
                .with_uri(&uri)
                .with_module_name(&module_name)
                .on_connect(move |conn, _identity, _token| {{
                    // Initialize table signals with current data and register callbacks
{table_callbacks}
                }})
                .on_disconnect(move |_ctx, err| {{
                    // SyncSignal is Send + Sync, so we can safely update from callbacks
                    if let Some(e) = err {{
                        error.set(Some(e.to_string()));
                        state.set(ConnectionState::Error);
                    }} else {{
                        state.set(ConnectionState::Disconnected);
                    }}
                }})
                .build()
            {{
                Ok(conn) => {{
                    let shared_conn = Arc::new(conn);
                    connection.set(Some(shared_conn.clone()));
                    state.set(ConnectionState::Connected);

                    // Run the connection in a background task
                    spawn(async move {{
                        let _ = shared_conn.run_async().await;
                    }});
                }}
                Err(e) => {{
                    error.set(Some(e.to_string()));
                    state.set(ConnectionState::Error);
                }}
            }}
        }});
    }});

    ctx
}}

/// Get the SpacetimeDB context from a parent component.
///
/// This hook retrieves the SpacetimeDB context that was set up by `use_spacetimedb_context_provider`.
/// It must be called in a component that is a descendant of a component that called
/// `use_spacetimedb_context_provider`.
///
/// # Panics
///
/// Panics if called outside of a component tree that has a `use_spacetimedb_context_provider`.
///
/// # Example
///
/// ```rust,ignore
/// fn Child() -> Element {{
///     let ctx = use_spacetimedb_context();
///
///     if ctx.state() == ConnectionState::Connected {{
///         // Use the connection
///     }}
///
///     rsx! {{ "Connection state: {{ctx.state():?}}" }}
/// }}
/// ```
#[must_use]
pub fn use_spacetimedb_context() -> SpacetimeDbContext {{
    use_context::<SpacetimeDbContext>()
}}

/// Get the current database connection, if connected.
///
/// Returns `None` if not yet connected.
#[must_use]
pub fn use_connection() -> SyncSignal<Option<SharedConnection>> {{
    let ctx = use_spacetimedb_context();
    ctx.connection
}}
            "#,
            table_fields_init = table_fields_init,
            table_callbacks = table_callbacks
        )
    );
}

fn generate_subscription_hook(out: &mut Indenter) {
    write!(
        out,
        r#"/// Subscribe to a set of SQL queries.
///
/// This hook subscribes to the given SQL queries and keeps the local cache in sync
/// with the database. The queries will be executed on the server and any matching
/// rows will be replicated to the client.
///
/// # Arguments
///
/// * `queries` - A slice of SQL query strings to subscribe to
///
/// # Example
///
/// ```rust,ignore
/// fn MyComponent() -> Element {{
///     // Subscribe to all users and messages
///     use_subscription(&["SELECT * FROM user", "SELECT * FROM message"]);
///
///     // Now you can use table hooks to access the data
///     let users = use_table_user();
///     // ...
/// }}
/// ```
pub fn use_subscription(queries: &[&str]) {{
    let queries: Vec<String> = queries.iter().map(|s| s.to_string()).collect();
    let conn_signal = use_connection();

    use_effect(move || {{
        let queries = queries.clone();
        if let Some(conn) = conn_signal.read().as_ref() {{
            conn.subscription_builder()
                .on_applied(|_ctx| {{
                    // Subscription applied successfully
                }})
                .on_error(|_ctx, _err| {{
                    // Handle subscription error
                }})
                .subscribe(queries);
        }}
    }});
}}
"#
    );
}

fn table_hook_name(accessor_name: &Identifier) -> String {
    format!("use_table_{}", accessor_name.deref().to_case(Case::Snake))
}

fn generate_table_hook(
    module: &ModuleDef,
    out: &mut Indenter,
    accessor_name: &Identifier,
    product_type_ref: AlgebraicTypeRef,
) {
    let row_type = type_ref_name(module, product_type_ref);
    let hook_name = table_hook_name(accessor_name);
    let field_name = table_signal_field_name(accessor_name);

    write!(
        out,
        r#"/// Get a reactive signal containing all rows of the `{accessor_name}` table.
///
/// This hook returns a signal that automatically updates when the `{accessor_name}` table changes.
/// The signal contains a `Vec` of all `{row_type}` rows currently in the local cache.
///
/// The signal is created at root level by `use_spacetimedb_context_provider` and retrieved
/// here from the context, ensuring it outlives any child components.
///
/// # Example
///
/// ```rust,ignore
/// fn {row_type}List() -> Element {{
///     let items = {hook_name}();
///
///     rsx! {{
///         for item in items.read().iter() {{
///             div {{ "{{item:?}}" }}
///         }}
///     }}
/// }}
/// ```
#[must_use]
pub fn {hook_name}() -> SyncSignal<Vec<{row_type}>> {{
    let ctx = use_spacetimedb_context();
    ctx.tables.{field_name}
}}
"#
    );
}

fn reducer_hook_name(reducer: &ReducerDef) -> String {
    format!("use_reducer_{}", reducer.name.deref().to_case(Case::Snake))
}

fn generate_reducer_hook(module: &ModuleDef, out: &mut Indenter, reducer: &ReducerDef) {
    let hook_name = reducer_hook_name(reducer);
    let reducer_name = reducer.name.deref();
    let func_name = reducer.name.deref().to_case(Case::Snake);

    // Build the argument list for the closure (with names) and for the trait bound (types only)
    let mut args_decl = String::new();
    let mut args_types = String::new();
    let mut args_call = String::new();
    for (i, (arg_name, arg_ty)) in reducer.params_for_generate.elements.iter().enumerate() {
        let name = arg_name.deref().to_case(Case::Snake);
        let ty = type_name(module, arg_ty);
        if i > 0 {
            args_decl.push_str(", ");
            args_types.push_str(", ");
            args_call.push_str(", ");
        }
        args_decl.push_str(&format!("{name}: {ty}"));
        args_types.push_str(&ty);
        args_call.push_str(&name);
    }

    // Generate appropriate callback signature (types only in trait bound)
    let callback_sig = if reducer.params_for_generate.elements.is_empty() {
        "impl Fn() + Clone + 'static".to_string()
    } else {
        format!("impl Fn({args_types}) + Clone + 'static")
    };

    write!(
        out,
        r#"/// Get a callback to invoke the `{reducer_name}` reducer.
///
/// This hook returns a callback that can be used to call the `{reducer_name}` reducer.
/// The callback is Clone and can be used in event handlers.
///
/// # Example
///
/// ```rust,ignore
/// fn MyComponent() -> Element {{
///     let {func_name} = {hook_name}();
///
///     rsx! {{
///         button {{
///             onclick: move |_| {func_name}({args_call}),
///             "Call {reducer_name}"
///         }}
///     }}
/// }}
/// ```
#[must_use]
pub fn {hook_name}() -> {callback_sig} {{
    let conn_signal = use_connection();

    move |{args_decl}| {{
        if let Some(conn) = conn_signal.read().as_ref() {{
            let _ = conn.reducers.{func_name}({args_call});
        }}
    }}
}}
"#
    );
}

fn procedure_hook_name(procedure: &ProcedureDef) -> String {
    format!("use_procedure_{}", procedure.name.deref().to_case(Case::Snake))
}

fn generate_procedure_hook(module: &ModuleDef, out: &mut Indenter, procedure: &ProcedureDef) {
    let hook_name = procedure_hook_name(procedure);
    let procedure_name = procedure.name.deref();
    let func_name = procedure.name.deref().to_case(Case::Snake);

    // Build the argument list for the closure (with names) and for the trait bound (types only)
    let mut args_decl = String::new();
    let mut args_types = String::new();
    let mut args_call = String::new();
    for (i, (arg_name, arg_ty)) in procedure.params_for_generate.elements.iter().enumerate() {
        let name = arg_name.deref().to_case(Case::Snake);
        let ty = type_name(module, arg_ty);
        if i > 0 {
            args_decl.push_str(", ");
            args_types.push_str(", ");
            args_call.push_str(", ");
        }
        args_decl.push_str(&format!("{name}: {ty}"));
        args_types.push_str(&ty);
        args_call.push_str(&name);
    }

    // Generate appropriate callback signature (types only in trait bound)
    let callback_sig = if procedure.params_for_generate.elements.is_empty() {
        "impl Fn() + Clone + 'static".to_string()
    } else {
        format!("impl Fn({args_types}) + Clone + 'static")
    };

    write!(
        out,
        r#"/// Get a callback to invoke the `{procedure_name}` procedure.
///
/// This hook returns a callback that can be used to call the `{procedure_name}` procedure.
/// The callback is Clone and can be used in event handlers.
///
/// # Example
///
/// ```rust,ignore
/// fn MyComponent() -> Element {{
///     let {func_name} = {hook_name}();
///
///     rsx! {{
///         button {{
///             onclick: move |_| {func_name}({args_call}),
///             "Call {procedure_name}"
///         }}
///     }}
/// }}
/// ```
#[must_use]
pub fn {hook_name}() -> {callback_sig} {{
    let conn_signal = use_connection();

    move |{args_decl}| {{
        if let Some(conn) = conn_signal.read().as_ref() {{
            conn.procedures.{func_name}({args_call});
        }}
    }}
}}
"#
    );
}

fn generate_connection_state(out: &mut Indenter) {
    write!(
        out,
        r#"/// Get a reactive signal for the current connection state.
///
/// This hook returns a signal that automatically updates when the connection state changes.
///
/// # Example
///
/// ```rust,ignore
/// fn ConnectionStatus() -> Element {{
///     let state = use_connection_state();
///
///     rsx! {{
///         match state() {{
///             ConnectionState::Connected => "Connected",
///             ConnectionState::Connecting => "Connecting...",
///             ConnectionState::Disconnected => "Disconnected",
///             ConnectionState::Error => "Error",
///         }}
///     }}
/// }}
/// ```
#[must_use]
pub fn use_connection_state() -> SyncSignal<ConnectionState> {{
    let ctx = use_spacetimedb_context();
    ctx.state
}}

/// Get a reactive signal for the current connection error, if any.
///
/// This hook returns a signal that automatically updates when an error occurs.
///
/// # Example
///
/// ```rust,ignore
/// fn ErrorDisplay() -> Element {{
///     let error = use_connection_error();
///
///     rsx! {{
///         if let Some(err) = error() {{
///             div {{ class: "error", "Error: {{err}}" }}
///         }}
///     }}
/// }}
/// ```
#[must_use]
pub fn use_connection_error() -> SyncSignal<Option<String>> {{
    let ctx = use_spacetimedb_context();
    ctx.error
}}
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_hook_name() {
        let ident = Identifier::new("user_profile".into()).unwrap();
        assert_eq!(table_hook_name(&ident), "use_table_user_profile");
    }

    #[test]
    fn test_reducer_hook_name() {
        // This is a simplified test - in practice we'd need a full ReducerDef
    }
}
