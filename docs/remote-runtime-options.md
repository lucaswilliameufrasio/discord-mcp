# Execução remota experimental: systemd ou Docker

Em 2026-11-07, `drills/caddy/run.sh` passou em namespace isolado de loopback:
o teste HTTPS do Caddy e os três testes opt-in da CLI Ciphera passaram, incluindo
a ausência do marcador sintético nos logs. Foi usado Caddy v2.11.7 preexistente
no host; isso não comprova sua proveniência nem TLS ou custódia em produção.

As duas opções são alternativas para **uma única instância HTTP**, não devem ser
iniciadas juntas contra o mesmo banco. São modelos para Linux, não instalações
executadas nem configurações de produção validadas. O modo stdio local continua
separado e pode ter múltiplas instâncias com outro banco local. O servidor HTTP
ainda não foi validado com Discord real.

## Limite de confiança

No modo remoto, somente o processo do serviço deve ter acesso ao diretório do
SQLite, arquivos auxiliares e chave; clientes CLI devem usar as operações
`remote-*`. Um proxy HTTPS externo é obrigatório para acesso remoto, deve
encaminhar para o loopback correto, preservar `Host` e não registrar credenciais,
cookies, códigos, URLs de callback ou corpos sensíveis. O proxy, DNS e firewall
precisam ser escolhidos e verificados no ambiente real; não são criados aqui.

Um exercício local isolado com Caddy, CA sintética e MCP real confirmou que o
Host configurado permite uma chamada autenticada, enquanto chamadas sem token ou
com token inválido recebem 401. A indisponibilidade do backend retornou 502.
Com outro Host e o mesmo token, o exercício não recebeu uma resposta MCP de
ferramentas; o Caddy pode responder 200 vazio para um site não correspondente,
portanto status HTTP isoladamente não comprova rejeição.
Sem `handle_errors`, o log de erro do Caddy incluiu um marcador sintético da
requisição mesmo sem access log; o modelo gerado agora trata o erro e o marcador
não apareceu no log desse exercício. O fixture usou `tls internal` e porta 8443,
não ACME ou o certificado do ambiente real. Isso não prova que todas as falhas,
configurações globais ou outros componentes deixam de registrar dados sensíveis;
revise e teste o caminho completo antes de ativá-lo.

O exercício automatizado `drills/caddy/run.sh` exige Caddy previamente
verificado em `CADDY_BIN`, CLI Ciphera verificada em `CIPHERA_TEST_BINARY`,
Rust/Cargo, curl, ip e um namespace de rede que
contenha apenas loopback; ele recusa outras interfaces, usa somente dados
sintéticos e não usa Docker. Execute-o dentro de um sandbox autorizado, sem
acesso a credenciais, serviços ou sockets do host, com cache Cargo local e
os dois apontando para binários de teste. Exemplo dentro desse sandbox:
`CADDY_BIN=/caminho/caddy CIPHERA_TEST_BINARY=/caminho/ciphera bash drills/caddy/run.sh`. O processo cria e remove
seu diretório temporário, inclusive a CA sintética; interrupções forçadas podem
deixar arquivos temporários. Ele não valida certificados ou proxies reais.
O exercício confere que o pacote não sobrescreve um diretório existente, usa seu
site Caddy e verifica os demais arquivos contra os modelos. Com a CLI Ciphera
real e API sintética, cobre falhas e inicialização do MCP por HTTPS; a expansão
do drop-in é simulada e não substitui sua execução sob systemd.

As opções globais do fixture estão em `drills/caddy/global-options.caddy` e o
teste opt-in em `tests/caddy_tls.rs`. Para execução manual, gere um arquivo com `setup remote caddy
example.test DIRETORIO --confirm` e concatene as opções globais do fixture antes
do arquivo gerado, sem alterar o bloco do site. Execute-o somente com Caddy e teste compartilhando um
namespace de rede isolado que disponha de loopback, com a CA sintética acessível
em `DISCORD_MCP_CADDY_CA` e o caminho do arquivo concatenado em
`DISCORD_MCP_CADDY_CONFIG`: `cargo test --locked --all-features --test caddy_tls
-- --ignored --test-threads=1`. O teste inicia seu próprio MCP sintético. A
verificação de logs e a falha do backend são etapas externas ao teste Rust; o
operador deve forçar 502 com marcador sintético em query e autorização e conferir
que ele não aparece no log do Caddy sem exibir os logs. O fixture adiciona apenas
opções globais para CA interna, porta 8443 e ausência de redirecionamento ao modelo gerado; não é configuração de
produção. Não reutilize a CA ou sua chave em outro ambiente.

