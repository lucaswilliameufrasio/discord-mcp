# Execução remota experimental: systemd ou Docker

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
com Ciphera, renovação de credenciais nem carregamento de secrets por arquivo.
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

Decisão do usuário: Ciphera para chaves/segredos e armazenamento de objetos
compatível com S3 para snapshots criptografados e versionados, em domínio de
falha separado. A documentação pública consultada do Ciphera descreve secrets,
não uma API de armazenamento de backups. **Nenhum** conector de aplicação ao
Ciphera, job de snapshot, envio ao S3, retenção de backups ou restore remoto é
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
