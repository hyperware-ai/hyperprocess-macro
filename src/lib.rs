#![allow(warnings)]
use proc_macro::TokenStream;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse_macro_input, punctuated::Punctuated, spanned::Spanned, token::Comma, Expr, ItemImpl,
    Meta, ReturnType,
};

//------------------------------------------------------------------------------
// Type Definitions
//------------------------------------------------------------------------------

/// Keywords for parsing attribute arguments
mod kw {
    syn::custom_keyword!(name);
    syn::custom_keyword!(icon);
    syn::custom_keyword!(widget);
    syn::custom_keyword!(ui);
    syn::custom_keyword!(endpoints);
    syn::custom_keyword!(save_config);
    syn::custom_keyword!(wit_world);
}

/// A wrapper for a punctuated list of Meta items
struct MetaList(Punctuated<Meta, Comma>);

/// Arguments for the hyperprocess macro
struct HyperProcessArgs {
    name: String,
    icon: Option<String>,
    widget: Option<String>,
    ui: Option<Expr>,
    endpoints: Expr,
    save_config: Expr,
    wit_world: String,
}

/// Metadata for a function in the implementation block
#[derive(Clone)]
struct FunctionMetadata {
    name: syn::Ident,               // Original function name
    variant_name: String,           // CamelCase variant name
    params: Vec<syn::Type>,         // Parameter types (excluding &mut self)
    return_type: Option<syn::Type>, // Return type (None for functions returning ())
    is_async: bool,                 // Whether function is async
    is_local: bool,                 // Has #[local] attribute
    is_remote: bool,                // Has #[remote] attribute
    is_http: bool,                  // Has #[http] attribute
    http_methods: Vec<String>,      // HTTP methods this handler accepts (GET, POST, etc.)
    http_path: Option<String>,      // Specific path this handler is bound to (optional)
}

/// Enum for the different handler types
#[derive(Copy, Clone)]
enum HandlerType {
    Local,
    Remote,
    Http,
}

/// Grouped handlers by type
struct HandlerGroups<'a> {
    local: Vec<&'a FunctionMetadata>,
    remote: Vec<&'a FunctionMetadata>,
    http: Vec<&'a FunctionMetadata>,
    // New group for combined handlers (used for local messages that can also use remote handlers)
    local_and_remote: Vec<&'a FunctionMetadata>,
}

impl<'a> HandlerGroups<'a> {
    fn from_function_metadata(metadata: &'a [FunctionMetadata]) -> Self {
        // Collect handlers that are explicitly marked as local
        let local: Vec<_> = metadata.iter().filter(|f| f.is_local).collect();

        // Collect handlers that are explicitly marked as remote
        let remote: Vec<_> = metadata.iter().filter(|f| f.is_remote).collect();

        // Collect HTTP handlers
        let http: Vec<_> = metadata.iter().filter(|f| f.is_http).collect();

        // Create a combined list of local and remote handlers for local messages
        // We first include all local handlers, then add remote handlers that aren't already covered
        let mut local_and_remote = local.clone();
        for handler in remote.iter() {
            // Check if this remote handler is already in the local_and_remote list
            if !local_and_remote
                .iter()
                .any(|h| h.variant_name == handler.variant_name)
            {
                local_and_remote.push(handler);
            }
        }

        HandlerGroups {
            local,
            remote,
            http,
            local_and_remote,
        }
    }
}

/// Handler dispatch code fragments
struct HandlerDispatch {
    local: proc_macro2::TokenStream,
    remote: proc_macro2::TokenStream,
    http: proc_macro2::TokenStream,
    local_and_remote: proc_macro2::TokenStream,
}

/// Init method details for code generation
struct InitMethodDetails {
    identifier: proc_macro2::TokenStream,
    call: proc_macro2::TokenStream,
}

/// WebSocket method details for code generation
struct WsMethodDetails {
    identifier: proc_macro2::TokenStream,
    call: proc_macro2::TokenStream,
}

//------------------------------------------------------------------------------
// Parse Implementation
//------------------------------------------------------------------------------

/// Implement Parse for our MetaList newtype wrapper
impl syn::parse::Parse for MetaList {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut args = Punctuated::new();
        while !input.is_empty() {
            args.push_value(input.parse()?);
            if input.is_empty() {
                break;
            }
            args.push_punct(input.parse()?);
        }
        Ok(MetaList(args))
    }
}

//------------------------------------------------------------------------------
// Utility Functions
//------------------------------------------------------------------------------

/// Convert a snake_case string to CamelCase
fn to_camel_case(snake: &str) -> String {
    let mut camel = String::new();
    let mut capitalize_next = true;

    for c in snake.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            camel.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            camel.push(c);
        }
    }

    camel
}

/// Parse a string literal from an expression
fn parse_string_literal(expr: &Expr, span: proc_macro2::Span) -> syn::Result<String> {
    if let Expr::Lit(expr_lit) = expr {
        if let syn::Lit::Str(lit) = &expr_lit.lit {
            Ok(lit.value())
        } else {
            Err(syn::Error::new(span, "Expected string literal"))
        }
    } else {
        Err(syn::Error::new(span, "Expected string literal"))
    }
}

/// Parse the UI expression (handling Some() wrapper)
fn parse_ui_expr(expr: &Expr) -> syn::Result<Option<Expr>> {
    if let Expr::Call(call) = expr {
        if let Expr::Path(path) = &*call.func {
            if path
                .path
                .segments
                .last()
                .map(|s| s.ident == "Some")
                .unwrap_or(false)
            {
                if call.args.len() == 1 {
                    return Ok(Some(call.args[0].clone()));
                } else {
                    return Err(syn::Error::new(
                        call.span(),
                        "Some must have exactly one argument",
                    ));
                }
            }
        }
    }
    Ok(Some(expr.clone()))
}

/// Check if a method has a specific attribute
fn has_attribute(method: &syn::ImplItemFn, attr_name: &str) -> bool {
    method
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident(attr_name))
}