O lock em `src/process_guard.rs` é consultivo e não é uma fronteira de segurança.
Em systemd, proteja o diretório com uma conta de sistema exclusiva. Em Docker
rootless, o daemon e seus contêineres devem pertencer a uma identidade dedicada;
outros processos capazes de usar essa identidade ou seu socket podem contornar
a separação. `root`, administradores do host e administradores do daemon Docker
continuam confiáveis. Verifique negação real de leitura, escrita, substituição e
acesso à chave usando uma identidade não privilegiada distinta antes de produção.
Esses modelos por si só não demonstram esse isolamento.

## Configuração e segredos

Veja no README as variáveis exigidas pelo `serve`. `DISCORD_MCP_KEY` é uma chave
de 32 bytes em hexadecimal; `DISCORD_MCP_DISCORD_CLIENT_SECRET` também é segredo.
Não coloque valores no repositório, argumentos, histórico, imagem ou logs.
O serviço atual lê essas variáveis do ambiente: não há integração automática
com Ciphera dentro da aplicação, renovação de credenciais nem carregamento de secrets por arquivo.
Uma solução operacional precisa buscar os valores com identidade de serviço
restrita e disponibilizá-los de forma segura antes da inicialização; o modelo
usa um arquivo externo de ambiente como **ponte operacional**, não como solução
de custódia. Proteja-o e sua entrega, avalie exposição a administradores e a
processos autorizados e não execute `docker compose config` sem `--quiet` com
segredos reais. O acesso ao Ciphera e à recuperação não pode depender apenas
do mesmo host que será recuperado.

Os modelos reservam o endereço `127.0.0.1:8000` e o caminho interno do banco;
revise colisões e ajuste conscientemente para o seu host. Em systemd, variáveis
definidas no `EnvironmentFile` podem sobrescrever `Environment`: não inclua
`DISCORD_MCP_DB` ou `DISCORD_MCP_LISTEN` no arquivo sem revisar essa mudança.

## Modelo systemd

`deploy/systemd/discord-mcp.service` pressupõe binário pré-compilado compatível
em `/usr/local/bin/discord-mcp`, conta/grupo `discord-mcp` previamente criados,
`/etc/discord-mcp/service.env` protegido e state directory local privado em
`/var/lib/discord-mcp`. O serviço aplica restrições systemd, inicializa com
`serve` e permite 25 segundos para o encerramento gracioso de até 15 segundos.
Confira propriedade/permissões inclusive de diretórios já existentes: a
configuração não é prova de correção de permissões preexistentes. O arquivo de
ambiente é lido pelo systemd e valores passam ao processo; não representam
isolamento de segredos contra root ou administradores do serviço.

## Modelo Docker

`deploy/docker/Dockerfile` empacota um binário release pré-compilado do checkout;
recebe `RUNTIME_IMAGE` sem padrão. Forneça imagem de runtime confiável fixada
por digest, da mesma arquitetura, com libc compatível e certificados de CA para
as conexões HTTPS. Não há download ou instalação embutida. O `.dockerignore`
restringe o contexto ao Dockerfile e binário, evitando enviar arquivos de
ambiente do projeto ao builder. Compile e valide o artefato no ambiente de build
adequado, não presumindo compatibilidade de glibc entre máquinas.

Para montar localmente, após compilar o binário release para o ABI escolhido,
o operador pode fornecer uma base verificada já disponível localmente e uma
tag local temporária:

```sh
cargo build --locked --release
docker --context rootless build --network none --pull=false \
  --build-arg "RUNTIME_IMAGE=$RUNTIME_IMAGE" \
  -f deploy/docker/Dockerfile -t "$LOCAL_IMAGE_TAG" .
```

