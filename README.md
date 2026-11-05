# Discord MCP

> Em desenvolvimento. O modo HTTP é experimental, não foi validado com o
> Discord real e não deve ser utilizado como serviço de produção.

Servidor MCP em Rust planejado para integrar canais do Discord por webhook ou
bot. Webhooks permitem apenas envio; bots podem enviar e consultar mensagens
conforme as permissões concedidas no Discord.

## Estado atual

O protocolo stdio oferece envio para um webhook configurado na variável de
ambiente `DISCORD_WEBHOOK_URL`; mensagens não permitem menções automáticas. O
modo servidor HTTP possui OAuth e controle de acesso experimentais; a gestão
guiada de segredos ainda falta.

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
ou outros usuários é recusado. O fluxo guiado de gerenciamento de chaves ainda
não existe.

### Interface web local

Com as variáveis de banco e chave configuradas, execute `discord-mcp web` em
outro terminal. O comando apresenta o endereço em `127.0.0.1` e um código de
uso único válido por cinco minutos. Abra o endereço informado e insira o código
para cadastrar, substituir ou remover canais. A sessão vale enquanto o processo
estiver ativo; reinicie-o para gerar outro código. Não compartilhe o código.

A interface valida o host e a origem de formulários, usa cookie HttpOnly com
SameSite estrito e não exibe webhooks já cadastrados. É uma interface local,
não um servidor acessível remotamente. Ela roda em processo separado do MCP
stdio e utiliza o mesmo banco. Não protege contra outro processo com os mesmos
privilégios do usuário que consiga ler a chave ou controlar o navegador.

### Servidor HTTP experimental

O comando `discord-mcp serve` inicia OAuth e MCP HTTP usando o mesmo banco de
canais. O processo só aceita escutar em endereço de loopback; não o exponha
diretamente em uma interface pública. O servidor ainda não foi validado com o
Discord real nem preparado para produção. Para acesso remoto é necessário um
proxy HTTPS configurado pelo operador, que preserve o `Host` público correto e
proteja logs de URLs de callback, códigos OAuth, cookies e cabeçalhos de
autorização. O proxy e sua configuração não são fornecidos aqui.

Além de `DISCORD_MCP_DB` e `DISCORD_MCP_KEY`, configure:

| Variável | Finalidade |
| --- | --- |
| `DISCORD_MCP_LISTEN` | Endereço de loopback; padrão `127.0.0.1:8000`. |
| `DISCORD_MCP_PUBLIC_ORIGIN` | Origem pública HTTPS, sem caminho; HTTP só em loopback na porta do listener. |
| `DISCORD_MCP_DISCORD_CLIENT_ID` | ID da aplicação OAuth Discord. |
| `DISCORD_MCP_DISCORD_CLIENT_SECRET` | Segredo OAuth Discord, fornecido por mecanismo de segredos. |
| `DISCORD_MCP_ADMIN_ID` | ID numérico da conta Discord autorizada a administrar permissões. |
| `DISCORD_MCP_CLIENT_ID` | ID de um cliente MCP previamente registrado pelo operador. |
| `DISCORD_MCP_CLIENT_NAME` | Nome do cliente mostrado no consentimento. |
| `DISCORD_MCP_CLIENT_REDIRECT` | URI de retorno exata desse cliente. |

Registre no Discord as URIs de callback formadas por sua origem pública e
`/oauth/discord/callback` e `/admin/discord/callback`. O recurso MCP será a
origem seguida de `/mcp`.
Esta versão aceita apenas **um cliente MCP pré-registrado**; registro dinâmico,
documentos de metadados de cliente ainda não estão disponíveis.

O operador local define permissões com
`discord-mcp set-permission ID_DISCORD CANAL read|send|both|none` e consulta
com `discord-mcp show-permissions CANAL`. O ID é o identificador numérico da
conta Discord, não o nome exibido. Não há concessão automática no login. A
substituição ou exclusão do canal revoga suas permissões; webhooks não aceitam
permissão de leitura. Os comandos exigem acesso local ao banco e à chave.

A área `/admin` permite à conta Discord cujo ID corresponde a
`DISCORD_MCP_ADMIN_ID` consultar e alterar permissões por canal. O login de
qualquer outra conta não concede administração. A sessão administrativa expira
após 30 minutos e termina ao reiniciar o servidor ou sair; mudanças exigem uma
requisição da mesma origem. A criação de canais ainda é feita na interface local.

Em `SIGINT` ou `SIGTERM`, o servidor tenta drenar requisições por até 15
segundos. Ao exceder o limite, operações em andamento podem ficar incompletas;
isso não garante entrega de mensagens ou recuperação após interrupção forçada.

### Canais com bot

Na interface local, informe o nome, o ID do canal e o token de um bot já
instalado no servidor Discord. O token é cifrado no mesmo banco e não é exibido
novamente. `send_message` envia pelo bot ou webhook; `read_messages` só aceita
canais de bot e retorna até 20 mensagens recentes. O bot precisa das permissões
Discord adequadas, incluindo `VIEW_CHANNEL`, `SEND_MESSAGES` e
`READ_MESSAGE_HISTORY` para as respectivas operações. A ausência de
`READ_MESSAGE_HISTORY` pode resultar em lista vazia; não prova que o canal não
tem mensagens. O Discord também pode retornar conteúdo vazio quando o intent
privilegiado `MESSAGE_CONTENT` não está configurado ou aprovado. Esta versão
não acompanha mensagens em tempo real e ainda não foi validada contra o Discord
real.

## Desenvolvimento

Requer Rust e Cargo. Execute `cargo run` para iniciar o protocolo stdio. Este
comando não abre uma interface web e é destinado a clientes MCP. Configure a
variável de ambiente por um mecanismo seguro do cliente, sem colocar a URL em
argumentos, histórico do shell ou arquivos versionados.

Verificações: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo build` e `cargo test`.

Nunca registre tokens de bot, URLs de webhook ou segredos OAuth no repositório.
