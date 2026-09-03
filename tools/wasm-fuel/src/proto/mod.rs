//! Zellij's plugin-API protobuf schema, as prost-generated Rust.
//!
//! Vendored verbatim from `zellij-utils` 0.44.3, `assets/prost/api.*.rs`
//! (MIT). Only the modules the harness needs to *encode* events, pipe
//! messages, and the `GetPluginIds` reply are here; `plugin_command.rs`
//! (what the plugin sends back) is decoded via the tiny [`command`] module
//! below instead of its 115 KB generated form, because the harness only
//! needs the command's `name`.
//!
//! Why vendor rather than depend on `zellij-utils`: its native build links
//! libcurl (via `isahc`), which is not available in every dev environment,
//! and the crate takes minutes to compile. The `zellij-tile` pin in
//! `crates/plugin/Cargo.toml` is the schema version to re-vendor from.

#![allow(clippy::all, dead_code, unused_imports)]

pub mod api {
    pub mod action {
        include!("action.rs");
    }
    pub mod event {
        include!("event.rs");
    }
    pub mod input_mode {
        include!("input_mode.rs");
    }
    pub mod key {
        include!("key.rs");
    }
    pub mod pipe_message {
        include!("pipe_message.rs");
    }
    pub mod plugin_ids {
        include!("plugin_ids.rs");
    }
    pub mod resize {
        include!("resize.rs");
    }
    pub mod style {
        include!("style.rs");
    }
}

/// The head of `ProtobufPluginCommand` and the one reply shape the plugin
/// blocks on besides `PluginIds`. Field numbers match `api.plugin_command.rs`
/// (`PluginCommand.name` is tag 1; prost skips the unknown payload tags).
pub mod command {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Head {
        #[prost(int32, tag = "1")]
        pub name: i32,
    }

    /// `CommandName` discriminants the harness answers or recognizes.
    pub const GET_PLUGIN_IDS: i32 = 3;
    pub const GET_ZELLIJ_VERSION: i32 = 4;
    pub const SET_TIMEOUT: i32 = 12;
    pub const GET_PANE_CWD: i32 = 191;

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct GetPaneCwdResponse {
        #[prost(oneof = "get_pane_cwd_response::Result", tags = "1, 2")]
        pub result: ::core::option::Option<get_pane_cwd_response::Result>,
    }
    pub mod get_pane_cwd_response {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub enum Result {
            #[prost(string, tag = "1")]
            Cwd(::prost::alloc::string::String),
            #[prost(string, tag = "2")]
            Error(::prost::alloc::string::String),
        }
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ZellijVersion {
        #[prost(string, tag = "1")]
        pub version: ::prost::alloc::string::String,
    }
}