Esses comandos não escolhem a base, verificam TLS ou publicam a imagem. Para
outro ABI, produza e teste o binário apropriado primeiro; não use uma VPS de
produção como ambiente de build. A construção local com a base em cache
`node:24-bookworm-slim` produziu uma imagem, mas **não iniciou**: o binário
compilado nesta máquina exige `GLIBC_2.38`, ausente dessa base. Não use essa
combinação como runtime.

`deploy/docker/compose.yaml` exige referência `DISCORD_MCP_IMAGE`, UID/GID não
root no contêiner, arquivo de ambiente externo e diretório de dados privado já
existente (`DISCORD_MCP_CONTAINER_UID`, `DISCORD_MCP_CONTAINER_GID`,
`DISCORD_MCP_SERVICE_ENV`, `DISCORD_MCP_DATA_DIR`), além de
`DISCORD_MCP_RELAY_IMAGE` e `DISCORD_MCP_HOST_PORT`. Faça o mapeamento desses
UIDs para o proprietário do bind mount conforme o daemon e seu user namespace;
um UID não root no contêiner isoladamente não prova separação no host. Use
somente uma imagem própria verificada pelo digest. O Dockerfile não contém
segredos; Compose passa valores do arquivo ao ambiente do processo e os torna
acessíveis a administradores do daemon/contêiner.

O servidor recusa bind fora de loopback. Um relay Caddy compartilha o namespace
de rede do MCP, escuta na porta interna 8080 e encaminha para `127.0.0.1:8000`;
Docker publica a porta do relay somente no loopback do host. O relay não oferece
TLS: ainda é necessário um proxy HTTPS externo. Outros contêineres na mesma
rede bridge podem alcançar o relay; a rede não é uma fronteira de autorização.
Não conecte serviços não confiáveis à rede e valide firewall e exposição no
ambiente real. O relay não tem volume do banco nem segredos do serviço, mas vê
as requisições e respostas. Seu ciclo de vida depende do namespace do MCP:
recriações e reinícios coordenados, disponibilidade e recuperação devem ser
testados no ambiente escolhido; `depends_on` não é uma verificação de saúde.

`deploy/docker/Relay.Dockerfile` cria a imagem do relay a partir de uma imagem
Caddy Alpine verificada e fixada por digest, copiando o binário sem a file
capability que impede sua execução quando todas as capabilities são removidas.
O build exige `cp` e `getcap` na base. Defina `CADDY_IMAGE` e construa sem pull:

```sh
docker --context rootless build --network none --pull=false \
  --build-arg "CADDY_IMAGE=$CADDY_IMAGE" \
  -f deploy/docker/Relay.Dockerfile -t "$LOCAL_RELAY_TAG" .
```

Em teste sintético rootless, a base local `caddy:2.11.7-alpine` tinha digest
`sha256:e70b164da81ef698b3ad19af799ea6086ce6d9e48ed4079eb9fe26f6fbf14ee5`.
O Compose com imagens locais, CA montada para o teste e dados sintéticos
respondeu HTTP 200 em `/admin` pelo loopback do host; após parar o MCP a
conexão foi recusada. Um marcador sintético na query não apareceu nos logs dos
contêineres. Isso não demonstra preservação de Host, ausência geral de vazamentos,
TLS, reinício coordenado ou segurança de produção. O experimento anterior com
`network_mode: host` não alcançou o loopback do contêiner neste daemon rootless.

O comando `docker --context rootless compose -f deploy/docker/compose.yaml
config --quiet`, com as referências e arquivo externo definidos, valida a
sintaxe sem iniciar serviços. Não prova que a imagem inicia, que o bind mount é
acessível, que o proxy alcança o serviço ou que as permissões são seguras.

## Ciphera e S3

Em 2026-11-07, uma VM Debian 13.7 descartável com systemd 257 e cgroup v2
executou uma unidade sintética com o fragmento `ExecStart` deste drop-in,
substituindo apenas caminhos e executáveis por fixtures de teste. A expansão
real das variáveis, a entrega de uma credencial via `LoadCredentialEncrypted`,
os argumentos e a restauração do stderr do MCP simulado funcionaram. Chave
herdada, projeto ausente e credencial ausente impediram a inicialização; os
marcadores de segredos sintéticos não apareceram no journal da unidade.
A credencial usou `systemd-creds --with-key=null`, sem confidencialidade ou
autenticidade, **somente para teste**. Não foi testada proteção por chave do
host/TPM nem uma instância Ciphera real nesse primeiro teste.

