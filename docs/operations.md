# Discord MCP

> O modo HTTP é experimental e ainda não foi validado com o Discord real.
> A decisão sobre seu uso cabe ao operador.

Servidor MCP em Rust para integrar canais do Discord por webhook ou
bot. Webhooks permitem apenas envio; bots podem enviar e consultar mensagens
conforme as permissões concedidas no Discord.

## Estado atual

O protocolo stdio oferece envio para um webhook configurado na variável de
ambiente `DISCORD_WEBHOOK_URL`; mensagens não permitem menções automáticas. O
modo servidor HTTP possui OAuth e controle de acesso experimentais. Para
instalação, consulte o [guia do servidor](server-setup.md).

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
ou outros usuários é recusado. A CLI também oferece comandos de preparação e
geração de chave; consulte `discord-mcp --help`.

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
Na remoção, a confirmação é vinculada à versão do canal exibida; se o canal
for substituído antes da confirmação, a interface recusa e pede nova revisão.
Nos formulários de cadastro, o estado dos canais é registrado quando a página
é carregada e verificado na transação de gravação. Se um canal for criado,
substituído ou removido nesse intervalo, a gravação é recusada. O formulário
vale por cinco minutos e é de uso único; carregar outra página autenticada
invalida formulários anteriores, inclusive em outras abas. Em caso de conflito,
volte à lista e informe a credencial novamente. O estado temporário contém nomes
e versões dos registros cifrados, não URLs nem tokens. A interface recusa gerar
o formulário se houver mais de 10.000 canais.

### Servidor HTTP experimental

Modelos alternativos para Linux com systemd ou Docker e seus limites estão em
[`docs/remote-runtime-options.md`](remote-runtime-options.md). Eles não
instalam serviços nem configuram backups; sua adequação ao ambiente deve ser
avaliada pelo operador.

O comando `discord-mcp serve` inicia OAuth e MCP HTTP usando o mesmo banco de
canais. O processo só aceita escutar em endereço de loopback; não o exponha
diretamente em uma interface pública. O servidor ainda não foi validado com o
Discord real. Para acesso remoto é necessário um
proxy HTTPS configurado pelo operador, que preserve o `Host` público correto e
proteja logs de URLs de callback, códigos OAuth, cookies e cabeçalhos de
autorização. O proxy e sua configuração não são fornecidos aqui.

No Linux, processos desta aplicação usam um lock consultivo no arquivo SQLite:
`serve` exige exclusividade, enquanto múltiplas instâncias stdio e comandos
locais podem coexistir. Outros processos desta aplicação recusam acesso enquanto
`serve` está ativo, e vice-versa. O lock é liberado ao encerrar o processo.
Ele **não é uma fronteira de segurança**: outras ferramentas SQLite podem
ignorá-lo, processos com acesso ao diretório podem substituir o arquivo, e o
mecanismo não é aplicado a outras plataformas. Restrinja arquivo, diretório e
chave ao serviço por isolamento de sistema operacional adequado. Esse lock
não habilita nem protege captura incremental ou recuperação operacional.

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
| `DISCORD_MCP_METRICS_RETENTION_DAYS` | Retenção de métricas, entre 1 e 365 dias; padrão 30. |
| `DISCORD_MCP_AUDIT_RETENTION_DAYS` | Retenção de auditoria, entre 1 e 3650 dias; padrão 90. |

Registre no Discord as URIs de callback formadas por sua origem pública e
`/oauth/discord/callback` e `/admin/discord/callback`. O recurso MCP será a
origem seguida de `/mcp`.
Esta versão aceita apenas **um cliente MCP pré-registrado**; registro dinâmico,
documentos de metadados de cliente ainda não estão disponíveis.

### Rotação operacional do segredo OAuth

