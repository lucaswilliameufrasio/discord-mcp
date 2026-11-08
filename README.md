# Discord MCP

Servidor MCP em Rust para enviar mensagens ao Discord por webhook ou bot e ler mensagens por bot.

> **Estado:** o modo HTTP remoto é experimental e ainda não foi validado com o Discord real. Não exponha o listener diretamente à internet.

## Começar

Para compilar a partir do código, instale Rust e Cargo, execute `cargo build --locked --release` e use o binário em `target/release/discord-mcp`.

O fluxo de release e o Compose para Linux estão descritos em [Releases e containers](docs/releases.md). O mesmo executável atende a CLI local/stdio e ao servidor com o comando `serve`; proxy HTTPS e deploy são responsabilidade do operador.

1. Execute `discord-mcp setup local` para conferir os requisitos de configuração.
2. Configure `DISCORD_MCP_DB` e `DISCORD_MCP_KEY` por um mecanismo seguro. A chave deve ter 32 bytes aleatórios em hexadecimal; mantenha uma cópia de recuperação separada do banco.
3. Execute `discord-mcp setup local --init` para inicializar o banco, sem substituir canais existentes.
4. Execute `discord-mcp web`, abra o endereço local exibido e use o código de uso único para cadastrar canais.
5. Configure o cliente MCP para iniciar `discord-mcp` sem argumentos, com as mesmas variáveis. Não coloque segredos em argumentos, histórico ou arquivos versionados.

Sem `--init`, a CLI apenas orienta e confere o formato da chave quando presente; não valida credenciais no Discord. Também é possível usar um webhook único em `DISCORD_WEBHOOK_URL` sem banco; consulte o [guia de operação](docs/operations.md).

### Segredo OAuth no cofre embutido

O modo HTTP pode usar o banco cifrado existente para guardar o segredo OAuth,
sem depender de Ciphera ou outro provedor. Primeiro inicialize o banco e revise
`discord-mcp setup local vault discord-oauth --preview`. Com `--confirm`, forneça
o segredo por stdin protegido **não terminal**, terminado por uma quebra de
linha. A operação exige banco existente, não imprime nem substitui o segredo.
Selecione `DISCORD_MCP_SECRET_PROVIDER=embedded` para `serve` e não defina
`DISCORD_MCP_DISCORD_CLIENT_SECRET` nesse modo; conflitos são recusados. A chave
`DISCORD_MCP_KEY` continua sendo necessária e deve ter custódia e recuperação
independentes do banco. O modo padrão de variável de ambiente continua compatível.

A preparação de recuperação remove o segredo OAuth da cópia em quarentena; após
ativação, cadastre uma credencial válida deliberadamente. Não reutilize segredo
revogado. O preflight `setup remote --check` ainda não abre o cofre embutido e
recusa essa seleção; ele não prova que o servidor iniciará.

Para copiar uma configuração OAuth existente do ambiente, revise
`discord-mcp setup local vault migrate-oauth-from-env --preview`; use o mesmo
comando com `--confirm` para copiar `DISCORD_MCP_DISCORD_CLIENT_SECRET` para um
banco já existente. Ele não modifica a fonte nem troca o provedor e recusa
sobrescrever o destino. Uma gravação bem-sucedida não prova que uma instância
em execução adotou o cofre. Faça backup, valide a nova configuração em ambiente
autorizado e só depois remova a fonte antiga; não deixe a variável antiga no
ambiente de um servidor selecionado com `embedded`, pois o conflito é recusado.

Para copiar do Infisical ao cofre, configure as variáveis `DISCORD_MCP_INFISICAL_*`
descritas abaixo e revise `discord-mcp setup local vault
migrate-oauth-from-infisical --preview`. Com `--confirm`, a CLI consulta o
Infisical e insere no banco existente apenas se o cofre ainda não contiver esse
segredo; erro de provedor, conflito ou quarentena recusam a operação. Não defina
a variável OAuth antiga durante essa cópia. A origem e a seleção de provedor
não são alteradas. A migração não verifica a validade do segredo no Discord nem
prova a adoção por uma instância em execução. Se o processo for interrompido ou
o resultado da gravação for desconhecido, não presuma que houve rollback nem
remova a origem; use a comparação local descrita abaixo.

