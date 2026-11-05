# Discord MCP

> Em desenvolvimento. Não possui interface web nem autenticação e não deve ser
> utilizado como serviço de produção.

Servidor MCP em Rust planejado para integrar canais do Discord por webhook ou
bot. Webhooks permitem apenas envio; bots podem enviar e consultar mensagens
conforme as permissões concedidas no Discord.

## Estado atual

O protocolo stdio oferece envio para um webhook configurado na variável de
ambiente `DISCORD_WEBHOOK_URL`; mensagens não permitem menções automáticas. O
modo servidor, OAuth, controle de acesso, persistência segura, CLI interativa,
bot e interfaces administrativas ainda precisam ser implementados.

## Desenvolvimento

Requer Rust e Cargo. Execute `cargo run` para iniciar o protocolo stdio. Este
comando não abre uma interface web e é destinado a clientes MCP. Configure a
variável de ambiente por um mecanismo seguro do cliente, sem colocar a URL em
argumentos, histórico do shell ou arquivos versionados.

Verificações: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo build` e `cargo test`.

Nunca registre tokens de bot, URLs de webhook ou segredos OAuth no repositório.
