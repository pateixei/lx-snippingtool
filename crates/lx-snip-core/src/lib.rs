//! Módulo: `lx_snip_core`
//! Objetivos: Fornecer a biblioteca central do `lx-snippingtool` (configuração, histórico em disco, cena de anotações, exportação destrutiva, traits de plataforma e telemetria).
//! Principais itens: `config`, `export`, `platform`, `scene`, `storage` e `telemetry`.
//! Última alteração: 2026-11-19

#![warn(missing_docs)]

pub mod config;
pub mod export;
pub mod platform;
pub mod scene;
pub mod storage;
pub mod telemetry;