Na mesma VM, uma unidade de teste separada executou também o fragmento do
drop-in sob systemd com Ciphera CLI 0.4.0 e o binário MCP reais, adaptando os
caminhos para o diretório de teste. Uma API Ciphera sintética em loopback
forneceu a chave de um banco sintético preexistente, e o MCP respondeu no endpoint
administrativo de loopback. Após parar o MCP e a API sintética, uma nova
inicialização falhou sem alterar o hash do banco; os marcadores de segredo
sintético não apareceram no journal da unidade. Os serviços de teste foram
parados. A credencial usou `--with-key=null`; esse exercício não valida a
confidencialidade da credencial, uma instância Ciphera real, TLS público ou
recuperação em produção.

O drop-in opcional `deploy/systemd/ciphera.conf` usa `ciphera run` para injetar
variáveis no processo sem gravar os valores de aplicação em um novo arquivo.
Ele exige `/usr/local/bin/ciphera`, credencial systemd criptografada em
`/etc/credstore.encrypted/discord-mcp-ciphera-token` e arquivo protegido
`/etc/discord-mcp/ciphera.env` com identificadores não secretos `CIPHERA_PROJECT`
e `CIPHERA_ENV`. Revise e adapte esses caminhos antes de instalar. O instalador
apenas cria o drop-in em diretório existente, sem sobrescrever, criar credenciais,
recarregar systemd ou reiniciar. A prévia mostra o conteúdo completo.

O token é acessível ao processo Ciphera durante o bootstrap; os segredos da
aplicação continuam no ambiente do processo e acessíveis a administradores ou
processos autorizados. Defina escopo, validade, rotação e recuperação independente
do token e da chave; nunca registre os valores. Planeje separadamente a remoção
de cópias antigas de segredos e revise conflitos com o `EnvironmentFile` anterior:
nada é removido ou alterado automaticamente. O drop-in recusa iniciar se
`DISCORD_MCP_KEY` ou `DISCORD_MCP_DISCORD_CLIENT_SECRET` já estiverem definidos
no ambiente herdado, inclusive com valor vazio; remova-os somente após revisão e
garantia de recuperação. Isso evita depender da precedência entre fontes.
O stderr do bootstrap Ciphera é descartado porque o cliente pode incluir no
erro um corpo de resposta do provedor; o código de saída continua indicando
falha. O stderr da aplicação é restaurado antes de iniciar o MCP. Isso reduz
diagnóstico de falhas do cliente: consulte sinais seguros do provedor e o estado
do serviço, sem registrar tokens ou corpos. Não é uma garantia geral de ausência
de vazamento em outros canais ou logs.
A compatibilidade do CLI, do systemd,
da fonte de credencial e o comportamento real de falha e reinício ainda exigem
verificação no host autorizado. Não use esse modelo como prova de produção.

Em validação local isolada, `systemd-analyze verify` carregou a unidade e o
drop-in `ciphera.conf` sem erro de sintaxe. O teste automatizado executa o
fragmento shell com um cliente Ciphera simulado, mas não executa o serviço sob
um gerenciador systemd nem comprova a expansão real de variáveis ou integração
com uma instância Ciphera. O teste posterior na VM, descrito acima, cobre a
expansão e entrega de uma credencial sintética com chave nula, não a custódia real.
O teste simulado cobre falha do cliente e execução sem chave da aplicação: ambos
encerram com erro antes da criação de um banco novo, sem registrar o token sintético.
O teste opt-in `tests/ciphera_cli.rs` também executou Ciphera CLI 0.4.0 real
contra uma API sintética em loopback: a recusa HTTP impediu a criação do banco,
e uma resposta sintética válida injetou as variáveis necessárias para o preflight
da aplicação sem exibi-las. Para repetir, use um sandbox sem rede externa e com
executável Ciphera verificado fornecido por `CIPHERA_TEST_BINARY`, então execute
`cargo test --locked --all-features --test ciphera_cli -- --ignored
--test-threads=1`. O endpoint, token, projeto e segredos desse teste são
sintéticos; ele não verifica autenticação, entrega, rotação ou falhas da
instância Ciphera real, nem substitui a execução do drop-in no systemd.
Um segundo caso executa o fragmento shell do drop-in com a CLI real e a mesma API
sintética: recusa HTTP, indisponibilidade HTTP 503 e resposta sem chave preservam um banco existente, e uma
resposta válida inicia o MCP sem remover o canal sintético. A expansão `$$` é
simulada nesse caso; ele não valida o parser em execução nem credenciais systemd.
Uma tentativa posterior de iniciar um gerenciador systemd de usuário em
namespaces descartáveis falhou ao acessar a hierarquia de cgroups; não foram
alteradas permissões ou configurações do host. A validação posterior na VM
superou essa limitação para o fixture, mas não comprova a implantação real.

