# Padrão Obrigatório de Documentação (`lx-snippingtool`)

Todo arquivo-fonte Rust (`.rs`) alterado ou criado neste repositório **DEVE** seguir e atualizar o padrão de documentação abaixo a cada edição:

## 1. Cabeçalho do Arquivo (`//!` na linha 1)

```rust
//! Módulo: `<nome do módulo>`
//! Objetivos: <objetivos principais e responsabilidade do módulo>.
//! Principais itens: <structs, enums, traits e funções principais>.
//! Última alteração: YYYY-MM-DD
```

- O campo `Última alteração` **deve** ser atualizado para a data da edição sempre que o arquivo for modificado.

## 2. Estruturas, Enums e Traits (`///`)

```rust
/// <Uma linha descrevendo por que o tipo existe e qual responsabilidade cumpre.>
/// Armazena: <campos ou variantes principais> | Principais métodos: <principais métodos ou "nenhum (tipo de dados)">.
```

## 3. Funções e Métodos (`///`, públicos e privados, exceto `#[test]`)

```rust
/// <Uma linha descrevendo o que a função faz.>
/// Input: <parâmetros de entrada> | Output: <valor de retorno ou efeito colateral>.
```

## 4. Regras de Qualidade e Verificação

- `lx-snip-core` mantém `#![warn(missing_docs)]` ativo: qualquer item público sem documentação gera erro no Clippy (`-D warnings`).
- Envolva nomes de tipos/caminhos entre crases `` ` `` (evite `[referência]` sem crases para não gerar avisos do `rustdoc`).
- Antes de concluir qualquer alteração, execute e garanta saída zero em:
  - `cargo fmt --check`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo doc --workspace --no-deps`
