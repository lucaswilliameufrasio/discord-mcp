# Instalar o servidor HTTP no Linux

Escolha **um** dos caminhos: Docker Compose, Docker sem Compose ou systemd.
Não execute duas instâncias HTTP contra o mesmo banco. O modo local/stdio é
independente e usa o binário da release, sem Docker, proxy, TLS ou systemd.
Este guia não instala nem configura seu proxy HTTPS. Os exemplos usam a versão
`v0.1.1`; troque as duas referências juntas ao atualizar.

## Antes de começar

1. Tenha um servidor Linux amd64 ou arm64, a porta de loopback escolhida livre,
   e acesso à [release v0.1.1](https://github.com/lucaswilliameufrasio/discord-mcp/releases/tag/v0.1.1).
   Para systemd, escolha o binário correspondente à arquitetura e confira a
   compatibilidade de libc. Para Docker, use um daemon Docker funcional; o
   caminho Compose também exige o plugin `docker compose`.
2. Escolha uma origem HTTPS pública e configure separadamente seu proxy para
   alcançar `127.0.0.1:8000` no host, preservar o `Host` público e não registrar
   tokens, cookies, códigos ou URLs de callback. Um proxy em outro container pode
   precisar de configuração adicional para alcançar o loopback do host. Não
   exponha diretamente o listener HTTP na internet.
3. No Discord, configure sua aplicação OAuth com as URLs formadas pela origem
   pública mais `/oauth/discord/callback` e `/admin/discord/callback`. O endpoint
   MCP é a origem seguida de `/mcp`. Registre previamente um cliente MCP e sua
   URI de redirecionamento exata; o servidor aceita um cliente pré-registrado.
4. Prepare a chave do banco: 32 bytes aleatórios em hexadecimal, de uma fonte
   segura. Guarde uma cópia de recuperação separada do SQLite. Não coloque
   chaves, tokens ou segredos no repositório, imagens, argumentos, histórico ou
   logs. Restrinja o acesso de sistema e faça backups e testes de recuperação.

### Configuração e segredos

Para o exemplo com o provedor padrão de ambiente, entregue estas variáveis ao
serviço por mecanismo seguro. `DISCORD_MCP_KEY` e
`DISCORD_MCP_DISCORD_CLIENT_SECRET` são segredos:

| Nome | Valor necessário |
| --- | --- |
| `DISCORD_MCP_KEY` | Chave do banco, 32 bytes em hexadecimal. |
| `DISCORD_MCP_PUBLIC_ORIGIN` | Origem HTTPS pública, sem caminho. |
| `DISCORD_MCP_DISCORD_CLIENT_ID` | ID da aplicação OAuth no Discord. |
| `DISCORD_MCP_DISCORD_CLIENT_SECRET` | Segredo OAuth da aplicação Discord. |
| `DISCORD_MCP_ADMIN_ID` | ID numérico da conta Discord administradora. |
| `DISCORD_MCP_CLIENT_ID` | ID do cliente MCP pré-registrado. |
| `DISCORD_MCP_CLIENT_NAME` | Nome exibido no consentimento. |
| `DISCORD_MCP_CLIENT_REDIRECT` | URI de retorno exata do cliente MCP. |

Os exemplos abaixo usam um arquivo externo `/caminho/protegido/service.env`,
uma linha `NOME=valor` por variável, sem `export`; crie-o fora do repositório
com dono e permissões adequados. Para Docker sem Compose, não use aspas como
delimitadores nesse arquivo: o Docker as interpreta como parte do valor.
Esse arquivo é uma ponte operacional, não um cofre: administradores do host e
do daemon Docker podem acessar os valores. Não execute `docker compose config`
sem `--quiet` com segredos. Para o cofre embutido, Infisical ou AWS, consulte
[Operação](operations.md) e o [README](../README.md); a seleção é explícita e
não há fallback silencioso. Ciphera é opcional.

## Opção 1: Docker Compose

Há duas imagens, mas o Compose sobe ambas: `discord-mcp` é a aplicação e
`discord-mcp-relay` é um relay HTTP interno necessário para encaminhar a porta
do host ao listener que aceita somente loopback. O relay não fornece HTTPS;
seu proxy externo permanece independente. O relay vê as requisições e respostas;
outros containers conectados à mesma rede também podem alcançá-lo. Não conecte
serviços não confiáveis à rede e não trate a rede como autorização.

1. Baixe `compose.yaml` e `compose.yaml.sha256` da release para um diretório
   próprio e confira `sha256sum -c compose.yaml.sha256`.
2. Prepare o arquivo de ambiente descrito acima. Para fixar as imagens, inclua
   nele as referências abaixo ou forneça-as por outro mecanismo seguro:

   ```text
   DISCORD_MCP_IMAGE=ghcr.io/lucaswilliameufrasio/discord-mcp:v0.1.1
   DISCORD_MCP_RELAY_IMAGE=ghcr.io/lucaswilliameufrasio/discord-mcp-relay:v0.1.1
   ```

   Sem essas variáveis, o Compose usa `latest`, que pode mudar. O banco fica no
   volume nomeado `discord-mcp-data`, em `/data/discord-mcp.sqlite`; o volume
   precisa ser gravável pelo UID/GID 10001 da imagem. Opcionalmente defina
   `DISCORD_MCP_HOST_PORT` para outra porta de loopback livre.
3. No diretório do Compose, execute:

   ```sh
   docker compose --env-file /caminho/protegido/service.env config --quiet
   docker compose --env-file /caminho/protegido/service.env pull
   docker compose --env-file /caminho/protegido/service.env up -d
   docker compose --env-file /caminho/protegido/service.env ps
   ```

   Ajuste o contexto Docker por comando se necessário. Confira os logs com
   cuidado, sem compartilhar dados sensíveis. Valide o login e o cliente MCP
   pelo proxy externo. Para atualizar, faça backup, altere ambas as referências
   para a mesma versão, faça `pull` e `up -d`; preserve o volume e a chave.
   **Não use `down -v`**, que remove volumes nomeados.

## Opção 2: Docker sem Compose

Prepare o arquivo de ambiente e escolha nomes e porta livres. Estes comandos
criam um volume dedicado e dois containers; o relay compartilha somente a rede
do servidor, não recebe seu arquivo de segredos nem monta o banco:

```sh
docker volume create discord-mcp-data
docker run -d --name discord-mcp --restart unless-stopped --init \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=32m \
  --env-file /caminho/protegido/service.env \
  -e DISCORD_MCP_DB=/data/discord-mcp.sqlite \
  -e DISCORD_MCP_LISTEN=127.0.0.1:8000 \
  -v discord-mcp-data:/data -p 127.0.0.1:8000:8080 \
  ghcr.io/lucaswilliameufrasio/discord-mcp:v0.1.1
docker run -d --name discord-mcp-relay --restart unless-stopped \
  --network container:discord-mcp --user 10001:10001 \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m \
  -e XDG_CONFIG_HOME=/tmp/caddy-config -e XDG_DATA_HOME=/tmp/caddy-data \
  ghcr.io/lucaswilliameufrasio/discord-mcp-relay:v0.1.1
docker ps --filter name=discord-mcp
```

O volume precisa ser gravável pelo UID/GID 10001 da imagem. Ao recriar o
container principal, recrie também o relay para que compartilhe seu novo
namespace de rede. Planeje a atualização e o rollback antes de alterar os
containers; não remova o volume ou a chave. Este caminho não exige Compose.

## Opção 3: systemd

Use somente o binário da aplicação: **não são necessárias as imagens nem o
relay**. O próprio processo escuta no loopback do host. Os passos exigem acesso
administrativo e devem ser adaptados à distribuição e às políticas do host.

1. Em um diretório de trabalho, escolha a arquitetura e baixe e confira o
   executável e o unit da mesma versão:

   ```sh
   VERSION=v0.1.1
   case "$(uname -m)" in
     x86_64) TARGET=x86_64-unknown-linux-gnu ;;
     aarch64) TARGET=aarch64-unknown-linux-gnu ;;
     *) printf '%s\n' 'Arquitetura não suportada' >&2; exit 1 ;;
   esac
   BASE="https://github.com/lucaswilliameufrasio/discord-mcp/releases/download/$VERSION"
   FILE="discord-mcp-$VERSION-$TARGET.tar.gz"
   curl -fsSLO "$BASE/$FILE"
   curl -fsSLO "$BASE/$FILE.sha256"
   sha256sum -c "$FILE.sha256"
   tar -xzf "$FILE"
   curl -fsSLo discord-mcp.service "https://raw.githubusercontent.com/lucaswilliameufrasio/discord-mcp/$VERSION/deploy/systemd/discord-mcp.service"
   ```

2. Revise o [`unit`](../deploy/systemd/discord-mcp.service), confirme que a
   conta e o grupo `discord-mcp` não estão em uso por outra aplicação e crie-os
   se necessário. Por exemplo, em distribuições com `groupadd` e `useradd`:

   ```sh
   sudo groupadd --system discord-mcp
   sudo useradd --system --gid discord-mcp --home-dir /var/lib/discord-mcp --shell /usr/sbin/nologin discord-mcp
   sudo install -m 0755 discord-mcp /usr/local/bin/discord-mcp
   sudo install -m 0644 discord-mcp.service /etc/systemd/system/discord-mcp.service
   sudo install -d -m 0700 /etc/discord-mcp
   sudo install -m 0600 /caminho/protegido/service.env /etc/discord-mcp/service.env
   ```

   O unit cria `/var/lib/discord-mcp` e usa nele o banco `channels.db`. Os
   comandos de criação de conta são para uma conta nova; se ela já existir,
   confira seu proprietário e uso antes de prosseguir. Confirme também as
   permissões de diretórios preexistentes.
3. Não sobrescreva `DISCORD_MCP_DB` ou `DISCORD_MCP_LISTEN` no arquivo de
   ambiente sem revisar o unit: `EnvironmentFile` pode sobrescrevê-los.
   Após revisar os caminhos e permissões, execute:

   ```sh
   sudo systemctl daemon-reload
   sudo systemctl enable --now discord-mcp
   sudo systemctl status discord-mcp
   ```

   Inspecione logs com cuidado e valide o acesso pelo proxy externo. Para
   atualização, faça backup e substitua o binário apenas após conferir sua
   arquitetura e checksum; preserve banco e chave.

O modo HTTP e as integrações com Discord e provedores reais devem ser validados
no ambiente escolhido. Consulte [Operação](operations.md) para administração,
rotação, recuperação e limites de segurança; o guia não executa alterações no
servidor nem decide sobre uso em produção.
