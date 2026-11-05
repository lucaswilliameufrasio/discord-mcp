# Discord MCP

> Em desenvolvimento. Não possui interface web nem autenticação e não deve ser
> utilizado como serviço de produção.

Servidor MCP em Rust planejado para integrar canais do Discord por webhook ou
bot. Webhooks permitem apenas envio; bots podem enviar e consultar mensagens
conforme as permissões concedidas no Discord.

## Estado atual

O protocolo stdio oferece envio para um webhook configurado na variável de
ambiente `DISCORD_WEBHOOK_URL`; mensagens não permitem menções automáticas. O
modo servidor, OAuth, controle de acesso, gestão guiada de segredos, CLI
interativa, bot e interfaces administrativas ainda precisam ser implementados.

Também é possível cadastrar múltiplos webhooks em SQLite local e selecionar o
canal pelo nome no argumento `channel` da ferramenta `send_message`. Configure
`DISCORD_MCP_DB` com o caminho do banco e `DISCORD_MCP_KEY` com uma chave
aleatória de 32 bytes codificada em hexadecimal, fornecida pelo gerenciador de
segredos do ambiente. Mantenha a chave e sua cópia de recuperação separadas do
banco: sem ela não é possível recuperar os webhooks. A chave em variável de
ambiente pode ser acessível a processos privilegiados ou ao próprio usuário;
ela não substitui um gerenciador de segredos. Não inclua chave nem webhook em
arquivos versionados, argumentos de processo ou histórico do shell.

`discord-mcp add-channel NOME` recebe a URL do webhook na entrada padrão, sem
exibi-la. Use uma entrada segura e não a digite em terminal com eco habilitado.
`discord-mcp list-channels` lista apenas nomes. O arquivo de banco novo é criado
com permissões restritas no Unix; um arquivo existente com permissões de grupo
ou outros usuários é recusado. Essas opções são provisórias: a interface web e
o fluxo guiado de gerenciamento de chaves ainda não existem.

## Desenvolvimento

Requer Rust e Cargo. Execute `cargo run` para iniciar o protocolo stdio. Este
comando não abre uma interface web e é destinado a clientes MCP. Configure a
variável de ambiente por um mecanismo seguro do cliente, sem colocar a URL em
argumentos, histórico do shell ou arquivos versionados.

Verificações: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo build` e `cargo test`.

Nunca registre tokens de bot, URLs de webhook ou segredos OAuth no repositório.
