# Exercício isolado de recuperação SQLite

Este exercício qualifica somente um snapshot local de dados sintéticos; **não é
um procedimento de restauração de produção nem prova recuperação de desastre**.
Não recebe caminho de banco, segredo ou backup real. O teste cria seus próprios
arquivos em um diretório temporário privado e os remove ao terminar normalmente.
Interrupção forçada pode deixar arquivos temporários do próprio teste.

## Dependência e execução

O exercício usa o exemplo Rust `snapshot-restore` do checkout irmão
`../sqlite-replication`. Ele usa a API de backup online do SQLite para criar um
snapshot LTX e `restore_if_missing` para restaurar em um novo caminho. A
aplicação não tem dependência em tempo de execução dessa biblioteca: o exemplo
é um processo separado fornecido apenas ao teste.

Na raiz deste projeto, com o checkout irmão e sua toolchain disponíveis:

```sh
cargo build --locked --manifest-path ../sqlite-replication/rust/Cargo.toml -p sqlite-replication-core --example snapshot-restore --target-dir "$PWD/target/replication-drill"
DISCORD_MCP_REPLICATION_EXAMPLE="$PWD/target/replication-drill/debug/examples/snapshot-restore" cargo test --test replication_drill -- --ignored --nocapture
```

O teste é explicitamente opt-in e não faz parte do `cargo test` comum. Seu
processo de replicação tem limite de 30 segundos, e cada interação com o MCP
restaurado tem limite de cinco segundos. Ele não carrega configuração de
produção nem faz requisições ao Discord: as ferramentas são invocadas com
entradas inválidas para verificar o carregamento sem efeitos externos.

## Preparar uma cópia restaurada

Os comandos abaixo descrevem a ferramenta, não autorizam acesso a backups reais
ou substituição de uma instância existente. Primeiro restaure usando uma versão
qualificada do `sqlite-replication` em um arquivo **novo**, com todos os processos
que possam abrir ou modificar essa cópia parados. A ferramenta exige um banco
completo offline sem arquivos `-wal`, `-shm` ou `-journal` e diretórios de origem
e destino privados. A ausência desses arquivos **não prova** que não há processo
usando o banco: garantir isolamento e ausência de escritores é responsabilidade
do operador. Não aponte para o banco ativo. A chave de 32 bytes da aplicação
deve ser fornecida separadamente por `DISCORD_MCP_KEY`.

```sh
discord-mcp recovery-prepare /diretorio-privado/restaurado.db /diretorio-privado/preparado.db
```

A preparação valida integridade, esquema e chave, copia via SQLite Backup API,
invalida permissões, tokens MCP e códigos OAuth e publica um arquivo novo em
quarentena, sem sobrescrever destino ou sidecars existentes. Ela rejeita bancos
com views ou triggers, e limita o tamanho do arquivo e das páginas a 512 MiB.
O prazo de 30 segundos é verificado entre etapas, **não é um timeout rígido**
para operações SQLite, integridade ou sincronização de disco. Os arquivos
temporários são colocados em um subdiretório privado removido na saída normal;
interrupções abruptas ou falha de limpeza podem deixá-los no disco. Trate esses
arquivos como sensíveis e não os remova sem verificar propriedade e escopo.

A cópia preparada recusa inicialização stdio, web e servidor e alterações
ordinárias até a ativação explícita. Revise o ponto de recuperação, identidade
do banco, canais, credenciais cifradas, configuração, versão e eventos de
auditoria; reconcilie também revogações ocorridas após o snapshot. **Permissões e
tokens anteriores permanecem invalidados**; sua recriação deve seguir a política
do operador. A cópia conserva tokens de bot e URLs de webhook cifrados: esses
segredos podem ter sido revogados ou substituídos desde o snapshot e devem ser
revisados antes de habilitar qualquer integração. A própria ativação não prova
que essa revisão ocorreu e não valida o estado no Discord.

Somente após a revisão, com `DISCORD_MCP_DB` apontando para a cópia preparada e
a mesma chave configurada, o operador pode executar:

```sh
discord-mcp recovery-activate --confirm
```

Ativação é auditada e não restaura as permissões ou credenciais MCP antigas.
Metadados e páginas livres do SQLite podem conter dados históricos, inclusive
hashes de tokens e permissões removidas; a quarentena não é sanitização forense.
Uma falha após a publicação mas antes do `fsync` do diretório é relatada
separadamente: inspecione o destino publicado antes de tentar novamente, sem
sobrescrevê-lo. Proteja cópias, WAL, LTX e arquivos temporários como dados
sensíveis; não há isolamento contra processos com acesso ao mesmo usuário.

## Verificações

- Snapshot enquanto uma conexão mantém dados sintéticos confirmados no WAL;
  `PRAGMA integrity_check` e leitura de um marcador após o restore.
- Recuperação de canais webhook e bot cifrados com uma chave sintética guardada
  separadamente, permissões independentes, eventos de auditoria e métricas.
- Recusa de chave incorreta; inicialização do MCP restaurado e resolução dos
  canais sem enviar ou consultar mensagens.
- Preparação do arquivo produzido pela replicação, bloqueio durante quarentena,
  invalidação de permissões, token e código OAuth, ativação explícita e
  verificação de que os acessos não retornam após ativação.
- Recusa de sobrescrever arquivo existente; comparação do arquivo protegido e
  dos dados e permissões da origem após a tentativa.

Na execução de desenvolvimento, o exemplo foi compilado do checkout local
`a1522d82e8ddd1b9e6f34126a6dab331dd323474` com alterações não commitadas no
projeto irmão. Esse commit não estava disponível no remoto quando verificado;
portanto a execução não constitui uma dependência publicada ou reproduzível a
partir de um commit remoto. O teste não modifica os arquivos-fonte desse checkout.

## Limites e trabalho restante

O exemplo faz somente snapshot e restore local; não configura replicação
incremental, envio remoto, criptografia do repositório, retenção nem recuperação
de host. O LTX pode conter metadados sensíveis, mesmo que webhooks e tokens de
bot estejam cifrados no banco. Uma implantação precisa proteger também o
repositório, manter a chave da aplicação e as ferramentas de recuperação fora
do domínio de falha e definir como recuperar configuração e credenciais.

Uma restauração antiga bruta pode ressuscitar permissões ou tokens MCP revogados
após o snapshot; nunca inicie a aplicação diretamente sobre a cópia bruta. A
preparação acima invalida esses acessos na cópia, mas não reconcilia o estado
externo ou transforma o exercício em um procedimento de produção aprovado.
Não há RPO/RTO de produção medido. Corrupção/truncamento de arquivos, ausência
de chave, falta de espaço, interrupção, recuperação remota e perda de host ainda
não foram qualificados neste projeto. Restauração real ou sobrescrita de dados
existentes exige autorização e plano específicos.
