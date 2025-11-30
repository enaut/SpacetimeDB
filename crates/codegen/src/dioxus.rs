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
use super::util::{
    is_reducer_invokable, iter_procedures, iter_reducers, iter_table_names_and_types,
    print_auto_generated_file_comment, print_auto_generated_version_comment, print_lines, type_ref_name,
};
use super::{Lang, OutputFile};
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

    fn generate_global_files(&self, module: &ModuleDef) -> Vec<OutputFile> {
        // Generate the standard mod.rs
        let mut files = crate::rust::Rust.generate_global_files(module);

        // Add the Dioxus-specific module
        files.push(generate_dioxus_module(module));

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

fn generate_dioxus_module(module: &ModuleDef) -> OutputFile {
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
//!     use_spacetimedb_context_provider("http://localhost:3000", "my-module");
//!
//!     rsx! {{ Child {{}} }}
//! }}
//!
//! fn Child() -> Element {{
//!     // Use reactive table data
//!     let users = use_table_all::<User>();
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

    // Generate the SpacetimeDB context provider hook
    generate_context_provider_hook(out);

    out.newline();

    // Generate the subscription hook
    generate_subscription_hook(out);

    out.newline();

    // Generate generic table hook
    generate_generic_table_hook(out);

    out.newline();

    // Generate table-specific hooks
    for (table_name, product_type_ref) in iter_table_names_and_types(module) {
        generate_table_hook(module, out, table_name, product_type_ref);
        out.newline();
    }

    // Generate reducer hooks
    for reducer in iter_reducers(module) {
        if is_reducer_invokable(reducer) {
            generate_reducer_hook(module, out, reducer);
            out.newline();
        }
    }

    // Generate procedure hooks
    for procedure in iter_procedures(module) {
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
    "use std::sync::Arc;",
    "use super::*;",
    "use spacetimedb_sdk::{DbContext, Table};",
    "use spacetimedb_sdk::__codegen::{",
    "\tself as __sdk,",
    "\t__lib,",
    "\t__sats,",
    "\t__ws,",
    "};",
    "",
    "/// Thread-safe wrapper for the database connection.",
    "/// In Dioxus 0.7+, Signals are Copy and thread-safe when T: Send + Sync.",
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

fn generate_context_provider_hook(out: &mut Indenter) {
    write!(
        out,
        r#"/// Internal state for managing the SpacetimeDB connection.
#[derive(Clone)]
pub struct SpacetimeDbContext {{
    /// The database connection (wrapped in Arc for thread-safety).
    pub connection: Signal<Option<SharedConnection>>,
    /// The current connection state.
    pub state: Signal<ConnectionState>,
    /// Error from the last connection attempt, if any.
    pub error: Signal<Option<String>>,
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

    let connection: Signal<Option<SharedConnection>> = use_signal(|| None);
    let state: Signal<ConnectionState> = use_signal(|| ConnectionState::Disconnected);
    let error: Signal<Option<String>> = use_signal(|| None);

    let ctx = SpacetimeDbContext {{
        connection,
        state,
        error,
    }};

    // Provide the context to child components
    use_context_provider(|| ctx.clone());

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
                .on_connect(move |_conn, _identity, _token| {{
                    // Connection successful - state is set below after build()
                }})
                .on_disconnect(move |_ctx, err| {{
                    // Clone signals for use in callback (they are Copy in Dioxus 0.7+)
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
pub fn use_connection() -> Signal<Option<SharedConnection>> {{
    let ctx = use_spacetimedb_context();
    ctx.connection
}}
"#
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

fn generate_generic_table_hook(out: &mut Indenter) {
    write!(
        out,
        r#"/// Get a reactive signal containing all rows of a table.
///
/// This hook returns a signal that automatically updates when the table data changes.
/// The signal contains a `Vec` of all rows currently in the local cache.
///
/// In Dioxus 0.7+, Signals are Copy and thread-safe when T: Send + Sync,
/// so we can safely update them from SpacetimeDB callbacks.
///
/// # Type Parameters
///
/// * `T` - The row type of the table
///
/// # Example
///
/// ```rust,ignore
/// fn UserList() -> Element {{
///     let users: Signal<Vec<User>> = use_table_all::<User>();
///
///     rsx! {{
///         for user in users.read().iter() {{
///             div {{ "{{user.name}}" }}
///         }}
///     }}
/// }}
/// ```
#[must_use]
pub fn use_table_all<T>() -> Signal<Vec<T>>
where
    T: __sdk::InModule<Module = RemoteModule> + Clone + Send + Sync + 'static,
{{
    use_signal(Vec::new)
}}
"#
    );
}

fn table_hook_name(table_name: &Identifier) -> String {
    format!("use_table_{}", table_name.deref().to_case(Case::Snake))
}

fn generate_table_hook(_module: &ModuleDef, out: &mut Indenter, table_name: &Identifier, product_type_ref: AlgebraicTypeRef) {
    let row_type = type_ref_name(_module, product_type_ref);
    let hook_name = table_hook_name(table_name);
    let table_method = table_name.deref().to_case(Case::Snake);

    write!(
        out,
        r#"/// Get a reactive signal containing all rows of the `{table_name}` table.
///
/// This hook returns a signal that automatically updates when the `{table_name}` table changes.
/// The signal contains a `Vec` of all `{row_type}` rows currently in the local cache.
///
/// In Dioxus 0.7+, Signals are Copy and thread-safe when T: Send + Sync,
/// so we can safely update them from SpacetimeDB callbacks.
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
pub fn {hook_name}() -> Signal<Vec<{row_type}>> {{
    let data: Signal<Vec<{row_type}>> = use_signal(Vec::new);
    let conn_signal = use_connection();

    use_effect(move || {{
        if let Some(conn) = conn_signal.read().as_ref() {{
            // Initialize with current data
            let initial_data: Vec<{row_type}> = conn.db.{table_method}().iter().collect();
            data.set(initial_data);

            // Set up callbacks for updates - Signals are Copy in Dioxus 0.7+
            conn.db.{table_method}().on_insert(move |_ctx, row| {{
                let mut current = data.read().clone();
                current.push(row.clone());
                data.set(current);
            }});

            conn.db.{table_method}().on_delete(move |_ctx, row| {{
                let current: Vec<{row_type}> = data.read().iter()
                    .filter(|r| *r != row)
                    .cloned()
                    .collect();
                data.set(current);
            }});
        }}
    }});

    data
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
    let _return_type = type_name(module, &procedure.return_type_for_generate);

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
pub fn use_connection_state() -> Signal<ConnectionState> {{
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
pub fn use_connection_error() -> Signal<Option<String>> {{
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