Para substituir o segredo já guardado, revise `discord-mcp setup local vault
rotate-oauth --preview`. Com `--confirm`, forneça por stdin protegido não terminal
duas linhas terminadas por quebra: o segredo atual seguido do substituto. A troca
é atômica e rejeita o valor atual incorreto ou desatualizado; concorrentes que
usam o mesmo valor atual não sobrescrevem a mudança um do outro. No Linux, o
lock cooperativo exige parar a instância HTTP que usa esse banco antes da troca.

**Isso apenas substitui a cópia local.** Não cria nem revoga credenciais no
Discord, não verifica se a nova credencial funciona e não reinicia consumidores.
Valide a adoção por uma nova instância e o fluxo de recuperação no ambiente
autorizado. Nunca restaure automaticamente uma credencial revogada ou
comprometida: o cofre não consulta o estado de revogação do Discord e não impede
que um operador recadastre manualmente uma credencial antiga.

A revogação do segredo OAuth não encerra automaticamente sessões administrativas
já abertas no processo: em teste sintético elas permaneceram válidas até o
reinício. Trate revogação de sessões e tokens MCP separadamente; o teste não
comprovou invalidação em outras instâncias ou no Discord real. Um teste do
binário real também confirmou que um token MCP existente permanece utilizável
após rotação local e reinício: revogue-o separadamente quando necessário.

Após resultado perdido de migração ou rotação, `discord-mcp setup local vault
compare-oauth --confirm` compara o candidato fornecido em uma única linha por
stdin protegido não terminal com o segredo atualmente no cofre. Informa se é
igual, diferente ou ausente, sem imprimi-lo nem alterá-lo. Erros de chave,
corrupção e quarentena não são tratados como ausência. A comparação reflete um
snapshot: não identifica qual operação gravou o valor, não garante que ele não
seja alterado depois e não prova validade, revogação ou adoção. Preserve a
origem até verificação independente; nunca restaure automaticamente credenciais
revogadas ou comprometidas.

### Infisical opcional

Para buscar **somente o segredo OAuth do Discord** na inicialização HTTP,
selecione `DISCORD_MCP_SECRET_PROVIDER=infisical` e forneça
`DISCORD_MCP_INFISICAL_ORIGIN`, `DISCORD_MCP_INFISICAL_PROJECT_ID`,
`DISCORD_MCP_INFISICAL_ENVIRONMENT`, `DISCORD_MCP_INFISICAL_SECRET_PATH` e
`DISCORD_MCP_INFISICAL_TOKEN` por um mecanismo seguro. O segredo no Infisical
deve se chamar `DISCORD_MCP_DISCORD_CLIENT_SECRET`. A origem exige HTTPS, exceto
`http://127.0.0.1` para testes locais; a aplicação não segue redirecionamentos
e recusa conflitos com a variável de segredo OAuth antiga. A chave
`DISCORD_MCP_KEY` do banco ainda é fornecida separadamente. Falhas do provedor
impedem o servidor de iniciar, sem fallback. O token deve ser uma credencial
de máquina com acesso mínimo, já emitida e entregue pelo operador: renovação,
rotação e atualização em processo não são implementadas. O preflight remoto
somente de leitura não conecta ao provedor. O HTTP continua experimental.

### AWS Secrets Manager opcional

Compile com `--features aws-secrets-manager`, selecione
`DISCORD_MCP_SECRET_PROVIDER=aws-secrets-manager` e configure
`DISCORD_MCP_AWS_SECRET_ID` com o nome ou ARN do segredo. Seu `SecretString`
é interpretado integralmente como o segredo OAuth do Discord, não como JSON;
`SecretBinary` é recusado. O SDK usa a cadeia de credenciais AWS habilitada
na compilação, região e configuração do ambiente. Prefira identidade de workload
com permissão mínima; essa compilação não habilita o provedor SSO do SDK. Trate
perfis, endpoint overrides e credenciais AWS como configuração confiável.