/// Parse HTTP methods and path from the #[http] attribute
/// Supports: #[http], #[http(method = "GET")], #[http(method = "POST", path = "/api")]
fn parse_http_attributes(method: &syn::ImplItemFn) -> (Vec<String>, Option<String>) {
    for attr in &method.attrs {
        if attr.path().is_ident("http") {
            // Handle #[http] with no arguments - defaults to ALL methods
            if matches!(&attr.meta, syn::Meta::Path(_)) {
                return (
                    vec![
                        "GET".to_string(),
                        "POST".to_string(),
                        "PUT".to_string(),
                        "DELETE".to_string(),
                        "PATCH".to_string(),
                        "HEAD".to_string(),
                        "OPTIONS".to_string(),
                    ],
                    None,
                );
            }

            // Handle #[http(method = "GET", path = "/api")]
            if let syn::Meta::List(list) = &attr.meta {
                let mut methods = None;
                let mut path = None;

                // Parse the token stream manually
                let tokens: Vec<_> = list.tokens.clone().into_iter().collect();
                let mut i = 0;

                while i < tokens.len() {
                    // Look for identifier (method or path)
                    if let proc_macro2::TokenTree::Ident(ident) = &tokens[i] {
                        let ident_str = ident.to_string();

                        // Check for = sign
                        if i + 2 < tokens.len() {
                            if let proc_macro2::TokenTree::Punct(punct) = &tokens[i + 1] {
                                if punct.as_char() == '=' {
                                    // Get the string literal
                                    if let proc_macro2::TokenTree::Literal(lit) = &tokens[i + 2] {
                                        let lit_str = lit.to_string();
                                        // Remove quotes from the literal
                                        let value = lit_str.trim_matches('"');

                                        if ident_str == "method" {
                                            let method = value.to_uppercase();
                                            if matches!(
                                                method.as_str(),
                                                "GET"
                                                    | "POST"
                                                    | "PUT"
                                                    | "DELETE"
                                                    | "PATCH"
                                                    | "HEAD"
                                                    | "OPTIONS"
                                            ) {
                                                methods = Some(vec![method]);
                                            }
                                        } else if ident_str == "path" {
                                            path = Some(value.to_string());
                                        }
                                    }
                                    i += 3; // Skip ident, =, and literal

                                    // Skip comma if present
                                    if i < tokens.len() {
                                        if let proc_macro2::TokenTree::Punct(punct) = &tokens[i] {
                                            if punct.as_char() == ',' {
                                                i += 1;
                                            }
                                        }
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                    i += 1;
                }

                // Default to ALL methods if none specified
                let final_methods = methods.unwrap_or_else(|| {
                    vec![
                        "GET".to_string(),
                        "POST".to_string(),
                        "PUT".to_string(),
                        "DELETE".to_string(),
                        "PATCH".to_string(),
                        "HEAD".to_string(),
                        "OPTIONS".to_string(),
                    ]
                });

                return (final_methods, path);
            }

            // Default to ALL methods if parsing fails
            return (
                vec![
                    "GET".to_string(),
                    "POST".to_string(),
                    "PUT".to_string(),
                    "DELETE".to_string(),
                    "PATCH".to_string(),
                    "HEAD".to_string(),
                    "OPTIONS".to_string(),
                ],
                None,
            );
        }
    }
    (Vec::new(), None)
}

/// Remove our custom attributes from the implementation block
fn clean_impl_block(impl_block: &ItemImpl) -> ItemImpl {
    let mut cleaned_impl_block = impl_block.clone();
    for item in &mut cleaned_impl_block.items {
        if let syn::ImplItem::Fn(method) = item {
            method.attrs.retain(|attr| {
                !attr.path().is_ident("init")
                    && !attr.path().is_ident("http")
                    && !attr.path().is_ident("local")
                    && !attr.path().is_ident("remote")
                    && !attr.path().is_ident("ws")
            });
        }
    }
    cleaned_impl_block
}

/// Check if a method has a valid self receiver (&mut self)
fn has_valid_self_receiver(method: &syn::ImplItemFn) -> bool {
    method
        .sig
        .inputs
        .first()
        .map_or(false, |arg| matches!(arg, syn::FnArg::Receiver(_)))
}

//------------------------------------------------------------------------------
// Argument Parsing Functions
//------------------------------------------------------------------------------

/// Parse the arguments to the hyperprocess macro
fn parse_args(attr_args: MetaList) -> syn::Result<HyperProcessArgs> {
    let mut name = None;
    let mut icon = None;
    let mut widget = None;
    let mut ui = None;
    let mut endpoints = None;
    let mut save_config = None;
    let mut wit_world = None;

    let span = attr_args
        .0
        .first()
        .map_or_else(|| proc_macro2::Span::call_site(), |arg| arg.span());

    for arg in &attr_args.0 {
        if let Meta::NameValue(nv) = arg {
            let key = nv.path.get_ident().unwrap().to_string();
            match key.as_str() {
                "name" => {
                    name = Some(parse_string_literal(&nv.value, nv.value.span())?);
                }
                "icon" => {
                    icon = Some(parse_string_literal(&nv.value, nv.value.span())?);
                }
                "widget" => {
                    widget = Some(parse_string_literal(&nv.value, nv.value.span())?);
                }
                "ui" => {
                    ui = parse_ui_expr(&nv.value)?;
                }
                "endpoints" => endpoints = Some(nv.value.clone()),
                "save_config" => save_config = Some(nv.value.clone()),
                "wit_world" => {
                    wit_world = Some(parse_string_literal(&nv.value, nv.value.span())?);
                }
                _ => return Err(syn::Error::new(nv.path.span(), "Unknown attribute")),
            }
        } else {
            return Err(syn::Error::new(arg.span(), "Expected name-value pair"));
        }
    }

    Ok(HyperProcessArgs {
        name: name.ok_or_else(|| syn::Error::new(span, "Missing 'name'"))?,
        icon,
        widget,
        ui,
        endpoints: endpoints.ok_or_else(|| syn::Error::new(span, "Missing 'endpoints'"))?,
        save_config: save_config.ok_or_else(|| syn::Error::new(span, "Missing 'save_config'"))?,
        wit_world: wit_world.ok_or_else(|| syn::Error::new(span, "Missing 'wit_world'"))?,
    })
}

//------------------------------------------------------------------------------
// Method Validation Functions
//------------------------------------------------------------------------------

/// Validate the init method signature
fn validate_init_method(method: &syn::ImplItemFn) -> syn::Result<()> {
    // Ensure the method is async
    if method.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "Init method must be declared as async",
        ));
    }

    // Ensure first param is &mut self
    if !has_valid_self_receiver(method) {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "Init method must take &mut self as first parameter",
        ));
    }

    // Ensure no other parameters
    if method.sig.inputs.len() > 1 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "Init method must not take any parameters other than &mut self",
        ));
    }

    // Validate return type
    if !matches!(method.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "Init method must not return a value",
        ));
    }

    Ok(())
}

/// Validate the websocket method signature
fn validate_websocket_method(method: &syn::ImplItemFn) -> syn::Result<()> {
    // Ensure first param is &mut self
    if !has_valid_self_receiver(method) {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "WebSocket method must take &mut self as first parameter",
        ));
    }

    // Ensure there are exactly 4 parameters (including &mut self)
    if method.sig.inputs.len() != 4 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "WebSocket method must take exactly 3 additional parameters: channel_id, message_type, and blob",
        ));
    }

    // Get parameters (excluding &mut self)
    let params: Vec<_> = method.sig.inputs.iter().skip(1).collect();

    // Check parameter types (we're not doing exact type checking, just rough check)
    let channel_id_param = &params[0];
    let message_type_param = &params[1];
    let blob_param = &params[2];

    if let syn::FnArg::Typed(pat_type) = channel_id_param {
        if !pat_type.ty.to_token_stream().to_string().contains("u32") {
            return Err(syn::Error::new_spanned(
                pat_type,
                "First parameter of WebSocket method must be channel_id: u32",
            ));
        }
    }

    if let syn::FnArg::Typed(pat_type) = message_type_param {
        let type_str = pat_type.ty.to_token_stream().to_string();
        if !type_str.contains("WsMessageType") && !type_str.contains("MessageType") {
            return Err(syn::Error::new_spanned(
                pat_type,
                "Second parameter of WebSocket method must be message_type: WsMessageType",
            ));
        }
    }

    if let syn::FnArg::Typed(pat_type) = blob_param {
        if !pat_type
            .ty
            .to_token_stream()
            .to_string()
            .contains("LazyLoadBlob")
        {
            return Err(syn::Error::new_spanned(
                pat_type,
                "Third parameter of WebSocket method must be blob: LazyLoadBlob",
            ));
        }
    }

    // Validate return type (must be unit)
    if !matches!(method.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &method.sig.output,
            "WebSocket method must not return a value",
        ));
    }

    Ok(())
}