Este é um procedimento orientativo, **não executado em produção**. A aplicação
carrega o segredo no início: alterar o provedor não atualiza processos ativos.
Cada instância e os clientes de login OAuth e administrativo precisam ser
reiniciados e verificados. O lock do SQLite não coordena instâncias que usem
bancos diferentes. A CLI não cria, valida nem revoga segredos no Discord.
Nesta versão, a verificação é feita pelo operador: não existe comprovação
automática de adoção por instância. Os indicadores locais de `rotation-status`
são observações, não identidade autenticada nem prova de validade atual.
O procedimento é para o servidor HTTP; clientes stdio locais não precisam
configurar o segredo OAuth do servidor, HTTP ou supervisor para usá-lo.

1. Identifique o emissor, provedor selecionado, banco, chave de abertura e todas
   as instâncias consumidoras. Defina responsável, janela, plano de recuperação
   e como verificar ambos os logins sem registrar segredos, códigos ou cookies.
   Não altere a chave SQLite nem apague fontes ou backups como parte da rotação.
2. Confirme no Discord e no provedor o procedimento suportado para emitir e
   substituir o segredo, inclusive se existe sobreposição. **Não presuma uma
   janela de validade simultânea**: se não houver, planeje indisponibilidade.
   Em comprometimento, revogue conforme a resposta a incidentes, mesmo que isso
   interrompa o serviço; nunca reative automaticamente uma credencial revogada.
3. Pare e confirme a saída dos consumidores que usam o banco antes de rotacionar
   o cofre local. Com `embedded`, revise `setup local vault rotate-oauth --preview`
   e use `--confirm` com segredo atual e substituto em duas linhas por stdin
   protegido não terminal. A comparação transacional impede sobrescrita por
   operador desatualizado. Para ambiente, systemd, Infisical ou AWS, publique a
   substituta pelo procedimento autorizado do respectivo provedor; a aplicação
   não escreve nem gira segredos nesses provedores.
4. Se a resposta da escrita se perder, interrompa a sequência. No cofre local,
   `setup local vault compare-oauth --confirm` compara um candidato fornecido por
   stdin protegido com o snapshot atual, sem divulgá-lo. Igualdade não identifica
   qual operação gravou o valor; diferença ou erro não provam rollback. Em
   provedores externos consulte a versão/estado pelo procedimento do próprio
   provedor, sem registrar valores. Não repita uma operação upstream ambígua
   sem garantia documentada de idempotência; preserve o estado incerto.
5. Reinicie cada consumidor usando apenas a fonte escolhida, sem a variável
   OAuth antiga quando outro provedor estiver selecionado. Verifique inicialização
   e faça novos logins OAuth e administrativo autorizados em cada instância;
   somente inicializar ou comparar valores **não prova adoção**. Registre apenas
   identidade não secreta da instância, versão/referência quando disponível,
   horário e resultado. Falha ou instância não verificada significa adoção
   incompleta: não declare conclusão nem remova a origem por suposição.
6. Após adoção comprovada, retire a autoridade antiga segundo o procedimento do
   emissor, quando ainda aplicável. Se a rotação foi por comprometimento, trate
   separadamente sessões administrativas e tokens MCP emitidos: a revogação do
   segredo OAuth não os invalida automaticamente. Determine revogação e impacto
   por instância e não prometa revogação global sem evidência.

Os testes locais de `serve_synthetic_authority` usam autoridade e credenciais
sintéticas em uma compilação instrumentada específica; não validam contas,
provedores, TLS público ou a operação de revogação no Discord real.

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
requisição da mesma origem. A visão geral apresenta contagens reais do banco no
momento da consulta: canais, usuários com concessões, permissões de leitura e
envio e tokens MCP não expirados. Tokens não expirados não são conexões ativas.
A criação de canais ainda é feita na interface local.

O formulário de permissões fica vinculado à versão do canal exibido. Se o canal
for substituído antes do envio, a alteração é recusada; volte à lista e confira
o canal novamente. A tabela de permissões e a versão exibidas são lidas do
mesmo snapshot do SQLite.