### Plano para validar o drop-in sob systemd

Execute somente em um ambiente de teste descartável e autorizado, com gerenciador
systemd próprio, cgroup delegado, rede isolada e sem acesso ao socket Docker,
serviços do host, identidade real ou credenciais reais. Não use o gerenciador do
host compartilhado. Use um executável Ciphera simulado que registre apenas nomes
de argumentos e estados booleanos, e uma credencial sintética protegida. Não
registre token, ambiente completo, conteúdo de credenciais ou saída de processos.

1. Instale cópias da unidade e do drop-in no ambiente descartável e confirme
   que o parser reconhece o drop-in. Use somente paths e identidades de teste.
2. Com projeto, ambiente e credencial sintéticos válidos, inicie a unidade e
   verifique que o cliente simulado recebe os argumentos como valores literais,
   inclusive quando contêm espaços e metacaracteres. Confirme que o serviço
   simulado foi executado e não houve divulgação em logs.
3. Repita com credencial ausente, vazia ou ilegível, e com projeto ou ambiente
   ausente. Em cada caso, o serviço não deve iniciar nem usar credenciais
   alternativas herdadas; verifique o estado sem imprimir o ambiente.
4. Simule falha e indisponibilidade do cliente Ciphera, e observe o encerramento
   e os limites de reinício. Confirme que nenhuma execução continua com segredos
   antigos e que a unidade existente não foi sobrescrita na preparação.
5. Registre versão do systemd e do CLI, método de isolamento, casos e resultados
   sem identificadores do host. Uma execução real com `LoadCredentialEncrypted`
   exige uma credencial sintética criptografada pelo gerenciador de teste; um
   arquivo de texto simples ou a substituição manual de `$$` não a valida.

Mesmo que esses casos passem, continuam pendentes a integração com a instância
Ciphera autorizada, política de expiração e rotação, isolamento da identidade,
TLS no caminho completo e restauração independente no ambiente de destino.

Direção atual: cofre embutido e provedores opcionais (Infisical, AWS Parameter
Store, AWS Secrets Manager e Ciphera), conforme `docs/secrets-provider-plan.md`.
Ciphera não é obrigatório. Para snapshots criptografados e versionados, avaliar
armazenamento de objetos compatível com S3 em domínio de
falha separado. A documentação pública consultada do Ciphera descreve secrets,
não uma API de armazenamento de backups. **Nenhum** conector de aplicação ao
Ciphera, job de snapshot de produção, envio ao S3 real, retenção de backups ou restore remoto é
implementado por estes modelos. Não copie somente o arquivo SQLite enquanto WAL
está ativo. A biblioteca irmã e seu binário ainda carecem de proveniência
pública fixável e coordenação comprovada dos escritores; captura incremental
permanece desabilitada. O exercício em `docs/sqlite-recovery-drill.md` é apenas
sintético; não demonstra recuperação operacional, RPO ou RTO.

Antes de operacionalizar, definir provedor/bucket/região, identidade com mínimo
privilégio, criptografia, versão/retensão de backup, custódia independente das
chaves, entrega das credenciais, recuperação de Ciphera, proxy e alvo de testes.
Um exercício de restauração em destino isolado deve preceder qualquer uso real.
Sem um protocolo de snapshot e ferramenta reproduzível aprovados, não ativar
backup automático nem afirmar que há proteção contra perda do host.