O servidor recusa a variável OAuth antiga nesse modo e não usa fallback se a
consulta falhar. A chave SQLite continua separada; o segredo é carregado apenas
na inicialização, sem atualização automática, e a recuperação exige revisão
independente. O preflight somente de leitura não consulta a AWS. Testes usam
endpoint e credenciais sintéticos; nenhuma integração AWS real foi validada.

Com essa feature habilitada, `discord-mcp setup local vault
migrate-oauth-from-aws-secrets-manager --preview` mostra a operação sem consultar
a AWS; substitua por `--confirm` para copiar o segredo configurado ao cofre
existente. A operação não altera o segredo de origem, a seleção de provedor ou
um segredo já cadastrado. Falhas ou destino em quarentena impedem a cópia. Não
configure a variável OAuth antiga junto a essa operação. Um resultado perdido
após o commit exige comparação e verificação independentes: preserve a origem e não presuma
que ocorreu rollback ou adoção pelo servidor.

### AWS Systems Manager Parameter Store opcional

Compile com `--features aws-parameter-store`, selecione
`DISCORD_MCP_SECRET_PROVIDER=aws-parameter-store` e configure
`DISCORD_MCP_AWS_PARAMETER_NAME` com o nome ou ARN do parâmetro. A integração
consulta exatamente esse parâmetro com descriptografia e exige `SecureString`;
o valor completo é o segredo OAuth. A cadeia de credenciais e as mesmas
ressalvas de IAM, SSO, endpoint, chave SQLite, rotação e testes sintéticos
descritas para Secrets Manager também se aplicam. Uma consulta negada, tipo
incorreto ou segredo OAuth antigo no ambiente impedem a inicialização, sem
fallback. O preflight somente de leitura não consulta a AWS.

Com essa feature, `discord-mcp setup local vault migrate-oauth-from-aws-parameter-store --preview`
apresenta a cópia sem consultar a AWS; `--confirm` copia o `SecureString` para
um cofre existente, sem substituir segredo ou alterar a origem e a seleção de
provedor. Resultado ambíguo após commit não possui reconciliação automática:
preserve a origem e não presuma rollback nem adoção pelo servidor.

Para gerar uma chave nova, revise `discord-mcp setup local --generate-key --preview`. Com `--confirm`, o comando emite a chave somente em stdout não terminal, para ser recebido por um mecanismo de segredos confiável; não salva arquivos nem configura a aplicação. Proteja o destino e a cópia de recuperação. **Não troque a chave de um banco existente:** isso impede a leitura dos dados cifrados.

## Modos e ferramentas

| Modo | Uso | Estado |
| --- | --- | --- |
| stdio local | Cliente MCP inicia o processo localmente | Webhook e bot; permissões do Discord ainda precisam ser verificadas |
| HTTP remoto | `discord-mcp serve` atrás de proxy HTTPS | Experimental; requer configuração e validação operacional |

