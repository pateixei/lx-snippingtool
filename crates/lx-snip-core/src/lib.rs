//! Core library for `lx-snippingtool`.
//!
//! Provides configuration management, bounded PNG snip storage, retained-mode
//! annotation scenes, destructive rasterization/export, platform abstraction
//! traits, and structured telemetry initialization.

pub mod config;
pub mod export;
pub mod platform;
pub mod scene;
pub mod storage;
pub mod telemetry;