Alterações de permissão pela interface administrativa ou pela CLI remota também
verificam uma revisão por canal. Se outra alteração de permissões ocorrer desde
a abertura da página ou criação da solicitação, a edição é recusada e deve ser
refeita após revisão. Essa proteção é conservadora: alterações de usuários
diferentes no mesmo canal também podem conflitar; canais distintos têm revisões
independentes. Escritores externos e versões antigas da aplicação que não
atualizem a revisão não são cobertos; não os use no mesmo banco.

Para alterar uma permissão remotamente, use
`discord-mcp remote-set-permission ORIGEM ID_DISCORD CANAL read|send|both|none`.
A origem deve ser HTTPS (HTTP somente em loopback). Abra o link exibido, entre
com a conta administradora pelo Discord, reabra o link após o login, confira a
operação e confirme; então pressione Enter na CLI. A CLI não precisa do banco
nem da chave. A solicitação fica em memória por até cinco minutos, é vinculada
à prova mantida pela CLI e pode ser consumida uma única vez. Reiniciar o
servidor invalida solicitações pendentes. Se a resposta se perder ou a operação
falhar após o consumo, confira o estado e a auditoria antes de iniciar outra
solicitação. Tokens MCP não concedem autorização administrativa. O cookie da
sessão web não é transferido para a CLI.

Para alterações de permissão e cadastros remotos, o serviço também verifica
na transação se o registro do canal permanece igual ao início da solicitação.
Se tiver sido substituído, criado ou removido nesse intervalo, a operação é
recusada; inicie outra solicitação e confira o consentimento novamente.

Para consultar somente os nomes dos canais, use
`discord-mcp remote-list-channels ORIGEM` e confirme a consulta no navegador.
Essa é uma autorização distinta, também de uso único: ela não concede
permissão para alterar canais nem retorna tokens, webhooks ou credenciais. A
consulta é recusada se o banco estiver em quarentena.

Para consultar as concessões de um canal específico, use
`discord-mcp remote-show-permissions ORIGEM CANAL` e confirme a consulta no
navegador. O consentimento é limitado ao canal informado e não autoriza
alterações; a saída contém IDs Discord e permissões de leitura e envio. Um
canal inexistente ou um banco em quarentena produz erro, não uma lista vazia.

Para cadastrar ou substituir um webhook remotamente, use
`discord-mcp remote-add-webhook ORIGEM CANAL`. Forneça a URL em uma linha por
stdin protegido **não conectado a um terminal**; mantenha o stdin aberto para
enviar uma segunda linha após o consentimento. Por exemplo, em um shell POSIX,
com um arquivo privado contendo a URL e uma quebra de linha:

```sh
{ cat /caminho/privado/webhook; read -r _ </dev/tty; printf '\n'; } | discord-mcp remote-add-webhook https://servidor.exemplo CANAL
```

Abra o link exibido, entre como administrador, reabra o link, confira o nome e
o ID do webhook e autorize; então pressione Enter. Substituir um canal revoga
suas permissões anteriores. A credencial é enviada ao serviço no corpo HTTPS
após o Enter na CLI, nunca na URL, nos argumentos ou no consentimento; se o
administrador ainda não tiver autorizado, o serviço recusará a operação.
Quando armazenada, a credencial é cifrada no banco. O proxy não deve registrar
corpos de requisições. Uma
falha ou perda da resposta após consumo exige verificar o estado e a auditoria
antes de tentar novamente. Esse comando cadastra webhook, não token de bot.

Para cadastrar um canal com bot, use
`discord-mcp remote-add-bot ORIGEM CANAL ID_CANAL_DISCORD`. Forneça o token do
bot por stdin protegido não terminal, com uma quebra de linha, e mantenha o
stdin aberto para a confirmação após o consentimento no navegador, como no
exemplo anterior. O consentimento mostra o nome e o ID do canal Discord, não o
token; a autorização é vinculada ao token e de uso único. A substituição de
um canal revoga suas concessões anteriores. O serviço cifra o token no banco e
registra o administrador Discord na auditoria; o proxy não deve registrar
corpos de requisição. O cadastro não verifica o token nem as permissões do bot
no Discord: isso exige validação posterior no ambiente autorizado.