O remoto é **opcional**: clientes locais que usam stdio não precisam configurar
HTTP, proxy, TLS, systemd ou supervisor. No remoto, a rotação do segredo OAuth
é acompanhada pelo operador: publique o substituto, reinicie e teste novos logins
OAuth e administrativo, conforme o [procedimento de rotação](docs/operations.md#rotação-operacional-do-segredo-oauth).
Não há comprovação automática de adoção; observações locais não são prova de
identidade da instância. Testes sintéticos não validam o Discord ou um provedor
real; a avaliação de uso em produção cabe ao operador.

`send_message` envia por webhook ou bot; `read_messages` lê até 20 mensagens de canal de bot. Menções automáticas são desativadas. O bot requer permissões Discord apropriadas, e a leitura de conteúdo pode exigir o intent privilegiado `MESSAGE_CONTENT`.

Para planejar o ambiente remoto, execute `discord-mcp setup remote` e leia as [opções de execução](docs/remote-runtime-options.md). No ambiente do serviço, `discord-mcp setup remote --check` verifica a configuração com as mesmas validações de inicialização, sem exibir valores. Esses comandos não conectam nem alteram servidor e não validam disponibilidade de TLS, credenciais, acesso ao banco ou recuperação. O serviço escuta somente em loopback; proxy, TLS, segredos e recuperação são responsabilidade do operador.

Para revisar os componentes juntos, use `discord-mcp setup remote bundle HOST --preview`. A criação opcional com `discord-mcp setup remote bundle HOST NOVO_DIRETORIO_ABSOLUTO --confirm` gera um pacote local sem segredos, sem sobrescrever caminhos existentes e sem instalar serviços. Revise cada arquivo e as dependências operacionais antes de qualquer ativação; se houver falha de escrita, o diretório parcial é preservado para inspeção.

Para um banco já existente, `discord-mcp setup remote --check-storage` verifica metadados do arquivo e do diretório pai imediato sem abri-lo ou criá-lo. Não verifica dono, diretórios ancestrais, conteúdo, chave ou recuperação e não elimina possíveis alterações concorrentes no caminho.

`discord-mcp setup remote --check-https-origin` verifica a sintaxe da origem HTTPS sem rede. Não verifica DNS, certificado, proxy, acesso público ou saúde do serviço.

Opcionalmente, `discord-mcp setup remote caddy HOST --preview` mostra um site Caddy HTTPS para o listener `127.0.0.1:8000`. Após revisão, `discord-mcp setup remote caddy HOST DIRETORIO_ABSOLUTO --confirm` cria um arquivo novo sem sobrescrever outro. Não instala nem recarrega o proxy, configura DNS ou emite certificado. O operador deve revisar a configuração global, a redação de logs, as portas e TLS antes de ativá-la; o hostname precisa corresponder à origem pública.

Para preparar o modelo systemd, revise `discord-mcp setup remote systemd --preview`. Opcionalmente gere os arquivos locais com `discord-mcp setup remote systemd NOVO_DIRETORIO --confirm`. O diretório não pode existir; nada é instalado ou enviado ao servidor. O modelo não inclui segredos, TLS ou backup e exige revisão específica do host.

Após essa revisão, no próprio host e com permissões adequadas, `discord-mcp setup remote systemd --install-unit DIRETORIO_ABSOLUTO --confirm` grava a unidade em um diretório systemd confiável escolhido pelo operador, somente se ainda não existir. Em Unix, o diretório não pode ser gravável por grupo ou outros. Não solicita privilégios, recarrega o systemd, inicia serviço ou configura o restante do servidor. Confira também proprietário e diretórios ancestrais; não use caminhos controlados por usuários não confiáveis.

O mesmo preview mostra um modelo de variáveis sem valores. `discord-mcp setup remote systemd --install-env-template DIRETORIO_ABSOLUTO --confirm` cria `service.env.example` sem sobrescrever outro arquivo; não lê segredos nem cria o arquivo de ambiente do serviço. A entrega efetiva das credenciais depende do mecanismo escolhido pelo operador.

O preview também inclui um drop-in **opcional** para Ciphera. Após revisar os caminhos, `discord-mcp setup remote systemd --install-ciphera-dropin DIRETORIO_ABSOLUTO --confirm` cria `ciphera.conf` no diretório de drop-ins escolhido, sem sobrescrever ou reiniciar. Ele exige Ciphera CLI compatível, um token de máquina fornecido por `LoadCredentialEncrypted` e arquivo separado com `CIPHERA_PROJECT` e `CIPHERA_ENV`; não cria nem consulta essas credenciais. Veja os limites e etapas em [opções de execução](docs/remote-runtime-options.md).

## Referências

- [Configuração, permissões, administração e segurança](docs/operations.md)
- [Modelos de execução remota e limitações](docs/remote-runtime-options.md)
- [Exercício sintético de backup e recuperação](docs/sqlite-recovery-drill.md)
- [Benchmark isolado e resultados](docs/benchmarks.md)
- Comandos disponíveis: `discord-mcp --help`

`discord-mcp setup recovery` apresenta a sequência de preparação em quarentena,
revisão e ativação explícita sem tocar nos arquivos. O exercício sintético não
comprova recuperação operacional ou prazo de recuperação em produção.

## Desenvolvimento

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked --all-features
cargo test --locked --all-features
```

Os testes opcionais de replicação e o exercício Floci têm pré-requisitos próprios; veja o guia de recuperação. Nunca use credenciais ou serviços reais nos testes locais.
