# Hyperware Process Framework

![How it feels to use Hyperware](hyperware.jpg)
How it feels to use hyperware


## Table of Contents

- [Part 1: User Guide](#part-1-user-guide)
  - [Overview](#overview)
  - [Getting Started](#getting-started)
  - [State Management](#state-management)
  - [Hyperprocess Macro Parameters](#hyperprocess-macro-parameters)
  - [Handler Types](#handler-types)
  - [Special Methods](#special-methods)
    - [Init Method](#init-method)
    - [WebSocket Handler](#websocket-handler)
  - [Binding Endpoints](#binding-endpoints)
    - [HTTP Configuration](#http-binding-configuration)
    - [WebSocket Configuration](#websocket-binding-configuration)
  - [Persistence Options](#persistence-options)
  - [Example Application](#example-application)
- [Part 2: Technical Implementation](#part-2-technical-implementation)
  - [Architecture Overview](#architecture-overview)
  - [Macro Implementation](#macro-implementation)
    - [Parsing Phase](#1-parsing-phase)
    - [Metadata Collection](#2-metadata-collection)
    - [Code Generation](#3-code-generation)
  - [Request/Response Flow](#requestresponse-flow)
  - [Async Runtime](#async-runtime)
    - [ResponseFuture Implementation](#responsefuture-implementation)
    - [Correlation System](#correlation-system)
  - [Executor and Task Management](#executor-and-task-management)
  - [Handler Generation](#handler-generation)
    - [Request and Response Enum Generation](#request-and-response-enum-generation)
    - [Handler Dispatch Generation](#handler-dispatch-generation)
  - [Error Handling Strategy](#error-handling-strategy)
  - [WebAssembly Integration](#webassembly-integration)
    - [WIT Bindings Generation](#wit-bindings-generation)
    - [Component Implementation](#component-implementation)

## Part 1: User Guide

### Overview

This is a process framework abstracting away most of the boilerplate for developing hyperware processes. It unlocks async support by implementing a custom async runtime, and in conjunction with [hyper-bindgen](https://github.com/hyperware-ai/hyper-bindgen), it allows the automatic generation of wit files from defined function endpoints, as well as functions stubs in `caller-utils` in order to be able to have a process asynchronously call another endpoint in another process as if it were a function.

RPC style, but for WASI.

So this includes:

- Defining functions as endpoints (http, remote, local, ws and init)
- Async support
- Automated state persistence with different options

### Getting Started

To create a Hyperware process, you need to:

1. Define your process state as a struct
2. Implement the struct with the `hyperprocess` macro
3. Define handlers for different types of requests

Here's a minimal example:

```rust
#[derive(Default, Debug, Serialize, Deserialize)]
struct MyProcessState {
    counter: u64,
}

#[hyperprocess(
    name = "My Process",
    ui = Some(HttpBindingConfig::default()),
    endpoints = vec![
        Binding::Http { 
            path: "/api", 
            config: HttpBindingConfig::new(false, false, false, None) 
        }
    ],
    save_config = SaveOptions::EveryMessage,
    wit_world = "my-process-dot-os-v0"
)]
impl MyProcessState {
    #[init]
    async fn initialize(&mut self) {
        // Initialize your process
    }
    
    #[http]
    async fn handle_http_request(&mut self, value: String) -> String {
        self.counter += 1;
        format!("Request processed. Counter: {}", self.counter)
    }
}
```

### State Management

Your state should implement the `Default` and `State` traits, and be serializable with `serde`.

### Hyperprocess Macro Parameters

The `hyperprocess` macro accepts the following parameters:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | String | Yes | Human-readable name of your process |
| `icon` | String | No | Icon to display in UI |
| `widget` | String | No | Widget type to display in UI |
| `ui` | Option\<HttpBindingConfig\> | Yes | UI configuration |
| `endpoints` | Vec\<Binding\> | Yes | HTTP and WebSocket endpoints |
| `save_config` | SaveOptions | Yes | When to persist state |
| `wit_world` | String | Yes | WIT world name for component model |

Example:

```rust
#[hyperprocess(
    name = "Async Requester",
    ui = Some(HttpBindingConfig::default()),
    endpoints = vec![
        Binding::Http {
            path: "/api",
            config: HttpBindingConfig::new(false, false, false, None),
        },
        Binding::Ws {
            path: "/ws",
            config: WsBindingConfig::new(false, false, false),
        }
    ],
    save_config = SaveOptions::EveryMessage,
    wit_world = "async-app-template-dot-os-v0"
)]
```

### Handler Types

Hyperware processes can handle three types of requests, specified by attributes:

| Attribute | Description |
|-----------|-------------|
| `#[local]` | Handles local (same-node) requests |
| `#[remote]` | Handles remote (cross-node) requests |
| `#[http]` | Handles HTTP requests to your process endpoints |

These attributes can be combined to make a handler respond to multiple request types:

```rust
#[local]
#[http]
async fn increment_counter(&mut self, value: i32) -> i32 {
    self.counter += value;
    self.counter
}

#[remote]
fn get_status(&mut self) -> String {
    format!("Status: {}", self.counter)
}
```

The function arguments and the return values _have_ to be serializable with `Serde`.

### Special Methods

#### Init Method

To run code on process startup, define:

```rust
#[init]
async fn initialize(&mut self) {
    // Initialization code
}
```

#### WebSocket Handler

For defining a `ws` endpoint, do:

```rust
#[ws]
fn handle_websocket(&mut self, channel_id: u32, message_type: WsMessageType, blob: LazyLoadBlob) {
    // Process WebSocket messages
}
```

if you have multiple ws endpoints, you can match on the ws endpoints with `get_path()`, which will give you an `Option<String>`.
if you want to access the http server, you can call `get_server()`, giving you access to `HttpServer`.

### Binding Endpoints

The `endpoints` parameter configures HTTP and WebSocket endpoints:

```rust
endpoints = vec![
    Binding::Http {
        path: "/api",
        config: HttpBindingConfig::new(false, false, false, None),
    },
    Binding::Ws {
        path: "/ws",
        config: WsBindingConfig::new(false, false, false),
    }
]
```

### Persistence Options

The `save_config` parameter controls when to persist state:

```rust
save_config = SaveOptions::EveryMessage
```

Available options:

| Option | Description |
|--------|-------------|
| `SaveOptions::Never` | Never persist state |
| `SaveOptions::EveryMessage` | Persist after every message |
| `SaveOptions::EveryNMessage(n)` | Persist every n messages |
| `SaveOptions::EveryNSeconds(n)` | Persist every n seconds |

### Example Application

```rust
#[derive(Default, Debug, Serialize, Deserialize)]
struct AsyncRequesterState {
    request_count: u64,
}

#[hyperprocess(
    name = "Async Requester",
    ui = Some(HttpBindingConfig::default()),
    endpoints = vec![
        Binding::Http {
            path: "/api",
            config: HttpBindingConfig::new(false, false, false, None),
        }, 
        Binding::Ws {
            path: "/ws",
            config: WsBindingConfig::new(false, false, false),
        }
    ],
    save_config = SaveOptions::EveryMessage,
    wit_world = "async-app-template-dot-os-v0"
)]
impl AsyncRequesterState {
    #[init]
    async fn initialize(&mut self) {
        // Initialize and make async calls to other processes
        let result = call_to_other_process().await;
    }

    #[http]
    async fn process_request(&mut self, value: i32) -> String {
        self.request_count += 1;
        "Response from process".to_string()
    }

    #[local]
    #[remote]
    fn get_count(&mut self) -> u64 {
        self.request_count
    }

    #[ws]
    fn websocket(&mut self, channel_id: u32, message_type: WsMessageType, blob: LazyLoadBlob) {
        // Process WebSocket messages
    }
}
```

If you want to call a function from another process, you run `hyper-bindgen` (see [hyper-bindgen repo](https://github.com/hyperware-ai/hyper-bindgen)), which will automatically generate `wit` files in `/api`, and a `caller-utils` rust project. If you import it like so, you'll be able to call any endpoint as an async function!

```rust
use caller_utils::async_requester::increment_counter_remote_rpc;
use shared::receiver_address;

async fn my_function() {
    let result = increment_counter_remote_rpc(&receiver_address(), 42, "test".to_string()).await;
    match result {
        SendResult::Success(value) => println!("Got result: {}", value),
        SendResult::Error(err) => println!("Error: {}", err),
    }
}
```

## Part 2: Technical Implementation

### Architecture Overview

Barebones hyperware processes use erlang style message passing. When wanting to send messages asynchronously, you had to send off a request, but handle the response in the response handler residing in another part of the code, adding a context switching cost. Being able to call things asynchronously makes things much more linear and easier to read and process, both for humans and LMs.

This was achieved by implementing our own async runtime. Given that processes are always single threaded, and the only real event occurs is when a message (either a request or response) is read with `await_message()`, we managed to implement a runtime through callbacks and other tricks.

### Macro Implementation

The `hyperprocess` macro transforms a struct implementation into a fully-featured process:

#### 1. Parsing Phase

The macro will parse arguments like so:

```rust
fn parse_args(attr_args: MetaList) -> syn::Result<HyperProcessArgs> {
    // Parse attributes like name, icon, endpoints, etc.
    // Validate required parameters
    
    Ok(HyperProcessArgs {
        name: name.ok_or_else(|| syn::Error::new(span, "Missing 'name'"))?,
        icon,
        widget,
        ui,
        endpoints: endpoints.ok_or_else(|| /* error */)?,
        save_config: save_config.ok_or_else(|| /* error */)?,
        wit_world: wit_world.ok_or_else(|| /* error */)?,
    })
}
```

It also checks the method signatures:

```rust
fn validate_init_method(method: &syn::ImplItemFn) -> syn::Result<()> {
    // Ensure method is async
    if method.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "Init method must be declared as async",
        ));
    }
    
    // Check parameter and return types
    // ...
}
```

#### 2. Metadata Collection

It then builds metadata:

```rust
fn analyze_methods(impl_block: &ItemImpl) -> syn::Result<(
    Option<syn::Ident>,    // init method
    Option<syn::Ident>,    // ws method
    Vec<FunctionMetadata>, // request/response methods
)> {
    let mut init_method = None;
    let mut ws_method = None;
    let mut function_metadata = Vec::new();

    for item in &impl_block.items {
        if let syn::ImplItem::Fn(method) = item {
            // Check method attributes and process accordingly
            if has_attribute(method, "init") {
                // Process init method
            } else if has_attribute(method, "ws") {
                // Process WebSocket method
            } else if has_http || has_local || has_remote {
                // Process handler methods
                function_metadata.push(extract_function_metadata(
                    method, has_local, has_remote, has_http,
                ));
            }
        }
    }

    Ok((init_method, ws_method, function_metadata))
}
```

#### 3. Code Generation

Under the hood, everything is still regular hyperware message passing, with the body being either a `Request` or `Response` enum. Whenever you define a new function/endpoint, it generates appropriate request and response variants, with the name of the function being the variant in CamelCase.

The inner values of the request variants will be the function arguments as tuples, the inner valus of the response variants will be return value of the defined function.

```rust
fn generate_component_impl(...) -> proc_macro2::TokenStream {
    quote! {
        // Generate WIT bindings
        wit_bindgen::generate!({...});

        // Include user's implementation
        #cleaned_impl_block

        // Add generated request/response enums
        #request_enum
        #response_enum

        // Add message handler functions
        #message_handlers

        // Create and export component
        struct Component;
        impl Guest for Component {...}
        export!(Component);
    }
}
```

### Request/Response Flow

The flow of a request through the system:

1. Message arrives (HTTP, local, or remote)
2. Main event loop deserializes it into a Request enum
3. Appropriate handler is dispatched based on message type
4. For async handlers, the future is spawned on the executor
5. When handler completes, response is serialized and sent back
6. For async handlers, awaiting futures are resumed with the response

### Async Runtime

Here is how the async runtime works on a high level.

#### ResponseFuture Implementation

Core type that suspends execution until a response arrives:

```rust
struct ResponseFuture {
    correlation_id: String,
}

impl Future for ResponseFuture {
    type Output = Vec<u8>;

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        let correlation_id = &self.correlation_id;

        // Check if response has arrived in registry
        let maybe_bytes = RESPONSE_REGISTRY.with(|registry| {
            let mut registry_mut = registry.borrow_mut();
            registry_mut.remove(correlation_id)
        });

        if let Some(bytes) = maybe_bytes {
            // Response found - resume execution
            Poll::Ready(bytes)
        } else {
            // Response not yet received - suspend
            Poll::Pending
        }
    }
}
```

#### Correlation System

The correlation system works by generating a unique correlation ID (UUID) for each request and attaching it to outgoing requests in the context field. Responses are stored in RESPONSE_REGISTRY keyed by their correlation ID. The ResponseFuture polls RESPONSE_REGISTRY until the matching response arrives.

This design enables async handlers to suspend execution while waiting for responses, with multiple requests able to be in flight simultaneously. When responses come back, they can be correctly routed to the handler that is awaiting them based on the correlation ID. The system also supports timeouts by tracking how long responses have been pending.

The implementation uses thread locals to avoid synchronization overhead, since the process runs in a single-threaded environment. This lock-free approach keeps the correlation system lightweight and efficient while maintaining the ability to track request/response pairs reliably.

```rust
pub async fn send<R>(
    message: impl serde::Serialize,
    target: &Address,
    timeout_secs: u64,
) -> SendResult<R>
where
    R: serde::de::DeserializeOwned,
{
    // Generate unique correlation ID
    let correlation_id = Uuid::new_v4().to_string();
    
    // Send request with correlation ID
    let _ = Request::to(target)
        .body(serde_json::to_vec(&message).unwrap())
        .context(correlation_id.as_bytes().to_vec())
        .expects_response(timeout_secs)
        .send();

    // Await response with matching correlation ID
    let response_bytes = ResponseFuture::new(correlation_id).await;
    
    // Process response...
}
```

### Executor and Task Management

The executor manages async tasks within the single-threaded environment:

```rust
pub struct Executor {
    tasks: Vec<Pin<Box<dyn Future<Output = ()>>>>,
}

impl Executor {
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    pub fn spawn(&mut self, fut: impl Future<Output = ()> + 'static) {
        self.tasks.push(Box::pin(fut));
    }

    pub fn poll_all_tasks(&mut self) {
        let mut ctx = Context::from_waker(noop_waker_ref());
        let mut completed = Vec::new();

        // Poll all tasks
        for i in 0..self.tasks.len() {
            if let Poll::Ready(()) = self.tasks[i].as_mut().poll(&mut ctx) {
                completed.push(i);
            }
        }

        // Remove completed tasks in reverse order
        for idx in completed.into_iter().rev() {
            let _ = self.tasks.remove(idx);
        }
    }
}
```

The executor is polled in the main event loop, right before we await a message. So if a response comes back, we make sure that everything is properly 'linked'.

```rust
loop {
    // Poll tasks between message handling
    APP_CONTEXT.with(|ctx| {
        ctx.borrow_mut().executor.poll_all_tasks();
    });
    
    // Wait for next message (blocking)
    match await_message() {
        // Process message...
    }
}
```

### Handler Generation

The macro generates specialized code for each handler method.

#### Request and Response Enum Generation

The macro extracts parameter and return types from each method:

```rust
fn extract_function_metadata(method: &syn::ImplItemFn, is_local: bool, is_remote: bool, is_http: bool) -> FunctionMetadata {
    // Extract parameter types (skipping &mut self)
    let params = method.sig.inputs.iter().skip(1)
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
        name: method.sig.ident.clone(),
        variant_name,
        params,
        return_type,
        is_async: method.sig.asyncness.is_some(),
        is_local,
        is_remote,
        is_http,
    }
}
```

For example, given these handlers:

```rust
#[http]
async fn get_user(&mut self, id: u64) -> User { ... }

#[local]
#[remote]
fn update_settings(&mut self, settings: Settings, apply_now: bool) -> bool { ... }
```

The macro generates these enums:

```rust
enum Request {
    GetUser(u64),
    UpdateSettings(Settings, bool),
}

enum Response {
    GetUser(User),
    UpdateSettings(bool),
}
```

#### Handler Dispatch Generation

For each handler, the macro generates dispatch code:

**Async Handler Example**:

```rust
Request::FetchData(id) => {
    let id_captured = id;  // Capture parameter before moving
    let state_ptr: *mut MyState = state; 
    
    hyper! {
        let result = unsafe { (*state_ptr).fetch_data(id_captured).await };
        
        // For remote/local handlers
        let resp = Response::new()
            .body(serde_json::to_vec(&result).unwrap());
        resp.send().unwrap();
    }
}
```

The `hyper!` macro lets our custom runtime execute this async code.

#### WIT Bindings Generation

We parse the `wit_world` in our `/api` folder with:

```rust
wit_bindgen::generate!({
    path: "target/wit",
    world: #wit_world,
    generate_unused_types: true,
    additional_derives: [
        serde::Deserialize, 
        serde::Serialize, 
        process_macros::SerdeJsonInto
    ],
});
```

Note: The `wit` files will always get generated with `hyper-bindgen`, which you have to call before `kit b`. More info [in the hyper-bindgen repository](https://github.com/hyperware-ai/hyper-bindgen).
```rust
struct Component;
impl Guest for Component {
    fn init(_our: String) {
        // Initialize state
        let mut state = initialize_state::<#self_ty>();

        // Set up server and UI
        let app_name = #name;
        let app_icon = #icon;
        let app_widget = #widget;
        let ui_config = #ui;
        let endpoints = #endpoints;

        // Setup UI if needed
        if app_icon.is_some() && app_widget.is_some() {
            homepage::add_to_homepage(app_name, app_icon, Some("/"), app_widget);
        }

        // Initialize logging
        logging::init_logging(...);

        // Setup server with endpoints
        let mut server = setup_server(ui_config.as_ref(), &endpoints);
        
        // Call user's init method if provided
        if #init_method_ident.is_some() {
            #init_method_call
        }

        // Main event loop
        loop {
            // Poll pending async tasks
            APP_CONTEXT.with(|ctx| {
                ctx.borrow_mut().executor.poll_all_tasks();
            });

            // Wait for next message and handle it
            match await_message() {
                // Message handling...
            }
        }
    }
}

```


Here is an overly complex llm generated graph that looks cool.

```mermaid
graph TB
    %% STYLE DEFINITIONS
    classDef default fill:#333333,color:#ffffff,stroke:#444444,stroke-width:1px
    classDef accent fill:#FF6600,color:#ffffff,stroke:#CC5500,stroke-width:2px
    classDef mainflow fill:#444444,color:#ffffff,stroke:#666666,stroke-width:2px
    classDef asyncflow fill:#2A2A2A,color:#ffffff,stroke:#444444,stroke-width:1px
    classDef external fill:#222222,color:#ffffff,stroke:#444444,stroke-width:1px
    classDef dataflow fill:#008CBA,color:#ffffff,stroke:#0077A3,stroke-width:1px
    classDef annotation fill:none,color:#FF6600,stroke:none,stroke-width:0px
    
    %% BUILD PHASE - Where components are generated
    subgraph BuildPhase["⚙️ BUILD PHASE"]
        UserSrc[/"User Source Code
        #[hyperprocess] macro
        #[http], #[local], #[remote] methods"/]
        
        subgraph CodeGen["Code Generation Pipeline"]
            direction TB
            
            HyperBindgen["hyper-bindgen CLI
            Scans for #[hyperprocess]"]
            
            subgraph BindgenOutputs["hyper-bindgen Outputs"]
                direction LR
                WitFiles["WIT Files
                WebAssembly Interfaces"]
                CallerUtils["caller-utils Crate
                Type-safe RPC Stubs"]
                EnumStructs["Shared Enums & Structs
                Cross-process types"]
            end
            
            ProcMacro["hyperprocess Macro
            AST Transformation"]
            
            subgraph MacroOutputs["Macro Generated Code"]
                direction LR
                ReqResEnums["Request/Response Enums
                - Generated variants per handler
                - Parameter & return mappings"]
                
                HandlerDisp["Handler Dispatch Logic
                - HTTP/Local/Remote routing
                - Async handler spawning
                - Message serialization"]
                
                AsyncRuntime["Async Runtime Components
                - ResponseFuture impl
                - Correlation ID system
                - Executor & task management"]
                
                MainLoop["Component Implementation
                - Message loop
                - Task polling
                - Error handling"]
            end
        end
        
        %% Dev-time Connections
        UserSrc --> HyperBindgen
        UserSrc --> ProcMacro
        HyperBindgen --> BindgenOutputs
        ProcMacro --> MacroOutputs
        
        %% Final Compilation
        MacroOutputs --> WasmComp["WebAssembly Component
        WASI Preview 2"]
        BindgenOutputs --> WasmComp
    end
    
    %% RUNTIME PHASE - How processes execute
    subgraph RuntimePhase["⚡ RUNTIME PHASE"]
        subgraph Process["Process A"]
            direction TB
            
            InMsg[/"Incoming Messages"/] --> MsgLoop["Message Loop
            await_message()"]
            
            subgraph ProcessInternals["Process Internals"]
                direction LR
                
                MsgLoop --> MsgRouter{"Message Router"}
                MsgRouter -->|"HTTP"| HttpHandler["HTTP Handlers"]
                MsgRouter -->|"Local"| LocalHandler["Local Handlers"]
                MsgRouter -->|"Remote"| RemoteHandler["Remote Handlers"]
                MsgRouter -->|"WebSocket"| WsHandler["WebSocket Handlers"]
                MsgRouter -->|"Response"| RespHandler["Response Handler"]
                
                %% State management
                HttpHandler & LocalHandler & RemoteHandler & WsHandler --> AppState[("Application State
                SaveOptions::EveryMessage")]
                
                %% Async handling
                RespHandler --> RespRegistry["Response Registry
                correlation_id → response"]
                
                CallStub["RPC Stub Calls
                e.g. increment_counter_rpc()"]
            end
            
            %% Asynchronous execution
            AppState -.->|"Persist"| Storage[(Persistent Storage)]
            
            MsgLoop -.->|"Poll Tasks"| Executor["Async Executor
            poll_all_tasks()"]
            
            ProcessInternals -->|"Generate"| OutMsg[/"Outgoing Messages"/]
        end
        
        %% External communication points
        ExtClient1["HTTP Client"] & ExtClient2["WebSocket Client"] --> InMsg
        OutMsg --> Process2["Process B"]
        Process2 --> InMsg
    end
    
    %% ASYNC FLOW - Detailed sequence of async communication
    subgraph AsyncFlow["⚡ ASYNC MESSAGE EXCHANGE"]
        direction LR
        
        AF1["1️⃣ Call RPC Stub
        increment_counter_rpc(target, 42)"] --> 
        AF2["2️⃣ Generate UUID
        correlation_id = uuid::new_v4()"] --> 
        AF3["3️⃣ Create Future
        ResponseFuture(correlation_id)"] -->
        AF4["4️⃣ Send Request
        context=correlation_id"] -->
        AF5["5️⃣ Target Process
        Handle request & generate result"] -->
        AF6["6️⃣ Send Response
        context=correlation_id"] -->
        AF7["7️⃣ Message Loop
        Receives response with correlation_id"] -->
        AF8["8️⃣ Response Registry
        Store response by correlation_id"] -->
        AF9["9️⃣ Future Polling
        ResponseFuture finds response and completes"]
    end
    
    %% KEY CONNECTIONS BETWEEN SECTIONS
    
    %% Build to Runtime
    WasmComp ==>|"Load Component"| Process
    
    %% Runtime to Async Flow
    CallStub ==>|"Initiates"| AF1
    AF9 ==>|"Resume Future in"| Executor
    RespRegistry ===|"Powers"| AF8
    
    %% Annotation for the Correlation ID system
    CorrelationNote["CORRELATION SYSTEM
    Tracks request→response with UUIDs"] -.-> RespRegistry
    
    %% Style elements
    class UserSrc,WitFiles,CallerUtils,EnumStructs,ReqResEnums,HandlerDisp,AsyncRuntime,MainLoop,WasmComp mainflow
    class MsgLoop,Executor,RespRegistry,RespHandler,AF2,AF8 accent
    class MsgRouter,HttpHandler,LocalHandler,RemoteHandler,WsHandler,CallStub,AppState dataflow
    class AF1,AF3,AF4,AF5,AF6,AF7,AF9 asyncflow
    class ExtClient1,ExtClient2,Process2,Storage,InMsg,OutMsg external
    class CorrelationNote annotation
    
    %% Subgraph styling
    style BuildPhase fill:#171717,stroke:#333333,color:#ffffff
    style CodeGen fill:#222222,stroke:#444444,color:#ffffff
    style BindgenOutputs fill:#2A2A2A,stroke:#444444,color:#ffffff
    style MacroOutputs fill:#2A2A2A,stroke:#444444,color:#ffffff
    style RuntimePhase fill:#171717,stroke:#333333,color:#ffffff
    style Process fill:#222222,stroke:#444444,color:#ffffff
    style ProcessInternals fill:#2A2A2A,stroke:#444444,color:#ffffff
    style AsyncFlow fill:#222222,stroke:#FF6600,color:#ffffff
```


## Todos

- Let the new kit templates make use of the new framework