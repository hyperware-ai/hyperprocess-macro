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
struct FunctionMetadata {
    name: syn::Ident,               // Original function name
    variant_name: String,           // CamelCase variant name
    params: Vec<syn::Type>,         // Parameter types (excluding &mut self)
    return_type: Option<syn::Type>, // Return type (None for functions returning ())
    is_async: bool,                 // Whether function is async
    is_local: bool,                 // Has #[local] attribute
    is_remote: bool,                // Has #[remote] attribute
    is_http: bool,                  // Has #[http] attribute
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
)> {
    let mut init_method = None;
    let mut ws_method = None;
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
                function_metadata.push(extract_function_metadata(
                    method, has_local, has_remote, has_http,
                ));
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

    Ok((init_method, ws_method, function_metadata))
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

    FunctionMetadata {
        name: ident,
        variant_name,
        params,
        return_type,
        is_async: method.sig.asyncness.is_some(),
        is_local,
        is_remote,
        is_http,
    }
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

    // HPMRequest enum variants
    let request_variants = function_metadata.iter().map(|func| {
        let variant_name = format_ident!("{}", &func.variant_name);
        generate_enum_variant(&variant_name, &func.params)
    });

    // HPMResponse enum variants
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
        quote! { #variant_name{} }
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
                hyperware_process_lib::http::server::send_response(
                    hyperware_process_lib::http::StatusCode::OK,
                    None,
                    response_bytes
                );
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
                hyperware_app_common::hyper! {
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
                hyperware_app_common::hyper! {
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
                hyperware_app_common::hyper! {
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
            hyperware_app_common::hyper! {
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

/// Generate handler functions for message types
fn generate_message_handlers(
    self_ty: &Box<syn::Type>,
    handler_arms: &HandlerDispatch,
    ws_method_call: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let http_request_match_arms = &handler_arms.http;
    let local_request_match_arms = &handler_arms.local;
    let remote_request_match_arms = &handler_arms.remote;
    // We now use the combined local_and_remote handlers for local messages
    let local_and_remote_request_match_arms = &handler_arms.local_and_remote;

    quote! {
        /// Handle messages from the HTTP server
        fn handle_http_server_message(state: *mut #self_ty, message: hyperware_process_lib::Message) {
            // Parse HTTP server request
            match serde_json::from_slice::<hyperware_process_lib::http::server::HttpServerRequest>(message.body()) {
                Ok(http_server_request) => {
                    match http_server_request {
                        hyperware_process_lib::http::server::HttpServerRequest::Http(http_request) => {
                            hyperware_app_common::APP_CONTEXT.with(|ctx| {
                                ctx.borrow_mut().current_path = Some(http_request.path().clone().expect("Failed to get path from HTTP request"));
                            });

                            // Get the blob containing the actual request
                            let Some(blob) = message.blob() else {
                                hyperware_process_lib::logging::warn!("Failed to get blob for HTTP, sending BAD_REQUEST");
                                hyperware_process_lib::http::server::send_response(
                                    hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                                    None,
                                    vec![]
                                );
                                return;
                            };

                            // Process HTTP request
                            match serde_json::from_slice::<serde_json::Value>(blob.bytes()) {
                                Ok(req_value) => {
                                    match serde_json::from_value::<HPMRequest>(req_value.clone()) {
                                        Ok(request) => {
                                            // Handle the HTTP request
                                            unsafe {
                                                #http_request_match_arms

                                                // Save state if needed
                                                hyperware_app_common::maybe_save_state(&mut *state);
                                            }
                                        },
                                        Err(e) => {
                                            hyperware_process_lib::logging::warn!("Failed to deserialize HTTP request into HPMRequest enum: {}", e);
                                            hyperware_process_lib::http::server::send_response(
                                                hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                                                None,
                                                format!("Invalid request format: {}", e).into_bytes()
                                            );
                                        }
                                    }
                                },
                                Err(e) => {
                                    hyperware_process_lib::logging::warn!("Failed to parse HTTP request as JSON: {}", e);
                                    hyperware_process_lib::http::server::send_response(
                                        hyperware_process_lib::http::StatusCode::BAD_REQUEST,
                                        None,
                                        format!("Invalid JSON: {}", e).into_bytes()
                                    );
                                }
                            }
                            hyperware_app_common::APP_CONTEXT.with(|ctx| {
                                ctx.borrow_mut().current_path = None;
                            });
                        },
                        hyperware_process_lib::http::server::HttpServerRequest::WebSocketPush { channel_id, message_type } => {
                            let Some(blob) = message.blob() else {
                                hyperware_process_lib::logging::warn!("Failed to get blob for WebSocketPush, exiting");
                                return;
                            };

                            // Call the websocket handler if it exists
                            #ws_method_call

                            // Save state if needed
                            unsafe {
                                hyperware_app_common::maybe_save_state(&mut *state);
                            }
                        },
                        hyperware_process_lib::http::server::HttpServerRequest::WebSocketOpen { path, channel_id } => {
                            hyperware_app_common::get_server().unwrap().handle_websocket_open(&path, channel_id);
                        },
                        hyperware_process_lib::http::server::HttpServerRequest::WebSocketClose(channel_id) => {
                            hyperware_app_common::get_server().unwrap().handle_websocket_close(channel_id);
                        }
                    }
                },
                Err(e) => {
                    hyperware_process_lib::logging::warn!("Failed to parse HTTP server request: {}", e);
                }
            }
        }

        /// Handle local messages
        fn handle_local_message(state: *mut #self_ty, message: hyperware_process_lib::Message) {
            match serde_json::from_slice::<serde_json::Value>(message.body()) {
                Ok(req_value) => {
                    // Process the local request based on our handlers (now including both local and remote handlers)
                    match serde_json::from_value::<HPMRequest>(req_value.clone()) {
                        Ok(request) => {
                            unsafe {
                                // Match on the request variant and call the appropriate handler
                                // Now using combined local_and_remote handlers
                                #local_and_remote_request_match_arms

                                // Save state if needed
                                hyperware_app_common::maybe_save_state(&mut *state);
                            }
                        },
                        Err(e) => {
                            hyperware_process_lib::logging::warn!("Failed to deserialize local request into HPMRequest enum: {}", e);
                        }
                    }
                },
                Err(e) => {
                    hyperware_process_lib::logging::warn!("Failed to parse message body as JSON: {}", e);
                }
            }
        }

        /// Handle remote messages
        fn handle_remote_message(state: *mut #self_ty, message: hyperware_process_lib::Message) {
            match serde_json::from_slice::<serde_json::Value>(message.body()) {
                Ok(req_value) => {
                    // Process the remote request based on our handlers
                    match serde_json::from_value::<HPMRequest>(req_value.clone()) {
                        Ok(request) => {
                            unsafe {
                                // Match on the request variant and call the appropriate handler
                                #remote_request_match_arms

                                // Save state if needed
                                hyperware_app_common::maybe_save_state(&mut *state);
                            }
                        },
                        Err(e) => {
                            hyperware_process_lib::logging::warn!("Failed to deserialize remote request into HPMRequest enum: {}", e);
                            // Try to decode as UTF-8 for better debugging
                            hyperware_process_lib::logging::warn!("Raw request value: {:?}", req_value);
                        }
                    }
                },
                Err(e) => {
                    hyperware_process_lib::logging::warn!("Failed to parse message body as JSON: {}", e);
                }
            }
        }
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
) -> proc_macro2::TokenStream {
    // Extract values from args for use in the quote macro
    let name = &args.name;
    let endpoints = &args.endpoints;
    let _save_config = &args.save_config;
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
    let message_handlers = generate_message_handlers(self_ty, handler_arms, ws_method_call);

    quote! {
        thread_local! {
            static CURRENT_MESSAGE: std::cell::RefCell<Option<hyperware_process_lib::Message>> =
                std::cell::RefCell::new(None);
        }

        fn source() -> hyperware_process_lib::Address {
            CURRENT_MESSAGE.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .expect("No message in current context")
                    .source()
            })
        }

        wit_bindgen::generate!({
            path: "target/wit",
            world: #wit_world,
            generate_unused_types: true,
            additional_derives: [serde::Deserialize, serde::Serialize, process_macros::SerdeJsonInto],
        });

        use hyperware_process_lib::http::server::HttpBindingConfig;
        use hyperware_process_lib::http::server::WsBindingConfig;
        use hyperware_app_common::Binding;

        #cleaned_impl_block

        // Add our generated request/response enums
        #request_enum
        #response_enum

        #message_handlers

        struct Component;
        impl Guest for Component {
            fn init(_our: String) {
                // Initialize our state
                let mut state = hyperware_app_common::initialize_state::<#self_ty>();

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

                // Initialize logging
                hyperware_process_lib::logging::init_logging(
                    hyperware_process_lib::logging::Level::DEBUG,
                    hyperware_process_lib::logging::Level::INFO,
                    None, Some((0, 0, 1, 1)), None
                ).unwrap();

                // Setup server with endpoints
                let mut server = hyperware_app_common::setup_server(ui_config.as_ref(), &endpoints);
                hyperware_app_common::APP_CONTEXT.with(|ctx| {
                    ctx.borrow_mut().current_server = Some(&mut server);
                });

                // Initialize app state
                if #init_method_ident.is_some() {
                    #init_method_call
                }

                // Main event loop
                loop {
                    hyperware_app_common::APP_CONTEXT.with(|ctx| {
                        ctx.borrow_mut().executor.poll_all_tasks();
                    });

                    match hyperware_process_lib::await_message() {
                        Ok(message) => {
                            CURRENT_MESSAGE.with(|cell| {
                                *cell.borrow_mut() = Some(message.clone());
                            });
                            match message {
                                hyperware_process_lib::Message::Response {body, context, ..} => {
                                    // TODO: We need to update the callback handlers to make async work.
                                    let correlation_id = context
                                        .as_deref()
                                        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                                        .unwrap_or_else(|| "no context".to_string());

                                    hyperware_app_common::RESPONSE_REGISTRY.with(|registry| {
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
                        Err(error) => {
                            let kind = &error.kind;
                            let target = &error.target;
                            let body = String::from_utf8(error.message.body().to_vec())
                                .map(|s| format!("\"{}\"", s))
                                .unwrap_or_else(|_| format!("{:?}", error.message.body()));
                            let context = error
                                .context
                                .as_ref()
                                .map(|bytes| String::from_utf8_lossy(bytes).into_owned());

                            hyperware_process_lib::kiprintln!(
                                "SendError {{
                            kind: {:?},
                            target: {},
                            body: {},
                            context: {}
                        }}",
                                kind,
                                target,
                                body,
                                context
                                    .map(|s| format!("\"{}\"", s))
                                    .unwrap_or("None".to_string())
                            );

                            if let hyperware_process_lib::SendError {
                                kind,
                                context: Some(context),
                                ..
                            } = &error
                            {
                                // Convert context bytes to correlation_id string
                                if let Ok(correlation_id) = String::from_utf8(context.to_vec()) {
                                    // Serialize None as the response
                                    let none_response = serde_json::to_vec(kind).unwrap();

                                    hyperware_app_common::RESPONSE_REGISTRY.with(|registry| {
                                        let mut registry_mut = registry.borrow_mut();
                                        registry_mut.insert(correlation_id, none_response);
                                    });
                                }
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
    let (init_method, ws_method, function_metadata) = match analyze_methods(&impl_block) {
        Ok(methods) => methods,
        Err(e) => return e.to_compile_error().into(),
    };

    // Filter functions by handler type
    let handlers = HandlerGroups::from_function_metadata(&function_metadata);

    // Generate HPMRequest and HPMResponse enums
    let (request_enum, response_enum) = generate_request_response_enums(&function_metadata);

    // Generate handler match arms
    let handler_arms = HandlerDispatch {
        local: generate_handler_dispatch(&handlers.local, self_ty, HandlerType::Local),
        remote: generate_handler_dispatch(&handlers.remote, self_ty, HandlerType::Remote),
        http: generate_handler_dispatch(&handlers.http, self_ty, HandlerType::Http),
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
    )
    .into()
}