/// Validate a request-response function signature
fn validate_request_response_function(method: &syn::ImplItemFn) -> syn::Result<()> {
    // Ensure first param is &mut self
    if !has_valid_self_receiver(method) {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "Request-response handlers must take &mut self as their first parameter",
        ));
    }

    // No limit on additional parameters - we support any number
    // No validation for return type - any return type is allowed

    Ok(())
}

//------------------------------------------------------------------------------
// Method Analysis Functions
//------------------------------------------------------------------------------

/// Analyze the methods in an implementation block
fn analyze_methods(
    impl_block: &ItemImpl,
) -> syn::Result<(
    Option<syn::Ident>,    // init method
    Option<syn::Ident>,    // ws method
    Vec<FunctionMetadata>, // metadata for request/response methods
    bool,                  // whether init method contains logging init
)> {
    let mut init_method = None;
    let mut ws_method = None;
    let mut has_init_logging = false;
    let mut function_metadata = Vec::new();

    for item in &impl_block.items {
        if let syn::ImplItem::Fn(method) = item {
            let ident = method.sig.ident.clone();

            // Check for method attributes
            let has_init = has_attribute(method, "init");
            let has_http = has_attribute(method, "http");
            let has_local = has_attribute(method, "local");
            let has_remote = has_attribute(method, "remote");
            let has_ws = has_attribute(method, "ws");

            // Handle init method
            if has_init {
                if has_http || has_local || has_remote || has_ws {
                    return Err(syn::Error::new_spanned(
                        method,
                        "#[init] cannot be combined with other attributes",
                    ));
                }
                validate_init_method(method)?;
                if init_method.is_some() {
                    return Err(syn::Error::new_spanned(
                        method,
                        "Multiple #[init] methods defined",
                    ));
                }
                init_method = Some(ident);

                // Check if init_method contains logging init
                has_init_logging = contains_init_logging(method);

                continue;
            }

            // Handle WebSocket method
            if has_ws {
                if has_http || has_local || has_remote || has_init {
                    return Err(syn::Error::new_spanned(
                        method,
                        "#[ws] cannot be combined with other attributes",
                    ));
                }
                validate_websocket_method(method)?;
                if ws_method.is_some() {
                    return Err(syn::Error::new_spanned(
                        method,
                        "Multiple #[ws] methods defined",
                    ));
                }
                ws_method = Some(ident);
                continue;
            }

            // Handle request-response methods
            if has_http || has_local || has_remote {
                validate_request_response_function(method)?;
                let metadata = extract_function_metadata(method, has_local, has_remote, has_http);

                // Parameter-less HTTP handlers can optionally specify a path, but it's not required
                // They can use get_path() and get_method() to handle requests dynamically

                function_metadata.push(metadata);
            }
        }
    }

    // Check if we have at least one handler
    if function_metadata.is_empty() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "You must specify at least one handler with #[remote], #[local], or #[http] attribute. Without any handlers, this hyperprocess wouldn't respond to any requests.",
        ));
    }

    // Check for duplicate HTTP (method + path) combinations
    // Only validate specific paths - allow multiple method-only handlers for dynamic routing
    let mut http_routes = std::collections::HashMap::new();
    for func in &function_metadata {
        if func.is_http {
            // Only validate handlers with specific paths
            if let Some(path) = &func.http_path {
                for method in &func.http_methods {
                    let route_key = (method.clone(), path.clone());
                    if let Some(existing_handler) = http_routes.get(&route_key) {
                        return Err(syn::Error::new(
                            proc_macro2::Span::call_site(),
                            format!(
                                "Duplicate HTTP route detected: {} {}\n\
                                First handler: {}\n\
                                Second handler: {}\n\
                                \n\
                                Each (method + specific path) combination must map to exactly one handler.\n\
                                Consider:\n\
                                - Using different paths for different handlers\n\
                                - Combining the logic into a single handler\n\
                                - Using method-only handlers with get_path() for dynamic routing",
                                method, path, existing_handler, func.name
                            ),
                        ));
                    }
                    http_routes.insert(route_key, &func.name);
                }
            }
            // Method-only handlers (no specific path) are allowed to coexist
            // They can use get_path() at runtime to implement custom routing logic
        }
    }

    Ok((init_method, ws_method, function_metadata, has_init_logging))
}

/// Extract metadata from a function
fn extract_function_metadata(
    method: &syn::ImplItemFn,
    is_local: bool,
    is_remote: bool,
    is_http: bool,
) -> FunctionMetadata {
    let ident = method.sig.ident.clone();

    // Extract parameter types (skipping &mut self)
    let params = method
        .sig
        .inputs
        .iter()
        .skip(1)
        .filter_map(|input| {
            if let syn::FnArg::Typed(pat_type) = input {
                Some((*pat_type.ty).clone())
            } else {
                None
            }
        })
        .collect();

    // Extract return type
    let return_type = match &method.sig.output {
        ReturnType::Default => None, // () - no explicit return
        ReturnType::Type(_, ty) => Some((**ty).clone()),
    };

    // Create variant name (snake_case to CamelCase)
    let variant_name = to_camel_case(&ident.to_string());

    // Parse HTTP attributes if this is an HTTP handler
    let (http_methods, http_path) = if is_http {
        parse_http_attributes(method)
    } else {
        (Vec::new(), None)
    };

    FunctionMetadata {
        name: ident,
        variant_name,
        params,
        return_type,
        is_async: method.sig.asyncness.is_some(),
        is_local,
        is_remote,
        is_http,
        http_methods,
        http_path,
    }
}