Para remover a configuração local de um canal e revogar suas concessões, use
`discord-mcp remote-delete-channel ORIGEM CANAL` e confirme a exclusão no
navegador. A operação não exclui o canal no Discord. A autorização é de uso
único e vinculada à versão do registro existente no início da solicitação: se o
canal for substituído antes da execução, a exclusão é recusada e será preciso
iniciar outra solicitação. A exclusão e a revogação são auditadas na mesma
transação. Em caso de resposta perdida, confira o estado antes de tentar outra vez.

Em `/admin/observability`, a conta administradora pode consultar as últimas 24
horas de requisições MCP HTTP, respostas HTTP 4xx/5xx e ferramentas de leitura e
envio concluídas com ou sem erro. Os dados são agregados por minuto no SQLite,
sem conteúdo, credenciais ou identificadores de usuário e canal. As falhas das
ferramentas incluem negação de acesso, validação e erros do Discord; não são
equivalentes a falhas HTTP. O tempo médio é medido até a produção da resposta
HTTP estar pronta, antes da gravação da própria métrica e não até o término da
transmissão do corpo; cada amostra é limitada a 10 minutos.

A coleta é de melhor esforço e pode omitir operações canceladas ou gravações
que falharam. Um aviso aparece se uma falha de gravação for detectada durante a
execução atual; ele não sobrevive ao reinício. A limpeza da retenção ocorre na
inicialização e quando um novo evento é gravado: dados antigos podem permanecer
em um servidor ocioso até o próximo evento ou reinício. A coleta não cobre stdio,
OAuth, interface administrativa nem a saúde do Discord.

Em `/admin/audit`, o administrador pode consultar eventos de login Discord após
validação da tentativa, alterações de canais locais e concessões ou revogações
de permissões. Alterações bem-sucedidas e seus eventos são gravados na mesma
transação; se o registro falhar, a alteração é revertida. O histórico inclui
identificadores Discord e nomes de canais, mas não inclui conteúdo de mensagens,
tokens ou URLs de webhook. Esses metadados ficam no mesmo banco SQLite, protegidos
pelas permissões do arquivo, mas não são cifrados nem resistentes à adulteração
por quem controla o banco. Tentativas rejeitadas antes da validação do login,
erros de token MCP e eventos anteriores à implementação não são registrados.

A retenção de auditoria é de 90 dias por padrão; sua limpeza ocorre na
inicialização e em novas gravações, podendo ser adiada enquanto o servidor está
ocioso. Falhas de gravação de tentativas de login negadas são indicadas na
interface apenas durante a execução atual. O histórico não é uma trilha de
auditoria completa ou imutável.

Em `SIGINT` ou `SIGTERM`, o servidor tenta drenar requisições por até 15
segundos. Ao exceder o limite, operações em andamento podem ficar incompletas;
isso não garante entrega de mensagens ou recuperação após interrupção forçada.
O teste local `cargo test --test drain` envia `SIGTERM` enquanto uma requisição
MCP está sendo recebida e verifica a conclusão, o fechamento do listener e o
prazo para uma requisição bloqueada. Ele não testa uma chamada real ao Discord,
streams longos ou efeitos externos iniciados antes do encerramento.

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

Para o exercício opt-in de snapshot e restauração com dados sintéticos usando o
projeto irmão, consulte [docs/sqlite-recovery-drill.md](sqlite-recovery-drill.md).
O documento também descreve a preparação em quarentena e a ativação explícita de
uma cópia offline. O exercício não configura recuperação de desastre em produção.

O benchmark isolado de caminhos MCP HTTP e suas limitações estão documentados
em [docs/benchmarks.md](benchmarks.md).
