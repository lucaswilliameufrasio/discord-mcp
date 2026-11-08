# Releases e containers

O workflow `.github/workflows/release.yml` publica, ao receber uma tag estável
`vMAJOR.MINOR.PATCH` igual à versão em `Cargo.toml`, arquivos para Linux x86-64
e ARM64 e imagens multi-arquitetura da aplicação e do relay interno no GHCR. Cada arquivo `.tar.gz` contém o
mesmo executável `discord-mcp`, utilizável sem argumentos para MCP stdio ou com
`serve` para HTTP. Não são dois executáveis diferentes. Cada arquivo tem um
SHA-256 separado; o Compose também acompanha a release com seu hash.
Os builds incluem `encrypted-s3-backup`, `aws-secrets-manager` e
`aws-parameter-store`; Infisical e o cofre embutido não dependem dessas features.
Nenhuma dessas integrações ou do servidor HTTP é obrigatória para stdio.

### Evidências locais

Em 2026-11-08, o binário ARM64 foi compilado em host x86-64 com Rust 1.99.1,
Zig 0.14.1 e `cargo zigbuild --locked --release --target
aarch64-unknown-linux-gnu.2.39`, com as três features de release acima. O ELF
resultante é AArch64 e depende de `libc.so.6`, `libm.so.6` e
`ld-linux-aarch64.so.1`, com símbolos GLIBC até 2.39. Usando o loader e as
bibliotecas da imagem Ubuntu 24.04 ARM64 e QEMU de usuário, o smoke test em
loopback isolado passou para inicialização stdio e rejeição HTTP sem autenticação.
Isso não é uma execução nativa ARM nem verifica o runner ARM do GitHub.

O teste `drills/release/compose_smoke.py` também foi executado duas vezes com
imagens locais amd64 em Docker rootless: gravou um canal sintético no SQLite,
recriou o container e confirmou o canal no mesmo volume. O teste deixa seu
volume de fixture para inspeção; não valida backups nem a integração real com
Uirapuru.

Para preparar uma versão, atualize e revise `Cargo.toml` e `Cargo.lock`, faça
os testes e integre a alteração normalmente. A criação e o envio da tag são uma
ação separada e autorizam a publicação. O workflow exige acesso de escrita a
releases e pacotes do repositório; a visibilidade do pacote GHCR depende das
configurações do GitHub. Uma execução interrompida pode deixar imagens com
sufixo de arquitetura publicadas antes da release: confira o estado antes de
reexecutar. `latest` é uma referência mutável; fixe uma versão ou digest ao
operar. Não há publicação automática de novas versões sem uma tag.

## CLI local

Baixe o arquivo correspondente à arquitetura da release, confira seu SHA-256
com `sha256sum -c` e extraia-o. Execute `discord-mcp setup local` e siga a seção
Começar do README. Configure seu cliente MCP para iniciar o executável sem
argumentos. O release Linux não depende de Docker, systemd nem de servidor HTTP.
Arquivos para Windows e macOS não são produzidos neste fluxo.

## Compose para servidor

O arquivo `compose.yaml` contém a aplicação e um relay HTTP interno: não instala
proxy HTTPS, TLS, serviços externos ou Ciphera. O relay apenas encaminha a porta
publicada em loopback para o listener privado da aplicação. Usa imagens `latest`
por padrão; para fixá-las, forneça `DISCORD_MCP_IMAGE` e
`DISCORD_MCP_RELAY_IMAGE` com referências da mesma versão ou seus digests. Configure as
variáveis descritas em [Operação](operations.md), inclusive `DISCORD_MCP_KEY`,
e disponibilize-as por mecanismo seguro. O Uirapuru pode fornecer os valores
por suas variáveis do serviço; valores de ambiente de containers podem ser
vistos por quem tem acesso ao daemon Docker. Não versione credenciais.
O volume nomeado persiste o SQLite em `/data`; guarde a chave separadamente e
prepare backups antes de alterar volumes. Não execute `down -v` para atualização.
O diretório do volume deve ser gravável pelo UID/GID 10001:10001 da imagem.

O servidor escuta em `127.0.0.1:8000` no namespace compartilhado com o relay;
este recebe conexões na porta interna 8080. O Docker publica somente em
`127.0.0.1:8000` no host; `DISCORD_MCP_HOST_PORT` pode escolher outra porta
livre. O relay não fornece HTTPS nem deve ser confundido com o proxy externo.
Quando um proxy roda em outro namespace, acesso ao loopback do host depende da
configuração do ambiente; um proxy em outro container não o acessa automaticamente.
O operador escolhe e configura o proxy externo, exposição, HTTPS e integração
com o Uirapuru. O Compose não altera a validação de loopback da aplicação.
O relay compartilha o namespace de rede do serviço (`network_mode: service:`):
o analisador de blue-green do Uirapuru considera `network_mode` incompatível com
duas versões simultâneas. Use uma instância por Compose e não presuma suporte a
blue-green ou escala automática com esse arquivo. A integração completa com o
Uirapuru depende da versão e da configuração do ambiente e não foi testada aqui.

O volume e a chave são necessários para comandos administrativos e recuperação;
use o mesmo volume e configuração quando executar comandos administrativos com
`docker compose run` e pare o servidor quando o lock do banco exigir. Os
provedores externos exigem também suas credenciais e configurações próprias,
entregues pelo mecanismo de identidade escolhido; o Compose não as cria.

## Referências do workflow

As versões e avisos publicados nos repositórios oficiais foram consultados em
2026-11-08; os `uses` apontam para commits completos. O aviso
[GHSA-cxww-7g56-2vh6](https://github.com/advisories/GHSA-cxww-7g56-2vh6)
afeta versões antigas do download-artifact anteriores à correção 4.1.3, não a
versão selecionada.

| Action oficial | Release consultada |
| --- | --- |
| [actions/checkout](https://github.com/actions/checkout/releases/tag/v7.0.1) | v7.0.1 |
| [actions/upload-artifact](https://github.com/actions/upload-artifact/releases/tag/v7.0.2) | v7.0.2 |
| [actions/download-artifact](https://github.com/actions/download-artifact/releases/tag/v8.0.2) | v8.0.2 |
| [docker/login-action](https://github.com/docker/login-action/releases/tag/v4.8.0) | v4.8.0 |