/// Check if a method contains a call to init_logging
fn contains_init_logging(method: &syn::ImplItemFn) -> bool {
    let mut contains_logging = false;

    // Visitor to find init_logging calls
    struct LoggingVisitor {
        found: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for LoggingVisitor {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = &*call.func {
                if path
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident == "init_logging")
                    .unwrap_or(false)
                {
                    self.found = true;
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
    }

    // Visit the method body to find init_logging calls
    let mut visitor = LoggingVisitor { found: false };
    syn::visit::visit_block(&mut visitor, &method.block);

    visitor.found
}

//------------------------------------------------------------------------------
// Enum Generation Functions
//------------------------------------------------------------------------------

/// Generate Request and Response enums based on function metadata
fn generate_request_response_enums(
    function_metadata: &[FunctionMetadata],
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    if function_metadata.is_empty() {
        return (quote! {}, quote! {});
    }

    // HPMRequest enum variants - ONLY include handlers that have parameters
    // Parameter-less handlers are dispatched directly in Phase 1, not through enum deserialization
    let request_variants = function_metadata
        .iter()
        //.filter(|func| !func.params.is_empty()) // Only include handlers with parameters
        .map(|func| {
            let variant_name = format_ident!("{}", &func.variant_name);
            generate_enum_variant(&variant_name, &func.params)
        });

    // HPMResponse enum variants - include ALL handlers since they all need to return responses
    let response_variants = function_metadata.iter().map(|func| {
        let variant_name = format_ident!("{}", &func.variant_name);

        if let Some(return_type) = &func.return_type {
            let type_str = return_type.to_token_stream().to_string();
            if type_str == "()" {
                // Unit variant for () return type
                quote! { #variant_name }
            } else {
                // Tuple variant with return type
                quote! { #variant_name(#return_type) }
            }
        } else {
            // Unit variant for no explicit return
            quote! { #variant_name }
        }
    });

    // Generate the enum definitions with serialization derives
    (
        quote! {
            #[derive(Debug, serde::Serialize, serde::Deserialize)]
            enum HPMRequest {
                #(#request_variants),*
            }
        },
        quote! {
            #[derive(Debug, serde::Serialize, serde::Deserialize)]
            enum HPMResponse {
                #(#response_variants),*
            }
        },
    )
}

/// Generate a token stream for an enum variant based on parameter types
fn generate_enum_variant(
    variant_name: &syn::Ident,
    params: &[syn::Type],
) -> proc_macro2::TokenStream {
    if params.is_empty() {
        // Changed to a struct variant with no fields for functions with no parameters
        // This matches the JSON format {"VariantName": {}} sent by the client
        quote! { #variant_name }
    } else if params.len() == 1 {
        // Simple tuple variant for single parameter
        let param_type = &params[0];
        quote! { #variant_name(#param_type) }
    } else {
        // Tuple variant with multiple types for multiple parameters
        quote! { #variant_name(#(#params),*) }
    }
}

//------------------------------------------------------------------------------
// Handler Generation Functions
//------------------------------------------------------------------------------

/// Generate handler match arms for request handling
fn generate_handler_dispatch(
    handlers: &[&FunctionMetadata],
    self_ty: &Box<syn::Type>,
    handler_type: HandlerType,
) -> proc_macro2::TokenStream {
    if handlers.is_empty() {
        let message = match handler_type {
            HandlerType::Local => "No local handlers defined but received a local request",
            HandlerType::Remote => "No remote handlers defined but received a remote request",
            HandlerType::Http => "No HTTP handlers defined but received an HTTP request",
        };
        return quote! {
            hyperware_process_lib::logging::warn!(#message);
        };
    }

    let type_name = match handler_type {
        HandlerType::Local => "local",
        HandlerType::Remote => "remote",
        HandlerType::Http => "http",
    };

    let dispatch_arms = handlers
        .iter()
        .map(|func| generate_handler_dispatch_arm(func, self_ty, handler_type, type_name));

    // Add an explicit unreachable for other variants
    let unreachable_arm = quote! {
        _ => unreachable!(concat!("Non-", #type_name, " request variant received in ", #type_name, " handler"))
    };

    quote! {
        match request {
            #(#dispatch_arms)*
            #unreachable_arm
        }
    }
}

/// Generate a match arm for a specific handler
fn generate_handler_dispatch_arm(
    func: &FunctionMetadata,
    self_ty: &Box<syn::Type>,
    handler_type: HandlerType,
    type_name: &str,
) -> proc_macro2::TokenStream {
    let fn_name = &func.name;
    let variant_name = format_ident!("{}", &func.variant_name);

    // Get the appropriate response handling code
    let response_handling =
        generate_response_handling(func, &variant_name, handler_type, type_name);

    if func.is_async {
        generate_async_handler_arm(func, self_ty, fn_name, &variant_name, response_handling)
    } else {
        generate_sync_handler_arm(func, fn_name, &variant_name, response_handling)
    }
}

/// Generate response handling code based on handler type
fn generate_response_handling(
    _func: &FunctionMetadata,
    variant_name: &syn::Ident,
    handler_type: HandlerType,
    type_name: &str,
) -> proc_macro2::TokenStream {
    match handler_type {
        HandlerType::Local | HandlerType::Remote => {
            quote! {
                // Instead of wrapping in HPMResponse enum, directly serialize the result
                let resp = hyperware_process_lib::Response::new()
                    .body(serde_json::to_vec(&result).unwrap());
                resp.send().unwrap();
            }
        }
        HandlerType::Http => {
            quote! {
                // Instead of wrapping in HPMResponse enum, directly serialize the result
                let response_bytes = serde_json::to_vec(&result).unwrap();

                // Get headers from the current HTTP context
                let headers_opt = hyperware_process_lib::hyperapp::APP_HELPERS.with(|helpers| {
                    helpers.borrow().current_http_context.as_ref().and_then(|ctx| {
                        if ctx.response_headers.is_empty() {
                            None
                        } else {
                            Some(ctx.response_headers.clone())
                        }
                    })
                });

                hyperware_process_lib::http::server::send_response(
                    hyperware_process_lib::http::StatusCode::OK,
                    headers_opt,
                    response_bytes
                );

                // Clear HTTP context immediately after sending the response
                hyperware_process_lib::hyperapp::clear_http_request_context();
            }
        }
    }
}

/// Generate a match arm for an async handler
fn generate_async_handler_arm(
    func: &FunctionMetadata,
    self_ty: &Box<syn::Type>,
    fn_name: &syn::Ident,
    variant_name: &syn::Ident,
    response_handling: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    if func.params.is_empty() {
        // Updated pattern to match struct variant with no fields
        quote! {
            HPMRequest::#variant_name{} => {
                // Create a raw pointer to state for use in the async block
                let state_ptr: *mut #self_ty = state;
                hyperware_process_lib::hyperapp::run_async! {
                    // Inside the async block, use the pointer to access state
                    let result = unsafe { (*state_ptr).#fn_name().await };
                    #response_handling
                }
            }
        }
    } else if func.params.len() == 1 {
        // Async function with a single parameter
        quote! {
            HPMRequest::#variant_name(param) => {
                let param_captured = param;  // Capture param before moving into async block
                // Create a raw pointer to state for use in the async block
                let state_ptr: *mut #self_ty = state;
                hyperware_process_lib::hyperapp::run_async! {
                    // Inside the async block, use the pointer to access state
                    let result = unsafe { (*state_ptr).#fn_name(param_captured).await };
                    #response_handling
                }
            }
        }
    } else {
        // Async function with multiple parameters
        let param_count = func.params.len();
        let param_names = (0..param_count).map(|i| format_ident!("param{}", i));
        let capture_statements = (0..param_count).map(|i| {
            let param = format_ident!("param{}", i);
            let captured = format_ident!("param{}_captured", i);
            quote! { let #captured = #param; }
        });
        let captured_names = (0..param_count).map(|i| format_ident!("param{}_captured", i));

        quote! {
            HPMRequest::#variant_name(#(#param_names),*) => {
                // Capture all parameters before moving into async block
                #(#capture_statements)*
                // Create a raw pointer to state for use in the async block
                let state_ptr: *mut #self_ty = state;
                hyperware_process_lib::hyperapp::run_async! {
                    // Inside the async block, use the pointer to access state
                    let result = unsafe { (*state_ptr).#fn_name(#(#captured_names),*).await };
                    #response_handling
                }
            }
        }
    }
}

/// Generate a match arm for a sync handler
fn generate_sync_handler_arm(
    func: &FunctionMetadata,
    fn_name: &syn::Ident,
    variant_name: &syn::Ident,
    response_handling: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    if func.params.is_empty() {
        // Updated pattern to match struct variant with no fields
        quote! {
            HPMRequest::#variant_name{} => {
                let result = unsafe { (*state).#fn_name() };
                #response_handling
            }
        }
    } else if func.params.len() == 1 {
        quote! {
            HPMRequest::#variant_name(param) => {
                let result = unsafe { (*state).#fn_name(param) };
                #response_handling
            }
        }
    } else {
        let param_count = func.params.len();
        let param_names = (0..param_count).map(|i| format_ident!("param{}", i));
        let param_names2 = param_names.clone();

        quote! {
            HPMRequest::#variant_name(#(#param_names),*) => {
                let result = unsafe { (*state).#fn_name(#(#param_names2),*) };
                #response_handling
            }
        }
    }
}

//------------------------------------------------------------------------------
// Component Generation Functions
//------------------------------------------------------------------------------

/// Convert optional init method to token stream for identifier
fn init_method_opt_to_token(init_method: &Option<syn::Ident>) -> proc_macro2::TokenStream {
    if let Some(method_name) = init_method {
        quote! { Some(stringify!(#method_name)) }
    } else {
        quote! { None::<&str> }
    }
}

/// Convert optional init method to token stream for method call
fn init_method_opt_to_call(
    init_method: &Option<syn::Ident>,
    self_ty: &Box<syn::Type>,
) -> proc_macro2::TokenStream {
    if let Some(method_name) = init_method {
        quote! {
            // Create a pointer to state for use in the async block
            let state_ptr: *mut #self_ty = &mut state;
            hyperware_process_lib::hyperapp::run_async! {
                // Inside the async block, use the pointer to access state
                unsafe { (*state_ptr).#method_name().await };
            }
        }
    } else {
        quote! {}
    }
}

/// Convert optional WebSocket method to token stream for identifier
fn ws_method_opt_to_token(ws_method: &Option<syn::Ident>) -> proc_macro2::TokenStream {
    if let Some(method_name) = ws_method {
        quote! { Some(stringify!(#method_name)) }
    } else {
        quote! { None::<&str> }
    }
}

/// Convert optional WebSocket method to token stream for method call
fn ws_method_opt_to_call(ws_method: &Option<syn::Ident>) -> proc_macro2::TokenStream {
    if let Some(method_name) = ws_method {
        quote! { unsafe { (*state).#method_name(channel_id, message_type, blob) }; }
    } else {
        quote! {}
    }
}

//------------------------------------------------------------------------------
// HTTP Helper Functions
//------------------------------------------------------------------------------

/// Generate HTTP context setup code
fn generate_http_context_setup() -> proc_macro2::TokenStream {
    quote! {
        hyperware_process_lib::hyperapp::APP_HELPERS.with(|helpers| {
            helpers.borrow_mut().current_http_context = Some(hyperware_process_lib::hyperapp::HttpRequestContext {
                request: http_request,
                response_headers: std::collections::HashMap::new(),
            });
        });
        hyperware_process_lib::logging::debug!("HTTP context established");
    }
}

/// Generate HTTP context cleanup code
fn generate_http_context_cleanup() -> proc_macro2::TokenStream {
    quote! {
        hyperware_process_lib::hyperapp::clear_http_request_context();
    }
}

/// Generate HTTP error response handling
fn generate_http_error_response(
    status: &str,
    message: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let status_ident = format_ident!("{}", status);
    let cleanup = generate_http_context_cleanup();
    quote! {
        hyperware_process_lib::http::server::send_response(
            hyperware_process_lib::http::StatusCode::#status_ident,
            None,
            #message.into_bytes()
        );
        #cleanup
    }
}

/// Generate HTTP method and path parsing code
fn generate_http_request_parsing() -> proc_macro2::TokenStream {
    quote! {
        let http_method = hyperware_process_lib::hyperapp::get_http_method()
            .unwrap_or_else(|| {
                hyperware_process_lib::logging::warn!("Failed to get HTTP method from request context");
                "UNKNOWN".to_string()
            });

        let current_path = match hyperware_process_lib::hyperapp::get_path() {
            Some(path) => {
                hyperware_process_lib::logging::debug!("Successfully parsed HTTP path: '{}'", path);
                path
            },
            None => {
                hyperware_process_lib::logging::error!("Failed to get HTTP path: no HTTP context available");
                hyperware_process_lib::http::server::send_response(
                    hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                    None,
                    b"Invalid path: no HTTP context available".to_vec(),
                );
                hyperware_process_lib::hyperapp::clear_http_request_context();
                return;
            }
        };
    }
}

/// Generate parameterized handler dispatch arms
fn generate_parameterized_handler_dispatch(
    parameterized_handlers: &[&&FunctionMetadata],
    self_ty: &Box<syn::Type>,
    http_request_match_arms: &proc_macro2::TokenStream,
    specific_paths: &[&String],
) -> proc_macro2::TokenStream {
    let mut sorted_handlers = parameterized_handlers.to_vec();
    sorted_handlers.sort_by_key(|handler| handler.http_path.is_none());

    let dispatch_arms: Vec<_> = sorted_handlers.iter().map(|handler| {
        let fn_name = &handler.name;
        let variant_name = format_ident!("{}", &handler.variant_name);
        let path_check = if let Some(path) = &handler.http_path {
            quote! { &current_path == #path }
        } else {
            quote! { ![#(#specific_paths),*].contains(&current_path.as_str()) }
        };
        let methods = &handler.http_methods;
        let method_check = quote! { [#(#methods),*].contains(&http_method.as_str()) };

        quote! {
            hyperware_process_lib::logging::debug!("Checking parameterized handler {} for {} {} - path_check: {}, method_check: {}",
                stringify!(#fn_name), http_method, current_path, (#path_check), (#method_check));
            if #path_check && #method_check {
                hyperware_process_lib::logging::debug!("Matched parameterized handler {} for {} {}", stringify!(#fn_name), http_method, current_path);

                if let Some(ref blob) = blob_opt {
                    hyperware_process_lib::logging::debug!("Got blob with {} bytes: {}", blob.bytes.len(), String::from_utf8_lossy(&blob.bytes));
                    match serde_json::from_slice::<HPMRequest>(&blob.bytes) {
                        Ok(request) => {
                            match request {
                                HPMRequest::#variant_name(..) => {
                                    unsafe {
                                        #http_request_match_arms
                                        hyperware_process_lib::hyperapp::maybe_save_state(&mut *state);
                                    }
                                },
                                _ => {
                                    hyperware_process_lib::logging::error!("Request body contains wrong handler name for {} {}", http_method, current_path);
                                    hyperware_process_lib::http::server::send_response(
                                        hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                                        None,
                                        format!("Expected handler name '{}' in request body", stringify!(#variant_name)).into_bytes()
                                    );
                                }
                            }
                        },
                        Err(e) => {
                            let error_details = if blob.bytes.is_empty() {
                                "Request body is empty but was expected to contain handler parameters.".to_string()
                            } else if let Ok(json_value) = serde_json::from_slice::<serde_json::Value>(&blob.bytes) {
                                format!(
                                    "Invalid request format. Expected one of the parameterized handler formats, but got: {}",
                                    serde_json::to_string(&json_value).unwrap_or_else(|_| "invalid JSON".to_string())
                                )
                            } else {
                                format!(
                                    "Invalid JSON in request body. Parse error: {}",
                                    e
                                )
                            };

                            hyperware_process_lib::logging::error!("Failed to parse request body for {} {}: {}", http_method, current_path, error_details);

                            hyperware_process_lib::http::server::send_response(
                                hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                                None,
                                error_details.into_bytes()
                            );
                            hyperware_process_lib::hyperapp::clear_http_request_context();
                            return;
                        }
                    }
                } else {
                    hyperware_process_lib::logging::error!("Handler {} requires a request body", stringify!(#fn_name));
                    hyperware_process_lib::http::server::send_response(
                        hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                        None,
                        format!("Handler {} requires a request body", stringify!(#fn_name)).into_bytes()
                    );
                }
                return;
            }
        }
    }).collect();

    quote! { #(#dispatch_arms)* }
}

/// Generate parameterless handler dispatch arms
fn generate_parameterless_handler_dispatch(
    parameterless_handlers: &[&&FunctionMetadata],
    self_ty: &Box<syn::Type>,
    specific_paths: &[&String],
) -> proc_macro2::TokenStream {
    let mut sorted_handlers = parameterless_handlers.to_vec();
    sorted_handlers.sort_by_key(|handler| handler.http_path.is_none());

    let dispatch_arms: Vec<_> = sorted_handlers.iter().map(|handler| {
        let fn_name = &handler.name;
        let path_check = if let Some(path) = &handler.http_path {
            quote! { &current_path == #path }
        } else {
            quote! { ![#(#specific_paths),*].contains(&current_path.as_str()) }
        };
        let methods = &handler.http_methods;
        let method_check = quote! { [#(#methods),*].contains(&http_method.as_str()) };

        let response_handling = quote! {
            let response_bytes = match serde_json::to_vec(&result) {
                Ok(bytes) => bytes,
                Err(e) => {
                    hyperware_process_lib::logging::error!("Failed to serialize response: {}", e);
                    hyperware_process_lib::http::server::send_response(
                        hyperware_process_lib::http::StatusCode::INTERNAL_SERVER_ERROR,
                        None,
                        "Failed to serialize response".as_bytes().to_vec(),
                    );
                    return;
                }
            };

            let headers_opt = hyperware_process_lib::hyperapp::APP_HELPERS.with(|helpers| {
                helpers.borrow().current_http_context.as_ref().and_then(|ctx| {
                    if ctx.response_headers.is_empty() {
                        None
                    } else {
                        Some(ctx.response_headers.clone())
                    }
                })
            });

            hyperware_process_lib::http::server::send_response(
                hyperware_process_lib::http::StatusCode::OK,
                headers_opt,
                response_bytes
            );

            hyperware_process_lib::hyperapp::clear_http_request_context();
        };

        let handler_body = if handler.is_async {
            quote! {
                let state_ptr: *mut #self_ty = state;
                hyperware_process_lib::hyperapp::run_async! {
                    let result = unsafe { (*state_ptr).#fn_name().await };
                    #response_handling
                }
                unsafe { hyperware_process_lib::hyperapp::maybe_save_state(&mut *state); }
            }
        } else {
            quote! {
                let result = unsafe { (*state).#fn_name() };
                #response_handling
                unsafe { hyperware_process_lib::hyperapp::maybe_save_state(&mut *state); }
            }
        };

        quote! {
            hyperware_process_lib::logging::debug!("Checking parameter-less handler {} for {} {} - path_check: {}, method_check: {}",
                stringify!(#fn_name), http_method, current_path, (#path_check), (#method_check));
            if #path_check && #method_check {
                hyperware_process_lib::logging::debug!("Matched parameter-less handler {} for {} {}", stringify!(#fn_name), http_method, current_path);
                #handler_body
                return;
            }
        }
    }).collect();

    quote! { #(#dispatch_arms)* }
}

/// Generate HTTP handler dispatcher
fn generate_http_handler_dispatcher(
    http_handlers: &[&FunctionMetadata],
    self_ty: &Box<syn::Type>,
    http_request_match_arms: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let specific_paths: Vec<_> = http_handlers
        .iter()
        .filter_map(|h| h.http_path.as_ref())
        .collect();

    let parameterized_handlers: Vec<_> = http_handlers
        .iter()
        .filter(|h| !h.params.is_empty())
        .collect();

    let parameterless_handlers: Vec<_> = http_handlers
        .iter()
        .filter(|h| h.params.is_empty())
        .collect();

    let parameterized_dispatch = generate_parameterized_handler_dispatch(
        &parameterized_handlers,
        self_ty,
        http_request_match_arms,
        &specific_paths,
    );

    let parameterless_dispatch =
        generate_parameterless_handler_dispatch(&parameterless_handlers, self_ty, &specific_paths);

    quote! {
        hyperware_process_lib::logging::debug!("Starting handler matching for {} {}", http_method, current_path);

        if blob_opt.is_some() && !blob_opt.as_ref().unwrap().bytes.is_empty() {
            hyperware_process_lib::logging::debug!("Request has body, using two-phase matching");

            if let Some(ref blob) = blob_opt {
                match serde_json::from_slice::<HPMRequest>(&blob.bytes) {
                    Ok(request) => {
                        hyperware_process_lib::logging::debug!("Successfully parsed request body, dispatching to specific handler");
                        unsafe {
                            #http_request_match_arms
                            hyperware_process_lib::hyperapp::maybe_save_state(&mut *state);
                        }
                        return;
                    },
                    Err(e) => {
                        let error_details = if blob.bytes.is_empty() {
                            "Request body is empty but was expected to contain handler parameters.".to_string()
                        } else if let Ok(json_value) = serde_json::from_slice::<serde_json::Value>(&blob.bytes) {
                            format!(
                                "Invalid request format. Expected one of the parameterized handler formats, but got: {}",
                                serde_json::to_string(&json_value).unwrap_or_else(|_| "invalid JSON".to_string())
                            )
                        } else {
                            format!(
                                "Invalid JSON in request body. Parse error: {}",
                                e
                            )
                        };

                        hyperware_process_lib::logging::error!("Failed to parse request body for {} {}: {}", http_method, current_path, error_details);

                        hyperware_process_lib::http::server::send_response(
                            hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                            None,
                            error_details.into_bytes()
                        );
                        hyperware_process_lib::hyperapp::clear_http_request_context();
                        return;
                    }
                }
            }
        } else {
            hyperware_process_lib::logging::debug!("Request has no body, trying parameter-less handlers first");
            #parameterless_dispatch
        }

        hyperware_process_lib::logging::error!("No handler found for {} {} - all handlers checked", http_method, current_path);
        hyperware_process_lib::http::server::send_response(
            hyperware_process_lib::http::StatusCode::NOT_FOUND,
            None,
            format!("No handler found for {} {}", http_method, current_path).into_bytes(),
        );
        hyperware_process_lib::hyperapp::clear_http_request_context();
    }
}

//------------------------------------------------------------------------------
// WebSocket Helper Functions
//------------------------------------------------------------------------------

/// Generate WebSocket message handler
fn generate_websocket_handler(
    ws_method_call: &proc_macro2::TokenStream,
    self_ty: &Box<syn::Type>,
) -> proc_macro2::TokenStream {
    quote! {
        hyperware_process_lib::http::server::HttpServerRequest::WebSocketPush { channel_id, message_type } => {
            hyperware_process_lib::logging::debug!("Received WebSocket message on channel {}, type: {:?}", channel_id, message_type);

            let Some(blob) = blob_opt else {
                hyperware_process_lib::logging::error!(
                    "Failed to get blob for WebSocketPush on channel {}. This indicates a malformed WebSocket message.",
                    channel_id
                );
                return;
            };

            hyperware_process_lib::logging::debug!("Processing WebSocket message with {} bytes", blob.bytes.len());
            #ws_method_call

            unsafe {
                hyperware_process_lib::hyperapp::maybe_save_state(&mut *state);
            }
        },
        hyperware_process_lib::http::server::HttpServerRequest::WebSocketOpen { path, channel_id } => {
            hyperware_process_lib::logging::debug!("WebSocket connection opened on path '{}' with channel {}", path, channel_id);
            match hyperware_process_lib::hyperapp::get_server() {
                Some(server) => server.handle_websocket_open(&path, channel_id),
                None => hyperware_process_lib::logging::error!("Failed to get server instance for WebSocket open event")
            }
        },
        hyperware_process_lib::http::server::HttpServerRequest::WebSocketClose(channel_id) => {
            hyperware_process_lib::logging::debug!("WebSocket connection closed on channel {}", channel_id);
            match hyperware_process_lib::hyperapp::get_server() {
                Some(server) => server.handle_websocket_close(channel_id),
                None => hyperware_process_lib::logging::error!("Failed to get server instance for WebSocket close event")
            }
        }
    }
}

//------------------------------------------------------------------------------
// Local/Remote Message Helper Functions
//------------------------------------------------------------------------------

/// Generate local message handler
fn generate_local_message_handler(
    self_ty: &Box<syn::Type>,
    match_arms: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    quote! {
        /// Handle local messages
        fn handle_local_message(state: *mut #self_ty, message: hyperware_process_lib::Message) {
            hyperware_process_lib::logging::debug!("Processing local message from: {:?}", message.source());
            match serde_json::from_slice::<HPMRequest>(message.body()) {
                Ok(request) => {
                    unsafe {
                        #match_arms
                        hyperware_process_lib::hyperapp::maybe_save_state(&mut *state);
                    }
                },
                Err(e) => {
                    let raw_body = String::from_utf8_lossy(message.body());
                    hyperware_process_lib::logging::error!(
                        "Failed to deserialize local request into HPMRequest enum.\n\
                        Error: {}\n\
                        Source: {:?}\n\
                        Body: {}\n\
                        \n\
                        💡 This usually means the message format doesn't match any of your #[local] or #[remote] handlers.",
                        e, message.source(), raw_body
                    );
                }
            }
        }
    }
}

/// Generate remote message handler
fn generate_remote_message_handler(
    self_ty: &Box<syn::Type>,
    match_arms: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    quote! {
        /// Handle remote messages
        fn handle_remote_message(state: *mut #self_ty, message: hyperware_process_lib::Message) {
            hyperware_process_lib::logging::debug!("Processing remote message from: {:?}", message.source());
            match serde_json::from_slice::<HPMRequest>(message.body()) {
                Ok(request) => {
                    unsafe {
                        #match_arms
                        hyperware_process_lib::hyperapp::maybe_save_state(&mut *state);
                    }
                },
                Err(e) => {
                    let raw_body = String::from_utf8_lossy(message.body());
                    hyperware_process_lib::logging::error!(
                        "Failed to deserialize remote request into HPMRequest enum.\n\
                        Error: {}\n\
                        Source: {:?}\n\
                        Body: {}\n\
                        \n\
                        💡 This usually means the message format doesn't match any of your #[remote] handlers.",
                        e, message.source(), raw_body
                    );
                }
            }
        }
    }
}

/// Generate message handler functions for message types
fn generate_message_handlers(
    self_ty: &Box<syn::Type>,
    handler_arms: &HandlerDispatch,
    ws_method_call: &proc_macro2::TokenStream,
    http_handlers: &[&FunctionMetadata],
) -> proc_macro2::TokenStream {
    let http_request_match_arms = &handler_arms.http;
    let local_and_remote_request_match_arms = &handler_arms.local_and_remote;
    let remote_request_match_arms = &handler_arms.remote;

    let http_context_setup = generate_http_context_setup();
    let http_request_parsing = generate_http_request_parsing();
    let http_dispatcher =
        generate_http_handler_dispatcher(http_handlers, self_ty, http_request_match_arms);
    let websocket_handlers = generate_websocket_handler(ws_method_call, self_ty);
    let local_message_handler =
        generate_local_message_handler(self_ty, local_and_remote_request_match_arms);
    let remote_message_handler =
        generate_remote_message_handler(self_ty, remote_request_match_arms);

    quote! {
        /// Handle messages from the HTTP server
        fn handle_http_server_message(state: *mut #self_ty, message: hyperware_process_lib::Message) {
            let blob_opt = message.blob();

            match serde_json::from_slice::<hyperware_process_lib::http::server::HttpServerRequest>(message.body()) {
                Ok(http_server_request) => {
                    match http_server_request {
                        hyperware_process_lib::http::server::HttpServerRequest::Http(http_request) => {
                            hyperware_process_lib::logging::debug!("Processing HTTP request, message has blob: {}", blob_opt.is_some());
                            if let Some(ref blob) = blob_opt {
                                hyperware_process_lib::logging::debug!("Blob size: {} bytes, content: {}", blob.bytes.len(), String::from_utf8_lossy(&blob.bytes[..std::cmp::min(200, blob.bytes.len())]));
                            }

                            #http_context_setup
                            #http_request_parsing
                            #http_dispatcher
                        },
                        #websocket_handlers
                    }
                },
                Err(e) => {
                    hyperware_process_lib::logging::error!(
                        "Failed to parse HTTP server request: {}\n\
                        This usually indicates a malformed message to the HTTP server.",
                        e
                    );
                }
            }
        }

        #local_message_handler
        #remote_message_handler
    }
}

/// Helper function to determine if an Expr is "None"
fn is_none_literal(expr: &Expr) -> bool {
    if let Expr::Path(expr_path) = expr {
        if let Some(ident) = expr_path.path.get_ident() {
            return ident == "None";
        }
    }
    false
}

/// Generate the full component implementation
fn generate_component_impl(
    args: &HyperProcessArgs,
    self_ty: &Box<syn::Type>,
    cleaned_impl_block: &ItemImpl,
    request_enum: &proc_macro2::TokenStream,
    response_enum: &proc_macro2::TokenStream,
    init_method_details: &InitMethodDetails,
    ws_method_details: &WsMethodDetails,
    handler_arms: &HandlerDispatch,
    has_init_logging: bool,
    http_handlers: &[&FunctionMetadata],
) -> proc_macro2::TokenStream {
    // Extract values from args for use in the quote macro
    let name = &args.name;
    let endpoints = &args.endpoints;
    let save_config = &args.save_config;
    let wit_world = &args.wit_world;

    let icon = match &args.icon {
        Some(icon_str) => quote! { Some(#icon_str.to_string()) },
        None => quote! { None },
    };

    let widget = match &args.widget {
        Some(widget_str) => quote! { Some(#widget_str.to_string()) },
        None => quote! { None },
    };

    let ui = match &args.ui {
        Some(ui_expr) => {
            if is_none_literal(ui_expr) {
                quote! { None }
            } else {
                quote! { Some(#ui_expr) }
            }
        }
        None => quote! { None },
    };

    let init_method_ident = &init_method_details.identifier;
    let init_method_call = &init_method_details.call;
    let ws_method_call = &ws_method_details.call;

    // Generate message handler functions
    let message_handlers =
        generate_message_handlers(self_ty, handler_arms, ws_method_call, http_handlers);

    // Generate the logging initialization conditionally
    let logging_init = if !has_init_logging {
        quote! {
            // Initialize logging
            hyperware_process_lib::logging::init_logging(
                hyperware_process_lib::logging::Level::DEBUG,
                hyperware_process_lib::logging::Level::INFO,
                None, Some((0, 0, 1, 1)), None
            ).unwrap();
        }
    } else {
        // Empty if init_method already does logging initialization
        quote! {}
    };

    quote! {
        wit_bindgen::generate!({
            path: "../target/wit",
            world: #wit_world,
            generate_unused_types: true,
            additional_derives: [serde::Deserialize, serde::Serialize, process_macros::SerdeJsonInto],
        });

        use hyperware_process_lib::http::server::HttpBindingConfig;
        use hyperware_process_lib::http::server::WsBindingConfig;
        use hyperware_process_lib::hyperapp::Binding;

        #cleaned_impl_block

        // Add our generated request/response enums
        #request_enum
        #response_enum

        #message_handlers

        struct Component;
        impl Guest for Component {
            fn init(_our: String) {
                // Initialize our state
                let mut state = hyperware_process_lib::hyperapp::initialize_state::<#self_ty>();

                // Set to persist state according to user setting
                hyperware_process_lib::hyperapp::APP_CONTEXT.with(|ctx| {
                    ctx.borrow_mut().hidden_state = Some(
                        hyperware_process_lib::hyperapp::HiddenState::new(#save_config)
                    );
                });

                // Set up necessary components
                let app_name = #name;
                let app_icon = #icon;
                let app_widget = #widget;
                let ui_config = #ui;
                let endpoints = #endpoints;

                // Setup UI if needed
                if app_icon.is_some() && app_widget.is_some() {
                    hyperware_process_lib::homepage::add_to_homepage(app_name, app_icon, Some("/"), app_widget);
                }

                #logging_init

                // Setup server with endpoints
                let mut server = hyperware_process_lib::hyperapp::setup_server(ui_config.as_ref(), &endpoints);
                hyperware_process_lib::hyperapp::APP_HELPERS.with(|ctx| {
                    ctx.borrow_mut().current_server = Some(&mut server);
                });

                // Initialize app state
                if #init_method_ident.is_some() {
                    #init_method_call
                }

                // Main event loop
                loop {
                    hyperware_process_lib::hyperapp::APP_CONTEXT.with(|ctx| {
                        ctx.borrow_mut().executor.poll_all_tasks();
                    });

                    match hyperware_process_lib::await_message() {
                        Ok(message) => {
                            hyperware_process_lib::hyperapp::APP_HELPERS.with(|ctx| {
                                ctx.borrow_mut().current_message = Some(message.clone());
                            });

                            // Store old state if needed (for OnDiff save option)
                            // This only stores if old_state is None (first time or after a save)
                            hyperware_process_lib::hyperapp::store_old_state(&state);

                            match message {
                                hyperware_process_lib::Message::Response { body, context, .. } => {
                                    let correlation_id = context
                                        .as_deref()
                                        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                                        .unwrap_or_else(|| "no context".to_string());

                                    hyperware_process_lib::hyperapp::RESPONSE_REGISTRY.with(|registry| {
                                        let mut registry_mut = registry.borrow_mut();
                                        registry_mut.insert(correlation_id, body);
                                    });
                                }
                                hyperware_process_lib::Message::Request { .. } => {
                                    if message.is_local() && message.source().process == "http-server:distro:sys" {
                                        handle_http_server_message(&mut state, message);
                                    } else if message.is_local() {
                                        handle_local_message(&mut state, message);
                                    } else {
                                        handle_remote_message(&mut state, message);
                                    }
                                }
                            }
                        },
                        Err(ref error) => {
                            if let hyperware_process_lib::SendError {
                                context: Some(context),
                                ..
                            } = error
                            {
                                let correlation_id = String::from_utf8_lossy(context)
                                    .to_string();

                                hyperware_process_lib::hyperapp::RESPONSE_REGISTRY.with(|registry| {
                                    let mut registry_mut = registry.borrow_mut();
                                    registry_mut.insert(correlation_id, serde_json::to_vec(error).unwrap());
                                });
                            }

                        }
                    }
                }
            }
        }

        export!(Component);
    }
}

//------------------------------------------------------------------------------
// Main Macro Implementation
//------------------------------------------------------------------------------

/// The main procedural macro
#[proc_macro_attribute]
pub fn hyperprocess(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the input
    let attr_args = parse_macro_input!(attr as MetaList);
    let impl_block = parse_macro_input!(item as ItemImpl);

    // Parse the macro arguments
    let args = match parse_args(attr_args) {
        Ok(args) => args,
        Err(e) => return e.to_compile_error().into(),
    };

    // Get the self type from the implementation block
    let self_ty = &impl_block.self_ty;

    // Analyze the methods in the implementation block
    let (init_method, ws_method, function_metadata, has_init_logging) =
        match analyze_methods(&impl_block) {
            Ok(methods) => methods,
            Err(e) => return e.to_compile_error().into(),
        };

    // Filter functions by handler type
    let handlers = HandlerGroups::from_function_metadata(&function_metadata);

    // HTTP handlers with parameters will be part of the HPMRequest enum and dispatched via body deserialization.
    let http_handlers_with_params: Vec<_> = handlers
        .http
        .iter()
        //.filter(|h| !h.params.is_empty())
        .cloned()
        .collect();

    // Collect all function metadata that will be represented in the HPMRequest enum.
    // This includes all local and remote handlers, plus HTTP handlers that have parameters.
    let metadata_for_enum: Vec<_> = function_metadata
        .iter()
        //.filter(|f| !f.is_http || !f.params.is_empty())
        .cloned()
        .collect();

    // Generate HPMRequest and HPMResponse enums from the filtered list of functions
    let (request_enum, response_enum) = generate_request_response_enums(&metadata_for_enum);

    // Generate handler match arms
    let handler_arms = HandlerDispatch {
        local: generate_handler_dispatch(&handlers.local, self_ty, HandlerType::Local),
        remote: generate_handler_dispatch(&handlers.remote, self_ty, HandlerType::Remote),
        // HTTP dispatch arms are only generated for handlers with parameters.
        http: generate_handler_dispatch(&http_handlers_with_params, self_ty, HandlerType::Http),
        // Generate dispatch for combined local and remote handlers
        local_and_remote: generate_handler_dispatch(
            &handlers.local_and_remote,
            self_ty,
            HandlerType::Local,
        ),
    };

    // Clean the implementation block
    let cleaned_impl_block = clean_impl_block(&impl_block);

    // Prepare init method details for code generation
    let init_method_details = InitMethodDetails {
        identifier: init_method_opt_to_token(&init_method),
        call: init_method_opt_to_call(&init_method, self_ty),
    };

    // Prepare WebSocket method details for code generation
    let ws_method_details = WsMethodDetails {
        identifier: ws_method_opt_to_token(&ws_method),
        call: ws_method_opt_to_call(&ws_method),
    };

    // Generate the final output
    generate_component_impl(
        &args,
        self_ty,
        &cleaned_impl_block,
        &request_enum,
        &response_enum,
        &init_method_details,
        &ws_method_details,
        &handler_arms,
        has_init_logging,
        &handlers.http,
    )
    .into()
}
