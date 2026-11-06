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

Execução em 2026-11-05: os dois comandos acima concluíram com sucesso; o teste
`snapshot_and_restore_isolated_synthetic_wal_database` passou (1 teste, 0,34 s).
O checkout irmão estava em `f5c0984f069407913365136a10d5962f9a33930d` com
alterações não commitadas, inclusive em código Rust da biblioteca; portanto,
esse resultado não identifica uma árvore reproduzível nem qualifica uma versão
publicada. A duração é apenas do teste, não um RTO. O exercício confirma somente
o cenário local sintético descrito, não perda de host ou recuperação operacional.

### Exercício opt-in S3 simulado

`bash drills/s3/run.sh` compila um programa isolado contra os checkouts locais
do Discord MCP e `../sqlite-replication`, inicia um contêiner Floci próprio e
descartável em `127.0.0.1`, executa o exercício e para somente esse contêiner.
São necessários Cargo, Python 3, `timeout`, contexto Docker `rootless` realmente
acessível e a imagem `floci/floci:2.3.0` no digest fixado no script já
disponível localmente. O script não faz pull, usa porta aleatória e não usa
volumes ou recursos S3 preexistentes; a imagem declara um volume anônimo, que
o Docker remove com o contêiner `--rm`. Seu processo de teste recebe ambiente
mínimo e credenciais sintéticas, e rejeita qualquer endpoint que não seja
`127.0.0.1` com porta explícita. Não forneça credenciais ou dados reais.

O subprojeto `drills/s3` tem `Cargo.lock` próprio, mas as bibliotecas de
replicação são obtidas do checkout irmão por caminho, não fixadas pelo lockfile.
Para reproduzir, registre sua revisão e alterações locais antes de executar;
essa dependência não integra nem habilita replicação operacional no aplicativo.
O programa cria sua própria base sintética, snapshot e bucket, envia o
repositório ao Floci e restaura em um caminho novo. Em seguida prepara outra
cópia, verifica quarentena e invalidação de permissões, tokens e códigos, ativa
explicitamente e verifica que não reaparecem. Uma tentativa contra destino
existente deve falhar sem modificá-lo; o teste também confirma que revogações
feitas na fonte após o snapshot permanecem na fonte. O tempo exibido exclui
build e inicialização do emulador, portanto não é RTO.

O exercício também cria uma segunda geração de snapshot completo após revogar
permissões e token na fonte. Essa geração usa outro repositório e outro prefixo
S3: ambos os snapshots iniciais têm TXID 1, de modo que reutilizar o prefixo
conflitaria com chaves imutáveis. A segunda cópia é restaurada e colocada em
quarentena; suas revogações são verificadas após a ativação. O snapshot anterior
continua restaurável e sua autoridade é invalidada pela quarentena. Isso testa
dois snapshots independentes, **não** captura contínua ou incremental.

O isolamento usa contêiner e diretório temporário próprios; interrupções abruptas
podem deixar recursos do exercício, que devem ser identificados antes de limpar.
O emulador local não valida IAM, criptografia de objetos, rede, consistência,
latência ou políticas de qualquer provedor real, nem perda de host. O restore
S3 da biblioteca irmã baixa o repositório inteiro para staging temporário e
exige espaço proporcional; limites de tamanho para um repositório remoto não
são demonstrados por este exercício.

Execução local em 2026-11-05: `sqlite-replication` na revisão local
`3553f91cce2535b4fbe521c6582bec64db94477d`, com alterações preexistentes em
documentação e testes, e Discord MCP em `4dfdde4` mais o subprojeto deste
exercício. Docker rootless e imagem pelo digest do script. Duas execuções
passaram: snapshot TXID 1 e dois objetos confirmados, restore e reconciliação,
com 0,183 s e 0,173 s para o trecho medido. Esses tempos não incluem compilação,
emulador, obtenção de credenciais ou configuração, e não são RTO. Naquela
campanha não foram testados restart do Floci, truncamento de objetos, falhas
de rede, recuperação de host ou provedores reais. O checkout irmão foi usado sem modificar seus
arquivos; sua revisão e seu estado precisam ser preservados independentemente
para reproduzir o resultado.

### Falhas sintéticas — 2026-11-05

O teste local `corrupt_channel_ciphertext_rejects_staged_restore_without_touching_source`
qualifica separadamente a verificação de credenciais do preparo offline: em um
banco sintético com dois canais, substitui o ciphertext de um deles mantendo o
`integrity_check` do SQLite válido. Com a chave correta, a preparação retorna
`InvalidKeyOrData`, não publica destino nem sidecars, remove o staging normal e
preserva byte a byte a fonte; o outro canal e sua permissão seguem acessíveis.
Esse teste não exercita queda brusca, fonte ativa, perda de chave ou recuperação
de infraestrutura e não demonstra disponibilidade de backups reais.

O teste `recovery_prepare_cli_wrong_key_cleans_staging_and_allows_safe_retry`
executa a CLI real sobre uma imagem sintética offline, sem serviços externos.
Uma chave incorreta retorna erro, preserva a fonte byte a byte e não deixa
destino, sidecars ou diretório de staging. Uma nova execução com a chave correta
publica uma cópia em quarentena e invalida a concessão; uma terceira execução
recusa sobrescrever essa cópia e preserva seus bytes. O teste não cobre crash,
custódia de chave, recuperação de backup real ou ativação em produção.

O teste `activation_cli_rejects_reintroduced_authority_and_preserves_source`
prepara três cópias sintéticas em quarentena e simula, separadamente, um escritor
externo reintroduzindo uma permissão, um token ou um código. Em cada caso,
`recovery-activate --confirm` falha, a quarentena e o registro injetado são
preservados e não há auditoria de ativação bem-sucedida; a fonte offline é
comparada byte a byte. Esse teste não autoriza corrigir dados reais manualmente
nem comprova isolamento contra o escritor que introduziu os registros.

O teste `recovery_activation_serializes_authority_reintroduction` usa duas
conexões SQLite e uma pausa de trace restrita ao teste para verificar as duas
ordens de concorrência. Uma permissão confirmada antes de `BEGIN IMMEDIATE`
impede a ativação, mantendo quarentena e ausência de auditoria de ativação;
após a reserva e a checagem de autoridade, a inserção concorrente recebe
`SQLITE_BUSY`, e a ativação confirma estado e uma auditoria de sucesso. Uma
inserção externa depois do commit consegue conceder acesso: serialização não é
isolamento de processos nem impede autoridade indevida posteriormente.

Duas execuções adicionais incluíram prefixos exclusivos com o manifesto presente
mas LTX ausente, e com o mesmo manifesto mais LTX truncado. Cada restore retornou
erro e não publicou banco nem sidecars no destino. Em outro cenário, um endpoint
TCP loopback próprio recebeu bytes da primeira requisição S3 e ficou bloqueado;
o teste então cancelou a tarefa de restore, aguardou seu encerramento e verificou
que não havia banco nem sidecars publicados. Após cada falha, comparou os nomes
e a acessibilidade dos canais e confirmou que leitura, envio e token revogados
na fonte continuavam revogados. O destino protegido também permaneceu intacto.

As execuções terminaram em 0,130 s e 0,131 s no trecho medido, sem build nem
inicialização do emulador; isso não é RTO. Após os testes, a revisão observada
no checkout irmão era `f5c0984f069407913365136a10d5962f9a33930d`, com alterações
preexistentes em documentação e testes. A revisão do checkout pode mudar durante
uma execução, portanto o registro não prova que todas as dependências foram
compiladas a partir de uma árvore imutável. Não houve alteração intencional no
checkout irmão. Não foram exercitados download parcial de um objeto, cancelamento
durante publicação local, SIGKILL/crash, retomada, remoção de staging após queda
brusca, restart do Floci, falhas reais de rede, perda de host ou provedores reais.

### Investigação da integração operacional — 2026-11-05

#### Revisões de permissões e restauração sintética

`tests/legacy_permission_recovery.rs` usa um banco criado pela aplicação atual
com a tabela de revisões removida, não uma captura de uma versão antiga. Os
testes verificam migração e aberturas simultâneas sem perda das permissões da
fonte, quarentena da cópia, invalidação das permissões restauradas e ativação
isolada. Tabelas de revisão com colunas, chave primária ou chave estrangeira
incompatíveis, revisões inválidas ou registros órfãos são recusadas antes da
publicação. A validação confere os dados existentes na abertura; não transforma
escritores externos ou versões antigas em participantes do controle de revisão.

A criação da tabela pela aplicação inclui `CHECK(revision >= 0)`, mas a
validação de uma tabela já existente **não exige nem verifica a presença desse
CHECK**. Uma tabela sem ele é aceita se as colunas, a chave primária, a chave
estrangeira e os dados existentes forem válidos; uma cópia assim também pode ser
publicada em quarentena. Os testes demonstram que outro cliente SQLite pode
inserir uma revisão inválida enquanto a conexão permanece aberta. Consultas de
contexto e formulários de permissões e alterações de permissões agora validam
a revisão do canal no mesmo snapshot ou transação e recusam valores negativos
ou que não sejam inteiros, sem gravar permissões nem auditoria de sucesso. Uma
nova abertura ou preparação de cópia também recusa dados inválidos. Isso não
impede a escrita externa nem detecta valores válidos que burlem a revisão, e
não é uma validação contínua de todos os canais em toda operação.
O lock consultivo não protege contra esse cliente; o controle concorrente
depende de todos os escritores usarem a aplicação atual e de um limite de
acesso apropriado no sistema operacional.

O teste sintético `exhausted_revision_rejects_permission_changes_atomically`
confirma que a revisão pode chegar ao máximo de INTEGER do SQLite, mas não é
incrementada além dele: alterações e revogações subsequentes, com ou sem
comparação otimista, falham sem modificar concessões ou auditar sucesso. Outro
canal continua operável. O teste não estabelece procedimento operacional para
recuperar um canal cuja revisão esteja esgotada.

O teste `invalid_revision_blocks_admin_form_and_approved_real_cli` exercita o
roteador administrativo e a CLI executável com Discord sintético: após abrir
o formulário e aprovar a CLI, um cliente SQLite escreve revisão negativa em
uma tabela sem `CHECK`. O formulário falha com HTTP 500, a CLI falha sem abrir
seu próprio banco e as concessões e auditorias de sucesso permanecem iguais.
Os handlers abrem novas conexões, logo esse teste inclui a validação de abertura;
o teste de armazenamento com conexão mantida aberta cobre o caso ativo separado.

O teste sintético `permission_transaction_serializes_competing_revision_writer`
pausa a operação real por trace SQLite, após `BEGIN IMMEDIATE` e a checagem da
revisão, imediatamente antes de incrementá-la. Uma segunda conexão, sem espera,
recebe `SQLITE_BUSY` ao tentar alterar a revisão. Após liberar a operação, o teste
confirma o incremento, a permissão e uma única auditoria de sucesso. A segunda
conexão consegue escrever depois do commit: essa evidência comprova serialização
da transação testada, não exclusividade nem proteção contra alterações externas
posteriores. A pausa e o trace existem apenas no teste e não no runtime.

O teste `invalid_revision_committed_before_write_reservation_is_rejected` cobre
a ordem inversa: uma tabela sintética sem `CHECK` é corrompida e confirmada por
outra conexão enquanto a operação real está pausada imediatamente antes de
executar `BEGIN IMMEDIATE`. Após retomar, a operação recusa a revisão negativa,
preserva as permissões e não adiciona auditoria de sucesso. O teste não presume
que o banco esteja protegido contra escritores não cooperativos.

O exercício filesystem opt-in passou usando o binário já existente em
`target/replication-drill/debug/examples/snapshot-restore`. Sua proveniência
não está fixada: o resultado não comprova build reproduzível da biblioteca,
captura incremental, isolamento de todos os escritores ou recuperação
operacional. Nenhum serviço ou backup real foi usado.

#### Requisitos de execução esclarecidos pelo usuário

**Mapa de acessos revisto em 2026-11-06:** no modo HTTP, `src/server.rs`,
`src/admin_web.rs`, `src/oauth_web.rs` e `src/http_auth.rs` abrem conexões no
processo do serviço. `remote-set-permission` entra antes de carregar a
configuração SQLite e usa HTTP; o teste `cli_real_router_discord_login_and_consent`
exercita a CLI real e o roteador de produção com Discord sintético, inclusive
expiração depois do consentimento mediante um hook compilado somente em testes.
No Linux, processos do binário agora obtêm um lock consultivo no inode do banco:
`serve` exige exclusividade e processos locais usam locks compartilhados. Um
teste com processos reais verifica exclusão mútua, liberação e coexistência de
duas instâncias stdio. A exclusividade é apenas cooperativa: SQLite aberto por
outra ferramenta ignora o lock, a substituição do arquivo pode contorná-lo e
outras plataformas não usam essa proteção. Portanto ela não impõe propriedade
global nem permite liberar captura incremental. A preparação de recuperação
também opera diretamente em cópias e exige
manter a separação documentada entre cópia privada e banco ativo. Não tratar a
mediação de uma operação como prova de exclusividade global.

- No modo remoto, um serviço dedicado deve ser o único responsável pelo banco;
  CLI e outros processos devem se comunicar com ele, em vez de acessar esse
  SQLite diretamente.
- No modo stdio, a execução é local e deve permitir várias instâncias. Não se
  pode exigir instância única como solução para coordenação ou recuperação.

Esses são requisitos de destino, **não garantias já implementadas**. Atualmente
há aberturas de `ChannelStore` em `src/main.rs`, `src/admin_web.rs`,
`src/oauth_web.rs` e `src/http_auth.rs`. É necessário identificar e coordenar
todos os acessos, definir e verificar a fronteira do serviço remoto e resolver a
coordenação entre processos locais no stdio. A possibilidade de processos fora
do protocolo acessarem o mesmo arquivo continua exigindo um modelo de confiança
e evidência adequados. O gate experimental e a captura incremental não estão
integrados; não liberar reconciliação operacional apenas com esses requisitos.

O teste `multiple_stdio_instances_observe_shared_channel_changes`, em
`tests/stdio.rs`, inicia dois processos stdio reais com o mesmo banco sintético,
registra um canal após ambos inicializarem e verifica que os dois resolvem esse
canal em requisições MCP locais. O teste não faz chamadas ao Discord. Esse
trecho verifica compartilhamento de leitura, não coordenação da captura,
exclusividade global ou recuperação operacional.

O mesmo teste também mantém as duas instâncias stdio ativas enquanto dois
processos CLI cadastram canais distintos no mesmo banco. Ambos os cadastros
devem terminar e os dois processos MCP devem observar os dois canais. A
transação de cadastro usa `BEGIN IMMEDIATE` antes da verificação de quarentena,
evitando a promoção de uma leitura diferida para escrita no WAL; a espera por
contenção continua limitada pelo `busy_timeout` do armazenamento. Isso cobre
cadastros concorrentes nesse cenário local, não todos os métodos de escrita nem
fencing para captura incremental.

A exclusão de canal também reserva a escrita com `BEGIN IMMEDIATE` antes de
verificar quarentena e permissões. Um teste com subprocesso mantém outra
transação escritora aberta, verifica que a exclusão aguarda, libera a reserva
e confirma remoção do canal e das permissões e os eventos de auditoria de
revogação e exclusão. Antes dessa alteração, a transação diferida falhava ao
tentar promover a leitura para escrita sob contenção. A espera continua sujeita
ao limite configurado; isso não implementa exclusividade para replicação.

Uma auditoria das transações restantes em `ChannelStore` encontrou que as
transações diferidas de auditoria e métricas começam com escrita; a de resolução
de canal autorizado é somente leitura. A emissão de token e código OAuth,
entretanto, verificava quarentena em leitura separada antes de inserir. Um teste
com subprocesso e reserva SQLite reproduziu emissão após outra conexão confirmar
quarentena. As inserções agora verificam quarentena atomicamente na própria
instrução; a troca de código também verifica novamente após reservar a escrita.
O teste cobre token, código e troca concorrentes com quarentena, sem credenciais
reais. Ele não comprova exclusividade de todos os escritores nem recuperação
operacional; a captura incremental continua desabilitada.

`verify_access_token` agora consulta o token e a condição de quarentena na
mesma instrução SQLite, portanto usa um único snapshot para essa decisão. Um
teste sintético reproduz leituras separadas que poderiam combinar o estado
anterior da quarentena com um token ainda presente no estado posterior; em
seguida, a validação real nega esse token. O teste retém o token artificialmente:
a quarentena normal da aplicação também invalida as credenciais em sua própria
transação, e não foi demonstrado bypass pelo fluxo normal. Uma validação já
concluída antes da mudança não é revogada retroativamente; esta alteração não
estabelece coordenação entre processos nem recuperação operacional.

Um teste de unidade adicional usa o rastreamento de instruções do SQLite para
pausar a consulta de token real imediatamente antes da execução, confirmar a
quarentena por outra conexão e então retomá-la. A versão anterior, de consultas
separadas, foi executada em cópia isolada e retornou o usuário; a consulta única
retornou ausência de autorização. O token foi mantido deliberadamente no banco
para injetar o estado inconsistente, não para simular a quarentena normal. A
suíte principal foi executada em namespace de rede separado com loopback próprio,
sem acesso à rede externa; o exercício filesystem opt-in permaneceu ignorado.

`get_channel` também consultava a quarentena e depois o segredo em instruções
separadas. Um teste pausa a consulta real do canal, confirma a quarentena em
outra conexão e demonstra que a versão anterior ainda retornava o canal. A
consulta agora obtém quarentena e registro no mesmo snapshot e mantém a
precedência de `RecoveryPending` mesmo quando o canal não existe. Na aplicação,
`authorized_channel` já resolve permissão e destino numa transação de leitura;
um snapshot iniciado antes de outra transação confirmar quarentena pode concluir
com o estado anterior. O fluxo de preparação normal exige cópia privada: esse
ensaio concorrente não demonstra publicação simultânea suportada nem revogação
de operações já autorizadas, e não qualifica captura ou recuperação operacional.

Um teste adicional de `authorized_channel` pausa a consulta do destino depois
que a transação já leu a permissão. Outra conexão confirma, atomicamente, a
quarentena e a remoção de permissões, tokens e códigos. A operação pausada ainda
resolve o canal usando o snapshot anterior; uma nova autorização é recusada e
uma nova leitura direta do canal retorna `RecoveryPending`. O teste não envia
mensagens nem acessa Discord. Ele confirma o limite de operações em andamento,
não uma garantia de revogação instantânea. A preparação de recuperação suportada
continua restrita a uma cópia privada, não a uma transição em banco ativo.

O checkout irmão observado em `f5c0984f069407913365136a10d5962f9a33930d`
oferece `snapshot`, `sync_to_s3`, `restore_from_s3` e `capture_once`, mas essa
revisão não foi encontrada pela API do GitHub. `git ls-remote origin
refs/heads/main` retornou `25e2bd16d99aa005b6c0fead064773a0aee5dd1a`; o
`origin/main` local correspondente expõe `snapshot`, mas não expõe publicamente
`sync_to_s3`, `restore_from_s3` ou `capture_once`. Há uma função interna
`pub(crate) capture_once`; ela não é API pública. Portanto, fixar a revisão local como
dependência Git não produziria um build reproduzível a partir do remoto.
Não houve publicação ou modificação do checkout irmão nesta investigação.

Nova consulta somente leitura em 2026-11-05: `git ls-remote --heads origin`
mostrou `main` ainda em `25e2bd16d99aa005b6c0fead064773a0aee5dd1a` e a branch
`fix/conformance-crc-and-cp01` em `9c9201e64c19a55f18fe4df1abe29a33a122bbc8`;
`git ls-remote --tags origin` não retornou tags. O `lib.rs` dessas duas revisões
publicadas mantém `capture` como `pub(crate)` e declara que a API não expõe
replicação WAL. Não foi identificada nelas a API pública incremental/S3 usada
pelas fixtures. O checkout irmão local ainda estava em `f5c0984f...` com
alterações preexistentes, inclusive em código de captura; ele não foi modificado
ou publicado. Uma revisão publicada e utilizável continua pendente.

A captura incremental da biblioteca exige um write-gate para **todos** os
escritores, um único escritor lógico coordenado, plataforma e SQLite dentro do
escopo CP-01. O Discord MCP abre conexões SQLite em múltiplos caminhos e
processos, inclusive CLI, servidor, métricas e auditoria; ainda não há gate
compartilhado nem coordenação de ciclo de vida que comprovem essa precondição.
Não conectar `capture_once` como se essas garantias já existissem.

Snapshots completos isolados podem ser uma alternativa periódica, mas exigem
projeto adicional: catálogo durável e seleção de gerações, nomes/prefixos
únicos, retry idempotente, exclusão mútua, limite de espaço e retenção,
proteção de metadados e credenciais, estado de sync visível, recuperação após
interrupção e política para o intervalo sem proteção. O teste atual não fornece
esses mecanismos nem mede RPO/RTO. Uma integração real depende de uma revisão
publicada e fixável da biblioteca, das garantias de escrita e da definição do
destino, custódia de chaves e política operacional pelo operador. Nenhum dos
exercícios autoriza implantação ou acesso a um bucket real.

Duas execuções do exercício ampliado de duas gerações passaram em Floci,
respectivamente em 0,185 s e 0,181 s para o trecho sintético medido, sem build
ou inicialização do emulador. São evidência do fluxo testado, não de durabilidade
remota, desempenho de produção ou recuperação de host.

### Investigação de coordenação de escritores — 2026-11-05

`tests/sqlite_coordination.rs` usa bancos temporários separados, processos reais
da CLI e do servidor em loopback, identidades sintéticas e nenhuma chamada
válida ao Discord. Uma conexão independente mantém `BEGIN IMMEDIATE` enquanto
outra tenta conceder permissão. Com a reserva ativa, o processo CLI permaneceu
pendente por 400 ms, uma segunda reserva com timeout zero falhou e uma leitura
WAL viu o estado anterior; após `ROLLBACK`, a CLI terminou e a permissão e sua
auditoria apareceram. Uma reserva mais longa levou a CLI a falhar no prazo
limitado, sem conceder a permissão ou registrar alteração bem-sucedida. Em outro
teste, uma requisição MCP ao servidor permaneceu pendente durante a reserva e
concluiu após a liberação; a métrica de leitura foi persistida. Os testes usam
esperas de observação e demonstram o comportamento dos caminhos exercitados,
não uma prova formal de que cada processo atingiu um ponto específico interno
ao SQLite antes da observação.

Este resultado **não é um write-gate** nem garante que todos os escritores
estejam coordenados. A abertura de `ChannelStore` faz inicialização e
`INSERT OR IGNORE` para verificar a chave, além de usar `busy_timeout` de cinco
segundos; portanto uma reserva SQLite pode bloquear até caminhos aparentemente
de leitura e fazer operações falharem. A coleta de métricas é de melhor esforço
e pode perder eventos se a gravação falhar. Há múltiplos processos/caminhos:
CLI de canais e permissões, stdio e web locais, servidor HTTP, OAuth, métricas,
auditoria, inicialização/limpeza e recuperação; abrir um SQLite diretamente por
fora da aplicação também não participa de qualquer protocolo futuro. Os testes
atuais não cobrem todos esses caminhos, checkpoint, crash, concorrência alta,
nem falhas de rede ou efeitos Discord em andamento.

É especialmente incorreto simplesmente manter `BEGIN IMMEDIATE` na conexão do
gate enquanto chama `capture_once`: o próprio adaptador de captura do projeto
irmão tenta adquirir outra reserva `BEGIN IMMEDIATE` em sua conexão. Isso
produziria contenção, não a exclusão mútua cooperativa exigida. Um mecanismo
real precisaria impedir novas escritas em **todos** os processos participantes,
aguardar as já admitidas, lidar com morte/reinício de processos e falha de
liberação, e permitir que a própria captura obtenha sua reserva SQLite. Seu
contrato, integração, limites de espera e falhas precisam de projeto e testes
antes de uso. Um snapshot completo via SQLite Backup API é uma operação distinta
da captura incremental do WAL; os testes de snapshot existentes não demonstram
que esse gate incremental foi implementado.

### Protocolo cooperativo experimental — 2026-11-05

`cargo test --test cooperative_gate_prototype` executa uma experiência isolada
entre subprocessos, com arquivos e SQLite sintéticos em diretório temporário.
Ela não altera os caminhos de escrita da aplicação nem chama `capture_once`.
O desenho pesquisado usa dois locks de arquivo separados e um byte de estado:

1. O escritor obtém lock compartilhado de admissão, verifica o estado aberto,
   obtém lock compartilhado de atividade e libera a admissão. Ele mantém o
   lock de atividade durante toda a transação SQLite, inclusive commit.
2. O coordenador obtém a admissão exclusivamente, grava e sincroniza o estado
   fechado e depois obtém o lock de atividade exclusivamente. Isso impede
   novas admissões e espera os escritores que já entraram terminarem.
3. Depois da drenagem, o experimento confirma que outra conexão consegue obter
   `BEGIN IMMEDIATE` no SQLite; o próprio gate não mantém essa reserva. O
   coordenador só marca aberto novamente enquanto ainda controla a admissão e
   após um comando explícito no teste.

Os testes verificam fechamento antes da drenagem, entrada de um escritor
posterior somente após reabertura, commit da escrita anterior, rollback SQLite
após morte do escritor, morte do coordenador antes e depois da drenagem, recusa
enquanto o estado permanece fechado, reinício e reabertura explícita. Estado
inválido faz o processo de teste falhar antes de admitir a escrita. Locks são
consultivos: a prova vale apenas para processos participantes que abrem os
mesmos arquivos e seguem a mesma ordem. Os subprocessos são sincronizados por
mensagens explícitas e encerrados pelo próprio teste; não há dados reais.

Ampliação dos testes: arquivos de estado ou lock ausentes e estado vazio são
recusados sem recriação; três escritores já admitidos terminam suas transações
antes da drenagem, enquanto outro aguarda; e dois coordenadores não entram na
captura simultaneamente. Falhas são injetadas no processo de teste depois da
gravação do byte e antes de chamar `sync_all`, tanto ao fechar quanto ao reabrir.
Não há falha real de disco ou perda de energia nessas injeções.

**Lacuna demonstrada:** na reabertura, uma falha entre gravar o byte aberto e
sincronizá-lo encerra o coordenador e libera seus locks; outro processo pode
observar o byte aberto e escrever mesmo sem confirmação de persistência. O
teste registra deliberadamente esse comportamento inseguro — passar não quer
dizer que a reabertura seja segura. Na falha equivalente ao fechar, outro
processo viu o byte fechado no mesmo sistema em execução; isso não prova que o
fechamento sobreviva a queda de energia. O protocolo deve ser redesenhado e
revalidado antes de qualquer integração, não tratado como solução fail-closed.

#### Experimento alternativo: autoridade por processo

`cargo test --test authority_gate_prototype` exercita outro desenho, apenas em
subprocessos sintéticos. Um coordenador temporário em socket Unix começa sempre
fechado, inclusive após reinício; o único marcador escrito no disco diz
`closed`. A reabertura modifica somente memória depois de uma escrita e
`sync_all` bem-sucedidos. Uma falha sintética após a escrita, mas antes do
`sync_all`, mantém a autoridade fechada; falha equivalente no início impede a
publicação do socket. Para a captura, a autoridade fecha a admissão, aguarda
conexões de escritores admitidos terminarem e exige conclusão explícita antes de
aceitar reabrir. O teste verifica falhas de fechamento e reabertura, reinício,
drenagem e recusa durante a captura. A falha injetada não é uma falha real do
filesystem e não qualifica perda de energia.

Esse desenho evita, nos casos exercitados, publicar um **byte aberto** antes de
saber que sua persistência teve sucesso, mas **ainda não é um gate seguro para
produção**. A morte do coordenador fecha novas admissões após reinício, porém
não revoga transações SQLite já admitidas: um teste demonstra que uma transação
sintética pode confirmar após a morte e reinício do coordenador. A autoridade
perde a contagem desses escritores; reabri-la sem reconciliação e uma barreira
que abarque escritores anteriores pode violar a captura. O fixture também não
implementa autenticação de pares, exclusão de coordenadores concorrentes,
tratamento seguro de socket obsoleto, deadlines, persistência de diretório,
defesa contra processos externos ou coordenação real dos módulos da aplicação.
Não há garantia de crash/power-loss ou de segurança em filesystem de rede.
Nenhuma autoridade ou comando deste teste é integrado à CLI ou ao servidor.

Um teste adicional mantém a autoridade viva e encerra somente o socket de um
cliente admitido, deliberadamente sem lock cooperativo, enquanto sua transação
SQLite continua aberta. A autoridade responde `CLOSED` e `DRAINED`; em seguida,
o cliente ainda confirma a transação, embora novas admissões sejam recusadas.
Assim, EOF no canal de controle não prova o fim da escrita, mesmo sem morte ou
reinício da autoridade. Este cenário não contradiz a barreira cooperativa abaixo:
ela depende de todos os escritores manterem o lock até terminarem a transação.

#### Barreira de atividade após reinício

O mesmo fixture agora experimenta um lock compartilhado de atividade mantido
por um subprocesso escritor desde antes da admissão até depois do commit e
fechamento SQLite. A autoridade reiniciada tenta obter esse lock em modo
exclusivo antes de reabrir ou declarar a drenagem. Nos testes sintéticos, uma
transação anterior ao reinício bloqueou `OPEN` e `RETRY_DRAIN`; após commit,
a drenagem e a reabertura explícita ocorreram. Quando o escritor foi morto, o
lock foi liberado e o SQLite reverteu sua transação. O teste anterior que não
usa o lock continua demonstrando que um escritor não cooperativo pode confirmar.

O socket perdido não cancela por si só o escritor: a barreira depende dele
manter o lock por toda a transação. Os locks são consultivos e não protegem
contra escritores externos, troca de inode, symlinks, múltiplos hosts, sistemas
de arquivos remotos ou perda de energia. Um escritor que obtém o lock antes de
ser admitido pode bloquear temporariamente a reabertura. Não há integração com
captura real, autenticação, deadlines nem política de recuperação de processos.

Em outro teste, um subprocesso escritor cooperativo fecha seu socket após ser
admitido, mas mantém o lock compartilhado e uma transação SQLite pendente. A
autoridade fecha a admissão e responde `BARRIER_BUSY` tanto na drenagem inicial
como na tentativa seguinte; outra escrita é recusada e a linha ainda não está
confirmada. Após o commit, encerramento do banco e liberação do lock pelo
subprocesso, a linha aparece e uma nova tentativa retorna `DRAINED`. A conclusão
e a reabertura continuam explícitas. Isso cobre somente esse protocolo sintético
e não comprova cooperação de todos os escritores do aplicativo.

Outro teste substitui, no diretório temporário da fixture, o arquivo de lock
de atividade por outro inode após um escritor cooperativo perder o socket e
enquanto mantém sua transação e seu lock originais. Antes da substituição a
autoridade responde `BARRIER_BUSY`; depois, `RETRY_DRAIN` retorna `DRAINED`,
e o escritor ainda consegue confirmar. O teste verifica que os inodes são
diferentes. Essa falha pressupõe que algum processo possa substituir a entrada
no diretório do lock; não demonstra que tal acesso exista na implantação. A
fixture não protege a identidade do arquivo e o resultado não estabelece uma
solução para substituição, corrida de caminhos ou filesystem hostil.

Uma segunda fixture de subprocesso mantém aberto o descritor original antes de
um escritor cooperativo adquirir seu lock. O teste substitui o caminho entre a
abertura e a tentativa de lock: o descritor retido ainda retorna `busy` enquanto
o escritor vive. Após matar esse subprocesso e iniciar outro que reabre o mesmo
caminho, a tentativa adquire o lock substituto apesar da transação antiga ainda
estar aberta; a tentativa libera seu lock e o escritor antigo confirma. Assim,
reter o descritor elimina apenas essa troca de caminho para aquele processo e
arquivo já aberto. Não protege escritores novos que abram o caminho substituído,
nem estabelece identidade confiável após reinício, proteção de diretório,
persistência, ou segurança contra filesystem hostil. O subprocesso é somente
uma experiência da primitiva, não uma implementação do protocolo de autoridade.

`tests/barrier_identity_prototype.rs` pesquisa outra barreira sintética: um
registro prévio de dispositivo/inode é comparado ao descritor e ao caminho,
antes e depois de adquirir o lock. Em subprocessos, a troca antes do reinício
foi recusada; após restaurar o inode original preservado por hard link, um
escritor com lock compartilhado bloqueou a aquisição até sua liberação. Uma
troca forçada entre verificação e aquisição também foi recusada na segunda
verificação. A recuperação usa apenas o arquivo original da fixture, não
estabelece procedimento operacional.

O teste também demonstra uma corrida **após** a última verificação: a autoridade
mantém o lock original, mas outro processo pode abrir e bloquear o inode recém-
substituído. Portanto essas comparações não fecham o TOCTOU e não autorizam
reabrir admissão. O registro é confiável apenas por hipótese; não foram
qualificados proteção/autenticação do registro e diretório, criação e sync do
diretório, reutilização de inode, symlinks concorrentes, crash de inicialização,
nem comportamento em outros filesystems. Não integrar esta fixture ao produto.

`tests/anchor_gate_fixture.py` experimenta outra alternativa, apenas em Linux:
um processo de teste mantém um descritor-âncora aberto e cria descrições de
arquivo independentes via `/proc/self/fd`, entregues a subprocessos escritores
e autoridades. Após substituir o caminho, um segundo escritor e autoridades
reiniciadas ainda usam o inode ancorado; o lock exclusivo permanece ocupado até
os dois escritores liberarem seus locks, quando a aquisição passa. O teste força
a troca antes de iniciar o segundo escritor e as autoridades; não simula todas
as interleavings nem uma operação real de captura.

Essa é uma propriedade da fixture enquanto a âncora está viva e todos os
participantes usam somente seus descritores. Não há supervisor operacional,
autenticação, transmissão segura entre processos independentes, isolamento do
diretório, ou coordenação dos escritores reais. A morte da âncora e de todos os
detentores perde essa referência; reiniciar por caminho substituído é inseguro,
e não há recuperação confiável demonstrada para esse caso. Não foram testadas
perda de energia, filesystem remoto ou disponibilidade de `/proc`. Não usar
esse experimento como gate de produção.

Um segundo cenário da fixture inicia um guardião da âncora e um escritor em
subprocessos, registra `uncertain` com `fsync` antes de distribuir descritores,
substitui o caminho e mata o guardião enquanto o escritor anterior mantém seu
lock. Uma autoridade sintética reiniciada recusa abrir mesmo quando consegue
obter lock exclusivo no arquivo substituto. Ela também recusa após o escritor
terminar, pois o marcador permanece incerto; não há transição automática para
limpo nem procedimento de reconciliação implementado. O teste inclui controle
inicial `clean`, mas esse marcador sozinho não comprova segurança de abertura.

O teste pressupõe gravação bem-sucedida e integridade do marcador antes de
qualquer distribuição: não cobre falha antes/durante `fsync`, persistência do
diretório, rollback do armazenamento, adulteração do marcador, concorrência de
autoridades ou perda de energia. O escritor mantém somente um lock sintético,
não uma transação SQLite; a fixture testa recusa, não recuperação operacional.

A mesma fixture agora injeta interrupções por retorno do publicador antes de
escrever o marcador e depois da escrita, antes de `fsync`: em ambos os casos o
socket fecha sem entregar descritor. Num terceiro caso, o primeiro `fsync`
termina, um descritor é enviado via `SCM_RIGHTS` a um escritor que mantém lock,
e uma falha simulada antes do segundo `fsync` interrompe a distribuição sem
entregar outro descritor. Um subprocesso de reinício permanece fechado nos três
casos, inclusive quando o marcador ainda diz `clean`; o primeiro escritor do
terceiro cenário continua vivo até ser liberado explicitamente.

O processo de reinício dessa pesquisa **sempre começa fechado**, sem tentar
reconciliar ou confiar no marcador. A falha é um retorno injetado, não erro real
de `write`/`fsync`, SIGKILL, queda de energia ou rollback de disco; os bytes
observados em cache não provam durabilidade. Não há prova de exclusão entre
autoridades, publicação segura em diretório, confirmação de transferência,
autenticação nem caminho de reabertura. Nenhum escritor real utiliza o protocolo.

Outro cenário pesquisa reconciliação sob uma hipótese mais forte: o processo de
teste sobrevive, mantém seu próprio descritor do inode original, controla toda
a admissão e conserva os handles de **todos** os escritores sintéticos. Após a
morte do guardião e a substituição do caminho, a função de reconciliação retorna
falso enquanto qualquer escritor conhecido vive ou um lock compartilhado no
inode original está ocupado. Depois da saída dos escritores, uma falha injetada
antes do `fsync` retorna falso mesmo que o marcador `clean` esteja visível; um
subprocesso reiniciado ainda começa fechado. Só uma nova tentativa, com lock
exclusivo e `fsync` bem-sucedido, retorna verdadeiro. É uma decisão booleana de
teste, não uma reabertura de servidor ou captura real.

A evidência não cobre perda ou reinício do supervisor que retém os handles,
escritores desconhecidos sem lock, novos participantes após a checagem,
descendentes, corrupção do marcador, power loss ou publicação real. A função
solta o lock ao retornar e não fornece uma barreira operacional contínua; não se
deve usar seu resultado para liberar escritores reais. A falha de persistência
é injetada, não um erro do filesystem.

Um experimento adicional usa uma autoridade sintética de processo único e um
solicitante em subprocesso, com sockets separados para controle e admissão. Um
lock compartilhado antigo impede a drenagem; depois que ele é liberado, a
autoridade mantém o lock exclusivo enquanto persiste `clean` e nega solicitações.
Outra tentativa de lock compartilhado também falha nessa janela. Após matar a
autoridade antes da abertura, outra instância começa fechada, repete drenagem e
persistência e só admite o solicitante depois do comando explícito de abertura.
O processamento de desbloqueio e abertura é serializado no mesmo loop de eventos.

O solicitante apenas troca mensagens: não inicia transação SQLite nem mantém
permissão de escrita após admissão. O teste não combina essa transição com a
reconciliação de handles anterior, não cobre solicitações simultâneas em larga
escala, morte após abertura, processos independentes não cooperativos, múltiplas
autoridades, autenticação, perda de supervisor/âncora ou falhas de `fsync` reais.
A âncora permanece no processo de teste. Portanto a transição é uma fixture, não
uma garantia de continuidade ou gate operacional.

O cenário de transição foi ampliado com um subprocesso escritor conhecido que
mantém lock compartilhado e uma transação SQLite real aberta. A primeira
autoridade morre durante a transação; após reinício, a solicitação de admissão é
negada e a drenagem continua ocupada. O supervisor de teste aguarda o commit e
o término do processo e confirma a linha no banco antes de pedir a drenagem. Em
seguida, outro solicitante é recusado durante a preparação e a autoridade morre
novamente. A terceira instância começa fechada, repete a transição e responde
admissão; só então o supervisor inicia outro escritor SQLite e confirma duas
linhas no banco.

A ligação entre resposta de admissão e início do escritor é feita pelo próprio
teste, não por uma credencial ou protocolo de produção. O supervisor retém a
âncora e controla todos os processos por hipótese; a autoridade não possui uma
lista confiável de escritores por conta própria. Não foram testados morte do
supervisor, escritores que ignorem o lock, corrupção/persistência real ou
captura incremental. Esta combinação permanece insuficiente para integração.

Outro cenário liga a solicitação de admissão ao próprio subprocesso escritor:
ele obtém lock compartilhado no inode ancorado **antes** de enviar `W`, recebe
admissão e espera antes do `BEGIN`. A autoridade é morta nessa janela. A nova
autoridade começa fechada, nega outro pedido e recebe `B` ao tentar drenar. O
escritor antigo então inicia e confirma uma transação SQLite, fecha o banco e
libera o lock; somente depois a nova autoridade drena e reabre explicitamente.
O teste verifica zero linhas antes e uma após o commit.

O resultado mostra que perder a autoridade **não revoga** a admissão já dada:
a segurança observada depende de o escritor manter o lock durante toda a
transação. A autoridade da fixture não verifica que o solicitante realmente
possui esse lock; um cliente não cooperativo poderia ignorar o protocolo. O
supervisor de teste ainda retém a âncora, controla os processos e não simula
captura ou perda de energia. Não é garantia para os escritores da aplicação.

Um experimento complementar transfere via `SCM_RIGHTS` um descritor já
bloqueado em modo compartilhado pela autoridade sintética. O subprocesso
escritor recebe o descritor e confirma recebimento; a autoridade fecha sua
referência. Depois da desconexão do socket e morte da autoridade, uma tentativa
de lock exclusivo em uma descrição independente do inode ancorado retorna
ocupado. O escritor confirma uma transação SQLite e fecha o descritor; então o
lock exclusivo é adquirido e o banco contém a linha esperada.

A transferência mostra que o lock do kernel sobrevive enquanto o receptor mantém
a descrição aberta; o ACK não prova que ele continuará fazendo isso. O escritor
pode fechar ou desbloquear o descritor antes do commit, compartilhar a descrição
ou ignorar a disciplina. Não há autenticação do receptor, associação segura ao
SQLite, teste de falha da transferência/ACK nessa fixture, nem protocolo completo
de reinício, captura ou recuperação. O supervisor mantém a âncora viva. Portanto
isso não qualifica a admissão de escritores reais nem substitui cooperação.

Um teste posterior compara três variantes com transação SQLite aberta e
autoridade morta após o ACK. Se o escritor fecha prematuramente o descritor
transferido sem outra referência ao lock, um probe independente obtém lock
exclusivo e o escritor ainda confirma enquanto esse lock está mantido. Se o
supervisor mantém uma referência duplicada à mesma descrição até o processo
escritor terminar, o fechamento prematuro não libera o lock e o probe é recusado;
após o commit, saída do escritor e fechamento da referência do supervisor, o
probe consegue adquiri-lo. Porém, se o escritor chama `flock(LOCK_UN)` antes de
fechar, o probe obtém lock exclusivo mesmo com a referência duplicada retida, e
o commit ocorre durante esse lock: os descritores compartilham a descrição de
arquivo e o desbloqueio afeta o mesmo lock.

Reter uma duplicata protege apenas contra fechamento prematuro nos casos
testados, não contra desbloqueio explícito, morte do supervisor, processos
externos ou descendentes que continuem escrevendo. A fixture não impõe posse
contínua nem prova que o processo acompanhado seja o único capaz de escrever;
continua imprópria para integração operacional.

Outra fixture abre duas descrições independentes do inode ancorado: uma é
bloqueada em modo compartilhado por um subprocesso supervisor; a outra é
transferida ao escritor SQLite. O escritor inicia uma transação e chama
`LOCK_UN` em seu descritor. Enquanto o supervisor vive, um probe exclusivo em
terceira descrição continua bloqueado. Quando o supervisor é morto, o probe
adquire o lock exclusivo e o escritor confirma a transação ainda com o probe
mantido. O teste confirma a linha no banco.

Assim, uma descrição independente resiste ao `LOCK_UN` do escritor, mas sua
liberação automática na morte do supervisor ainda pode criar falsa drenagem.
Não há aqui monitoramento confiável do processo escritor, fencing, recuperação
após morte do supervisor ou integração à admissão real; o probe representa
somente um concorrente sintético, não uma captura executada pela aplicação.

Um cenário Linux adicional entrega à autoridade reiniciada um `pidfd` aberto
para o escritor anterior antes da morte do supervisor. O escritor desbloqueia
seu próprio descritor com uma transação SQLite aberta e o supervisor independente
é morto. O teste comprova que um probe exclusivo já consegue adquirir a
barreira, mas a autoridade mantém admissão fechada e recusa drenagem enquanto
o `pidfd` não sinaliza saída. Depois do commit e término do escritor, a linha é
verificada e a autoridade executa drenagem, persistência e abertura explícitas.

O `pidfd` identifica apenas **esse processo**, não garante ausência de filhos,
outros escritores ou novos participantes. O processo de teste conserva a âncora
e entrega o handle correto à nova autoridade; não foi demonstrada descoberta,
custódia ou transferência segura de todos os handles após uma perda real do
supervisor. Também não há captura real, validação de continuidade ou garantia
de durabilidade após crash. O teste é condicional à disponibilidade de
`pidfd_open` e não autoriza integração operacional.

O exemplo de spool também exercita dois subprocessos escritores conhecidos após
morte da autoridade. Um mantém transação SQLite aberta e o outro já mantém lock
compartilhado antes de escrever. Após reinício fechado, a captura retorna `BUSY`
até os dois processos terminarem; a evidência anterior é então recusada como
`MISMATCH`, sem avançar o watermark TXID 1. O teste recria uma testemunha a
partir da própria fonte sintética, captura os dois commits, verifica watermark
TXID 3 e restaura as três linhas em outro banco, mantendo a admissão fechada.

Essa testemunha recriada não é independente nem comprova que todos os escritores
foram identificados: o próprio teste controla os dois subprocessos, e a admissão
é consultada separadamente de sua criação. O gate interno da biblioteca é sem
bloqueio no exercício e a barreira é consultiva. A dependência continua sendo o
checkout irmão modificável, não uma revisão publicada e reproduzível. Não há
prova de continuidade operacional, restauração de host ou permissão para reabrir.

Outro cenário de spool inicia um subprocesso escritor desconhecido que mantém
uma transação SQLite sem adquirir o lock consultivo nem se registrar. Depois da
morte e reinício da autoridade, os comandos **somente experimentais**
`CAPTURE`, `CAPTURE_FAULT` e `CAPTURE_INVENTORY` retornam `INVENTORY_UNKNOWN`
para inventário ausente, incompleto ou mesmo um marcador que afirma estar completo. `WRITE` e `OPEN`
continuam negados. Antes e depois das tentativas, o teste compara nomes e bytes
de todos os arquivos do repositório e o watermark; após o commit do escritor,
repete a recusa e verifica também que o WAL não mudou durante a tentativa.

A recusa é incondicional na variante conservadora porque a fixture não dispõe de
prova confiável de completude; não é descoberta automática de escritores. O teste
de regressão primeiro observou `VERIFIED` indevidamente em `CAPTURE` e depois
confirmou a recusa uniforme nos três comandos. Para preservar os experimentos
antigos, a fixture oferece uma variável explícita que **simula**, sem comprovar,
escritores conhecidos e permite os caminhos antigos; ela não é código de
aplicação nem autorização operacional. O teste conservador não executa captura
ou reconciliação e não qualifica um gate real ou a biblioteca irmã.

O caminho sintético `RECONCILE` também recusa com `INVENTORY_UNKNOWN` antes de
criar staging. O teste mantém o escritor desconhecido em transação, confirma
`SQLITE_BUSY` em `BEGIN IMMEDIATE` de outra conexão e compara bytes do WAL,
arquivos do repositório e watermark após a recusa. Após o escritor confirmar,
a ausência de prova continua causando recusa; `WRITE`/`OPEN` permanecem negados.
O comando não identifica escritores nem realiza reconciliação: é apenas uma
barreira conservadora de pesquisa.

Um cenário adicional interrompe a publicação após gravar o LTX de TXID 2,
com watermark ainda em TXID 1, mata a autoridade e inicia um escritor
desconhecido em transação. Após reinício conservador, `RECONCILE` e os três
caminhos de captura recusam a operação; os nomes e bytes de todos os arquivos
do repositório, o watermark e o WAL permanecem iguais durante as tentativas.
Após o commit do escritor, a reconciliação continua recusada e o repositório e
watermark permanecem iguais. A publicação parcial usa o modo de escritores
conhecidos simulado; a recusa posterior é incondicional e não recupera o spool
nem comprova exclusividade operacional.

Uma ampliação acompanha múltiplos processos sintéticos por `pidfd`, incluindo
um pai que inicia um descendente e termina enquanto o descendente permanece
ativo. Após a morte do supervisor e o término dos processos inicialmente
conhecidos, o descendente mantém uma transação SQLite aberta sem lock cooperativo.
Uma autoridade marcada como tendo lista incompleta recusa admissão e drenagem.
Uma segunda autoridade recebe explicitamente também o `pidfd` do descendente e,
mesmo marcada como completa pelo teste, recusa drenar até o descendente confirmar
e terminar; o banco contém então as duas linhas esperadas e a transição explícita
pode ocorrer.

A marca de completude e todos os handles são fornecidos pelo processo de teste;
não há mecanismo que descubra descendentes, prove que a lista está completa ou
recupere handles após perda de seu custodiante. Marcar erroneamente uma lista
como completa continua inseguro. O caso cobre apenas uma árvore sintética e
depende de `pidfd_open` e `pidfd_send_signal`; não constitui fencing, controle
de processos externos ou reconciliação operacional.

Uma investigação adicional iniciou um pai em sessão própria e um descendente
em outra sessão e grupo de processos. O pai terminou, mas o descendente ficou
vivo e iniciou uma transação SQLite; seu grupo era distinto do grupo do pai.
Uma autoridade sintética sem prova de completude recusou admissão e drenagem,
inclusive depois de o descendente confirmar e terminar. A limpeza usou `pidfd`
para identificar o descendente, sem sinalizar grupos do host. O teste mostra
que acompanhar apenas o líder ou seu grupo não comprova ausência de descendentes;
ele não prova que um grupo esteja vazio nem implementa contenção.

No host de teste existe cgroup v2. A inspeção somente leitura encontrou o grupo
da sessão sem permissão de escrita e uma hierarquia de usuário compartilhada
gravável; sua delegação, fronteiras de acesso e contenção não foram qualificadas
ou modificadas. Não foi executado teste de cgroup isolado e não se conclui que
esse recurso esteja indisponível. A fixture continua recebendo a condição de
inventário incompleto do teste, sem um mecanismo que a determine por si só.

#### Exercício opt-in de fronteira cgroup v2

`tests/cgroup_boundary_fixture.py` é uma pesquisa Linux independente da aplicação.
Ela só aceita um escopo transitório cujo nome comece por
`discord-mcp-cgroup-drill-`, cria um subgrupo próprio e remove somente esse
subgrupo ao terminar. Requer systemd de usuário acessível, delegação cgroup v2,
Python 3 com `pidfd_open`/`pidfd_send_signal` e permissão para criar o subgrupo.
Não faz parte de `cargo test`; não execute em uma hierarquia compartilhada ou
em um escopo que não seja criado exclusivamente para este exercício:

```sh
systemd-run --user --scope --quiet --property=Delegate=yes --collect --unit="discord-mcp-cgroup-drill-$$" /usr/bin/python3 "$PWD/tests/cgroup_boundary_fixture.py"
```

Em 2026-11-05, o escopo transitório delegado permitiu observar o descendente no
subgrupo após a saída do lançador, inclusive após `setsid`. Contudo, o próprio
descendente conseguiu mover-se do subgrupo para o escopo pai e o subgrupo passou
a indicar `populated 0` enquanto ele ainda vivia. A fixture manteve a decisão
sintética fechada e confirmou que seu subgrupo estava vazio ao terminar; a
consulta posterior não listou unidades transitórias do exercício. Portanto,
nessa configuração de permissões, subgrupo vazio não comprova fim dos escritores.

O teste move somente seu próprio subprocesso entre subgrupo e escopo transitório;
não modifica grupos compartilhados. Ele não testa separação de identidades,
cgroup namespaces, uma configuração endurecida, acesso a SQLite ou efeitos
externos, nem prova resistência a todos os escapes. O sistema de produção não
possui essa fronteira integrada ou qualificada; não reabrir com base neste ensaio.

O mesmo exercício oferece uma variante experimental `--identity`, executada com
o mesmo comando acima e esse argumento adicional. Após entrar no subgrupo, um
descendente passa por um user namespace com UID/GID subordinados e sem
capacidades efetivas; outro descendente cria uma sessão própria. O controlador
verifica que o UID efetivo no host difere do seu e que `CapEff` é zero; o
descendente tenta recuperar UID 0 no namespace e migrar-se para o escopo pai.
Em duas execuções locais, a recuperação de UID 0 não ocorreu, a migração foi
negada, `populated` continuou ativo até a saída e o subgrupo próprio ficou vazio.
O controlador permanece fechado por padrão; o escopo é removido ao final.

Esse teste usa mapeamentos subordinados e ferramentas já disponíveis, sem
alterar permissões do host. Ele cobre apenas uma identidade e uma tentativa de
migração para o pai imediato. Não prova ausência de outros acessos ou escapes,
integridade da hierarquia, proteção dos dados SQLite, criação de todos os
escritores dentro do domínio, morte do controlador, recuperação após reinício,
nem comportamento sob outras configurações. Não é sandbox qualificada nem
autorização para integrar a fronteira ao gate de produção.

Outra variante opt-in usa `--controller-crash` no mesmo comando. O processo de
teste mantém o escopo transitório e inicia um controlador separado, que lança
o descendente de identidade restrita no subgrupo. Depois que o descendente está
pronto, o teste mata o controlador. Um processo novo observa `populated=1` e
mantém `decision=closed`; o descendente ainda não consegue migrar para o escopo
pai. Após sua saída, outro processo observa `populated=0`, mas permanece fechado.
O experimento remove apenas o subgrupo próprio ao final.

O processo de recuperação é deliberadamente um observador que sempre recusa:
ele não recupera inventário, não verifica continuidade da replicação e não
autoriza reabertura. O processo de teste e o escopo sobrevivem à morte do
controlador; não foram simuladas perda do escopo/host, falha de persistência,
transação SQLite ou integração com escritores reais. A evidência não qualifica
essa fronteira para produção.

A variante opt-in `--inventory` inicia dois descendentes de identidade separada
no mesmo subgrupo e mata seus dois controladores. Em uma observação controlada,
os PIDs em `cgroup.procs` coincidem com os dois processos sintéticos conhecidos;
`populated` permanece 1 após a saída do primeiro e passa a 0 após a saída do
segundo. Ambas as tentativas de migrar ao escopo pai são negadas. Um observador
novo permanece fechado com dois, um ou zero processos e quando a evidência está
ausente. Como contraprova, o teste inicia uma transação SQLite sintética fora do
subgrupo: o subgrupo permanece vazio enquanto esse escritor está ativo, e a
decisão continua fechada.

O kernel informa somente o conteúdo do subgrupo observado; as leituras de
`cgroup.events` e `cgroup.procs` não formam snapshot atômico. O teste controla a
criação e os handles dos dois descendentes, mas não prova que todos os escritores
da aplicação nasceram no subgrupo nem garante que ele não seja alterado. A
observação não verifica inventário, continuidade ou recuperação de host e não é
autorização para reabrir. A execução cria e remove apenas escopo/subgrupo e banco
sintético próprios, seguindo o comando anterior com `--inventory`.

As variantes opt-in `--launch-admission` e `--launch-deny` experimentam um
lançamento cooperativo em duas fases. O descendente de identidade restrita espera
antes de criar o banco; o controlador de teste verifica seu UID, capacidades,
presença no cgroup e caminho em `/proc/<pid>/cgroup`. Na primeira variante, o
processo recebe permissão de teste, inicia a transação e o controlador é morto;
o exercício conserva a recusa de recuperação e verifica os snapshots como acima.
Na segunda, uma leitura de evidência deliberadamente inexistente causa recusa;
o escritor termina sem criar o banco e captura/admissão permanecem fechadas.

Essa é uma barreira **cooperativa**: o próprio código sintético espera a mensagem
antes de acessar SQLite. Ela não impede código alternativo, outro processo com
acesso ao banco ou uma mudança entre verificação e uso; tampouco prova que todos
os escritores são lançados por ela. O teste não estabelece controle obrigatório,
isolamento completo ou recuperação operacional. Não aplicar ao aplicativo como
se fosse prova de completude.

O exemplo de spool inclui um teste opt-in que combina processos com o mesmo UID
subordinado e a recusa conservadora de captura. Um processo representando o
corretor mantém uma conexão SQLite; outro processo com o mesmo UID inicia e
confirma uma escrita direta, sem consultar o gate. A autoridade é morta e
reiniciada enquanto a transação está aberta. Antes e depois do commit, os três
caminhos de captura conservadores retornam `INVENTORY_UNKNOWN`; nomes e bytes
do repositório e watermark permanecem idênticos. O teste verifica que a linha
confirmada existe e que o WAL mudou, mas que a tentativa posterior de captura
não o alterou.

Após os dois subprocessos conhecidos terminarem, o teste copia o banco, seu WAL
e o repositório sintéticos para um diretório privado separado. Nesse cenário
controlado, sem escritores adicionais iniciados pelo teste, uma captura sobre
as cópias avança de TXID 1 para 2; a restauração isolada tem as mesmas linhas da
fonte e passa `integrity_check`. O teste compara os bytes do banco principal,
WAL e repositório originais antes e depois e mantém os três comandos de captura
e abertura da autoridade conservadora negados.

O banco e seus sidecars são sintéticos, com permissões temporariamente
compartilhadas apenas dentro do diretório próprio do teste; o processo que
representa o corretor não implementa um corretor operacional. Copiar banco e WAL
assim é válido somente para esta fixture controlada, não procedimento de backup.
O gate usado na cópia não bloqueia escritores; a saída de dois processos conhecidos
não prova ausência de escritores externos ou continuidade real. Não há
reconciliação nem reabertura do repositório original. A biblioteca usa o checkout
irmão local modificável. Para executá-lo, além dos requisitos do exercício de
spool, são necessários Linux x86-64, `unshare` e mapeamentos de UID subordinado:

```sh
TMPDIR=/tmp/opencode cargo test --locked --manifest-path drills/s3/Cargo.toml --target-dir target/s3-drill --example spool_recovery tests::subordinate_uid_direct_writer_cannot_trigger_conservative_capture -- --ignored --exact
```

A variante opt-in `--uncontrolled-writer` cria um banco SQLite exclusivamente
sintético em diretório temporário próprio e concede acesso de escrita ao arquivo
de teste para a identidade subordinada. Um processo restrito, sem capacidades
efetivas, é iniciado no escopo transitório **fora** do subgrupo controlado e sem
o handshake de lançamento. Ele abre uma transação e confirma uma linha; antes do
commit a linha não é visível a outra conexão. Enquanto ele está ativo, o
subgrupo indica vazio e o observador de inventário mantém a decisão fechada.
O teste confirma o commit, encerra o processo e remove os recursos próprios.

As permissões deliberadamente compartilhadas são apenas do arquivo sintético,
não representam a configuração do banco real. O resultado mostra que, quando
uma identidade possui acesso ao banco, executar código com essa identidade fora
do launcher pode contorná-lo. O observador não identifica automaticamente esse
escritor; permanece fechado por política fixa. Não foram alteradas permissões do
host ou da aplicação, e o experimento não estabelece contenção operacional.

#### Exercício opt-in de acesso mediado ao SQLite

`tests/access_boundary_fixture.py` pesquisa um corretor SQLite em subprocesso
separado. Dentro de um escopo transitório próprio, ele cria um banco sintético
em diretório temporário privado (`0700`) e um cliente de UID subordinado sem
capacidades efetivas. O cliente falha ao abrir o arquivo diretamente tanto via
`open` quanto via SQLite, mas pede uma escrita ao corretor por socket local. O
primeiro pedido confirma uma linha; no segundo, o corretor é morto com transação
aberta. O cliente observa o fechamento do socket e continua sem acesso direto;
uma conexão independente verifica que a segunda linha não foi persistida e que
`integrity_check` retorna `ok`. A decisão sintética de recuperação permanece
fechada. Execute somente no escopo isolado:

```sh
systemd-run --user --scope --quiet --property=Delegate=yes --collect --unit="discord-mcp-cgroup-drill-$$" /usr/bin/python3 "$PWD/tests/access_boundary_fixture.py"
```

O diretório e o banco são exclusivos do exercício e removidos após os processos
terminarem; nenhum dado ou permissão real é alterado. O teste não autentica o
socket nem cobre pedidos duplicados, processos com o mesmo UID do corretor,
descritores vazados, autorização de canais, captura incremental, perda de host
ou recuperação operacional. O corretor é apenas uma fixture: não foi integrado
ao aplicativo nem demonstra que todos os escritores reais passem por ele.

O mesmo exercício agora inicia outra autoridade sintética que recusa pedidos de
escrita e captura enquanto não há prova de fencing. Um subprocesso distinto com
o mesmo UID do corretor acessa diretamente o banco privado e mantém uma transação;
a autoridade recusa ambos os pedidos. O subprocesso confirma a linha sem usar o
corretor, e a autoridade continua recusando. A contagem é verificada por uma
conexão independente antes e depois do commit.

Esse controle mostra que um gate fechado **não impede** acesso direto por outro
processo com o mesmo UID e permissões ao arquivo. A autoridade da fixture apenas
nega mensagens: não executa captura, não identifica automaticamente processos,
não aplica isolamento obrigatório e não protege o aplicativo real. A recusa é
conservadora, não uma solução para o acesso concorrente.

#### Exercício opt-in de corretor com UID subordinado

`tests/subuid_broker_fixture.py` inicia um corretor sintético em user namespace,
com UID subordinado e diretório SQLite próprio `0700`. No host de teste, o UID
original não conseguiu abrir o banco diretamente; após matar o corretor com
transação aberta, um novo corretor da mesma identidade verificou rollback e
integridade. Entretanto, outro processo iniciado pelo mesmo usuário conseguiu
mapear o mesmo UID subordinado, abrir o banco diretamente e confirmar uma linha.
O gate sintético permaneceu fechado antes e depois da escrita.

O ensaio usa somente banco e diretórios temporários próprios; um processo da
identidade subordinada remove apenas os arquivos conhecidos da fixture após os
subprocessos terminarem. O resultado depende dos mapeamentos e permissões locais:
separar o corretor do UID original não prova exclusividade quando outros
processos conseguem assumir a identidade subordinada. A fixture não autentica
o corretor, não cobre todos os caminhos de acesso, captura incremental ou
recuperação de host e não é integração operacional. Requer user namespaces,
mapeamentos subordinados e escopo transitório próprio:

```sh
systemd-run --user --scope --quiet --property=Delegate=yes --collect --unit="discord-mcp-cgroup-drill-$$" /usr/bin/python3 "$PWD/tests/subuid_broker_fixture.py"
```

#### Exercício opt-in de restrição por processo

`tests/landlock_boundary_fixture.py` usa Landlock Linux ABI 3 ou superior em
x86-64, com `no_new_privs` aplicado **apenas ao cliente sintético**. A política
de teste nega operações tratadas de escrita, criação, remoção e truncamento;
não há regras de permissão. O cliente tem o mesmo UID do corretor e consegue
ler o banco sintético, mas suas tentativas de abri-lo para escrita e removê-lo
são negadas. Ele solicita uma transação ao corretor via socket; o teste mata o
corretor antes do commit, confirma rollback e verifica que o cliente continua
sem escrita direta. Um gate sintético recusa admissão e captura.

Como contraprova, outro processo do mesmo UID **sem Landlock** confirma uma
escrita diretamente no mesmo banco, enquanto o gate continua recusando. Assim,
restringir um processo não cerca processos não controlados nem prova que todos
os escritores herdem a política. O teste não cobre descritores abertos antes da
restrição, outras operações de filesystem, aplicação obrigatória a todos os
processos, autenticação de socket, captura incremental ou perda de host. Não é
sandbox nem gate de produção. Para executá-lo, requer kernel compatível e um
escopo transitório próprio; não instala nem altera políticas do host:

```sh
systemd-run --user --scope --quiet --property=Delegate=yes --collect --unit="discord-mcp-cgroup-drill-$$" /usr/bin/python3 "$PWD/tests/landlock_boundary_fixture.py"
```

#### Exercício opt-in de máscara em namespace de montagem

`tests/mount_boundary_fixture.py` cria banco e escopo sintéticos próprios. Após
matar o corretor com uma transação aberta e verificar rollback, inicia um processo
com o mesmo UID no host em user/mount namespaces privados. O processo monta um
tmpfs somente sobre o diretório sintético em seu namespace: o caminho normal do
banco deixa de existir ali. Porém, um descritor de escrita intencionalmente
aberto antes da máscara continua funcionando; o teste altera o `user_version`
do banco sintético e verifica o resultado externamente. Nessa configuração, o
processo também conseguiu desmontar sua própria máscara e confirmar uma linha
via SQLite. O gate sintético continuou recusando admissão e captura.

O teste pressupõe um descritor vazado e capacidades de montagem no namespace;
não prova que uma configuração endurecida permita os mesmos acessos. Ele mostra
que esconder apenas o caminho não é fencing global: descritores existentes e
capacidade de alterar o namespace precisam ser considerados. Não há autenticação,
isolamento obrigatório de todos os escritores, captura incremental ou recuperação
operacional. Não use este experimento como fronteira de produção. A montagem é
privada, não altera a hierarquia de montagem do host, e o banco é temporário:

```sh
systemd-run --user --scope --quiet --property=Delegate=yes --collect --unit="discord-mcp-cgroup-drill-$$" /usr/bin/python3 "$PWD/tests/mount_boundary_fixture.py"
```

#### Exercício opt-in de namespace endurecido e descritores

`tests/hardened_mount_fixture.py` testa uma variante local: antes de executar o
cliente, um bootstrap sintético rejeita descritores herdados fora de uma lista
permitida; uma execução com descritor do banco intencionalmente herdado é
recusada. Sem o descritor, o bootstrap mascara o diretório sintético em namespace
privado, limpa grupos suplementares, troca para UID/GID subordinados, aplica
`no_new_privs` e verifica que não recupera UID 0. A desmontagem retorna `EPERM`,
e os caminhos testados — direto e via `/proc/<pid>/root` do processo de teste —
não permitem abrir o banco. O controlador confirma UID efetivo distinto e
ausência de capacidades efetivas no cliente.

Como contraprova, outro processo fora dessa restrição e com o UID original
confirma uma escrita diretamente no mesmo banco sintético; o gate de mensagens
continua recusando captura e admissão. A lista de descritores e o bootstrap são
confiáveis apenas por hipótese: não há imposição para todos os processos, prova
de ausência de outros caminhos ou de processos privilegiados, autenticação,
captura incremental ou recuperação de host. O teste não qualifica sandbox nem
gate de produção. A montagem é privada e os recursos são próprios do exercício:

```sh
systemd-run --user --scope --quiet --property=Delegate=yes --collect --unit="discord-mcp-cgroup-drill-$$" /usr/bin/python3 "$PWD/tests/hardened_mount_fixture.py"
```

A variante `--sqlite-controller-crash` acrescenta uma transação SQLite em WAL
ao descendente restrito. Antes de sinalizar prontidão, ele cria banco sintético,
inicia `BEGIN IMMEDIATE`, insere uma linha e confirma com outra conexão que ela
ainda não está visível. O controlador é morto com essa transação aberta; o
observador retorna `populated=1 decision=closed`, a migração para o escopo pai
é negada, e o descendente confirma o commit. Outra conexão verifica uma linha;
o observador permanece fechado com o processo vivo e também após sua saída.
O banco fica em diretório temporário próprio do teste, removido ao final.

O observador ainda recusa incondicionalmente: não verifica recuperação de WAL,
continuidade incremental, integridade do inventário ou segurança para reabrir.
O escopo e o processo de teste sobrevivem; não há perda de host, falha de disco,
escritores externos ou acesso a dados reais. A configuração e as limitações de
contenção anteriores continuam aplicáveis; não integrar essa fixture ao gate.

A variante SQLite agora cria um snapshot sintético com a API de backup do
SQLite antes da transação e outro após o commit. Depois da saída do escritor,
um subprocesso de reconciliação verifica integridade e compara as linhas da
tabela de teste: o snapshot anterior retorna `stale`, o posterior `matched` e
um snapshot ausente `unavailable`; todos mantêm `decision=closed`. Para essa
fixture exclusivamente, a leitura usa `immutable=1` somente depois de observar
a saída do escritor e confirmar ausência dos sidecars `-wal` e `-journal`.

A comparação cobre uma única tabela e dados sintéticos locais; não valida
identidade ou completude da fonte, LTX, WAL histórico, publicação remota,
revogações, todas as tabelas da aplicação ou continuidade de replicação. Mesmo
`matched` não autoriza reabrir. O modo imutável não deve ser generalizado para
bancos vivos ou com sidecars. Não houve restauração operacional ou prova de
recuperação de host.

Uma fixture de inventário investiga a janela entre lançar e registrar um
escritor. O lançador grava `launching` e chama `fsync` antes de iniciar um
subprocesso SQLite; o teste o mata depois de iniciar o escritor e antes de
registrá-lo. O escritor continua com transação aberta, enquanto uma verificação
sintética recusa abertura por causa da intenção pendente. Após o commit e a
saída do escritor, o marcador continua pendente e a verificação segue fechada.
Uma interrupção injetada antes do `fsync` também não lança escritor na fixture.

Isso demonstra recusa para uma janela específica, **não prova completude**:
o teste pressupõe um lançador único que controla todos os escritores, marcador
pré-existente e íntegro e sucesso de `fsync` antes do lançamento. Não há
protocolo de registro/conclusão ou reconstrução após reinício, proteção contra
escritores externos/descendentes, fsync do diretório, falha de armazenamento,
rollback ou perda de energia. O guardião simplificado aceita `clean` sem prova
independente; esse controle da fixture não é autorização de abertura real.

Um protótipo separado de registro SQLite exercita estados `intent`, `running`
e `done` e uma época `unsealed`/`sealed`. Um subprocesso é morto com atualização
para `done` não confirmada; após reinício, a transação é revertida e a verificação
permanece fechada. Com o pai encerrado e um descendente em transação SQLite,
inventário não selado, registro ativo e estado inválido são recusados. Após o
descendente confirmar, o `pidfd` sinalizar saída e o supervisor de teste registrar
`done`, a verificação sintética retorna `OPEN`.

O mesmo teste força um selo prematuro antes de registrar o descendente e observa
`OPEN` **enquanto ele ainda escreve**. Portanto o registro, `integrity_check` e
um selo gravado não provam completude: são necessários controle verificável de
todos os lançamentos e descendentes e custódia confiável dos handles. O selo e as
conclusões são controlados pelo teste; não há autenticação, protocolo de produção,
reabertura real, garantia contra rollback, perda de energia ou escritores externos.
Este resultado demonstra explicitamente uma falsa abertura, não uma solução.

**Correção experimental de `RETRY_DRAIN`:** anteriormente a tentativa declarava
drenagem após `CLOSE_FAIL` sem repetir a persistência. Agora cada tentativa válida
sincroniza o marcador fechado antes de verificar atividade ou adquirir a barreira;
se a persistência falhar, retorna `PERSIST_FAILED` sem declarar drenagem. O teste
de regressão primeiro reproduziu a resposta incorreta `DRAINED` com o marcador
ausente e passou após a correção. Outro teste mantém escritor cooperativo ativo,
induz falha de persistência, verifica recusa, recupera o marcador e reinicia a
autoridade; a barreira permanece ocupada até o commit do escritor anterior.
Essas falhas são sintéticas ou de arquivo ausente, não prova de durabilidade
em caso de perda de energia. O protocolo continua inadequado para produção.

#### Propriedade e sobrevivência da captura

Dois testes adicionais reproduzem falhas deliberadamente no fixture de
autoridade. Primeiro, após uma conexão receber `DRAINED`, uma segunda conexão
envia `FINISH` e `OPEN` e consegue admitir um escritor, embora a primeira não
tenha terminado sua captura. Não há vínculo de propriedade ou autenticação do
comando de encerramento. Segundo, uma transação de leitura SQLite sintética
permanece ativa enquanto a autoridade morre e reinicia: a nova autoridade
abre a admissão e uma conexão escreve, enquanto a transação anterior ainda vê
seu snapshot antigo. A leitura e o socket anteriores permanecem no processo
do teste; isso modela uma captura sobrevivente, não executa `capture_once` nem
prova seu comportamento específico. A barreira exclusiva estava em posse da
autoridade antiga e foi liberada com sua morte, não pelo cliente da captura.

Esses testes passam por registrar o comportamento inseguro, não porque as
garantias estejam satisfeitas. Uma solução precisa vincular a conclusão ao
detentor autorizado e tratar de forma segura uma captura que sobreviva à morte
da autoridade, inclusive quando conexão e processo divergem. Sem solução e
testes adicionais, não integrar este fixture aos escritores da aplicação.

#### Alternativa com lock no processo de captura

Um caminho separado `CLOSE_OWNED` no fixture experimenta manter um lock
exclusivo adicional no próprio subprocesso de captura, adquirido antes do pedido
à autoridade e mantido até encerrar sua transação SQLite sintética. A resposta
de conclusão deve chegar pela conexão que pediu a captura; outro cliente recebe
`NOT_OWNER`. A reabertura testa o lock de captura além da barreira de escritores.
Nos casos testados, a captura viva bloqueou reabertura após morte e reinício da
autoridade; terminar ou matar o subprocesso liberou seu lock. Se o subprocesso
morre com a autoridade ativa, o fixture permanece fechado até reinício. Os
caminhos antigos e seus testes inseguros permanecem para comparação.

Isso é somente uma hipótese cooperativa, não segurança de produção: a
autoridade não verifica se o cliente detém o lock, nem autentica processos ou
vincula uma identidade ao inode. Não foi testado `capture_once`, transferência
de lock, filesystem remoto, perda de energia, captura com filhos que sobrevivem
ao detentor do lock, nem recuperação de captura parcial. A combinação com
`RETRY_DRAIN` após desconexão ou falha de drenagem ainda não estabelece uma
transferência segura de propriedade; conexão encerrada e estado fechado podem
exigir intervenção. Não integrar à aplicação antes de definir e verificar esses
casos e a coordenação de todos os escritores.

Testes adicionais demonstraram que `CLOSE_OWNED` aceita um cliente que nunca
obteve `capture.lock`: outra conexão de arquivo consegue adquirir o lock após
`DRAINED`. Portanto, mesmo observar um lock ocupado não identificaria quem o
detém, e o fixture ainda não verifica propriedade. Também foi reproduzido que
outra conexão podia obter `DRAINED` via `RETRY_DRAIN` após uma tentativa de
captura com barreira ocupada. O fixture agora responde `NOT_OWNER` a essa
tentativa e permanece fechado; o teste reinicia a autoridade somente depois de
liberar o lock sintético e então verifica a abertura. Não há retomada segura da
captura interrompida: o teste não valida recuperação de captura parcial nem
autoriza reiniciar ou reabrir um sistema real nessas condições.

#### Experimento de transferência de descritor — Linux

`tests/fd_gate_fixture.py`, executado por `cargo test --test fd_gate_prototype`
no Linux com Python 3 em `/usr/bin` ou `/bin`, pesquisa uma primitiva distinta:
o remetente adquire `flock` exclusivo e transfere o descritor já bloqueado por
`SCM_RIGHTS` em socket Unix a um subprocesso. O receptor valida que recebeu um
descritor e o mantém aberto. Os testes verificam que o lock impede outra
aquisição depois do fechamento da cópia do remetente e da desconexão do socket;
que a morte do receptor libera o lock; que uma transferência a socket já
desconectado falha e o lock permanece no remetente até ele fechar sua cópia; e
que um lock compartilhado impede a aquisição exclusiva até ser liberado.

Este é um teste da primitiva, não uma autoridade completa: não executa o gate
anterior nem `capture_once`, não mata e reinicia uma autoridade real, não liga
transferência à persistência do fechamento, nem garante recuperação após
captura parcial. Um descritor prova que o receptor recebeu uma referência ao
lock, mas pode ser duplicado, transferido ou fechado antes de terminar; não
prova identidade, autorização, exclusividade lógica da captura ou conclusão
durável. Descendentes que herdam descritores, perda de mensagem/ACK, troca de
inodes e clientes não cooperativos permanecem fora da prova. Não usar essa
primitiva como gate de produção sem protocolo e testes completos.

O fixture Linux também contém uma autoridade sintética de cliente único em
subprocesso: ela inicia fechada, sincroniza marcador fechado, tenta drenar um
lock de atividade e transfere um descritor de lock de captura por `SCM_RIGHTS`.
No teste de reinício, a autoridade é morta depois da confirmação da entrega;
outra autoridade inicia fechada e recusa abrir enquanto o cliente mantém o
descritor. No teste de ACK perdido, a primeira tentativa de drenagem encontra
um escritor sintético; após liberar seu lock, a transferência ocorre, o
cliente fecha a conexão sem ACK mas mantém o descritor, e a autoridade seguinte
recusa abertura até que ele seja fechado. Esses testes usam socketpair privado
e dados sintéticos; o cliente de captura é o próprio processo do teste.

A demonstração não resolve a segurança operacional: a autoridade é uma máquina
de estados mínima, com um único cliente, e não coordena conexões ou escritores
da aplicação. O lock transferido
pode ser fechado ou repassado indevidamente; não há autenticação, garantia de
captura concluída, persistência de diretório, recuperação de publicação parcial
ou acesso real à biblioteca de captura. A posse do descritor é uma capacidade,
não prova de identidade nem de que o trabalho terminou. Esses resultados não
qualificam reinício de host, perda de energia ou implantação.

Ampliação posterior: a abertura também exige o lock exclusivo de atividade,
para recusar escritores sintéticos que sobreviveram ao reinício; o teste de
regressão primeiro observou a abertura incorreta com um lock compartilhado ativo.
Antes da transferência, o fixture sincroniza um marcador `pending`; após
reinício, a abertura e outra captura recusam esse estado. Um comando explícito
`X`, com ambas as barreiras livres, só limpa o marcador quando um arquivo de
teste contém exatamente `synthetic-complete`. O teste rejeita um arquivo
truncado e uma barreira ocupada antes de aceitar o conteúdo sintético.

Essa verificação é **apenas um modelo**, não valida LTX, TXID, checksums, spool,
publicação remota ou integridade SQLite. A substituição do conteúdo truncado é
realizada pelo próprio teste, não por um algoritmo de recuperação; não existe
reconciliação operacional de captura parcial. O marcador é sobrescrito em lugar,
sem qualificação de falhas de gravação, sincronização de diretório ou perda de
energia. A autoridade continua de cliente único e os locks são consultivos.

#### Reconciliação isolada de spool LTX

O teste abaixo usa diretamente as bibliotecas locais de LTX, spool e filesystem
do checkout irmão, sem Floci ou acesso a dados reais:

```sh
TMPDIR=/tmp/opencode cargo test --locked --manifest-path drills/s3/Cargo.toml --target-dir target/s3-drill --example spool_recovery
```

Ele cria em diretórios temporários uma imagem artificial de duas páginas com
cabeçalho SQLite, um snapshot LTX TXID 1 e um delta LTX TXID 2 válido para a
biblioteca. Após falha injetada no ponto `AfterLtxPublish`, o watermark continua
em 1 enquanto o delta existe. `spool::recover` verifica a cadeia, atualiza o
watermark para 2 e retorna o checksum esperado; uma segunda execução produz o
mesmo estado. Em outro diretório, truncar o delta publicado faz a recuperação
retornar erro sem avançar o watermark. Nos dois casos os bytes da imagem fonte
sintética são comparados com a cópia original. Nenhuma base da aplicação é usada.

Executado com checkout irmão em `f5c0984f069407913365136a10d5962f9a33930d`,
com modificações e arquivos preexistentes, inclusive em `core/src/lib.rs` e
`core/src/coordinator.rs`; a dependência por caminho não é fixada pelo lockfile
e pode mudar concorrentemente. Não houve edição intencional do checkout irmão.
A imagem artificial não comprova funcionamento de uma base SQLite real, e a
falha é um retorno injetado, não SIGKILL nem perda de energia. O teste usa WAL
vazio; a biblioteca exige reconciliar o estado recuperado com a base ativa quando
a geração WAL não está disponível. Não foram testados captura real, publicação
remota parcial, consistência com o banco da aplicação ou retomada operacional.

Uma ampliação usa SQLite real com dados sintéticos: cria uma tabela e uma linha,
faz checkpoint inicial, cria snapshot, insere outra linha e mantém a conexão
aberta com autocheckpoint desativado. A biblioteca traduz o WAL não vazio para
um delta; após falha injetada na publicação, uma cópia em memória com cabeçalho
WAL corrompido é recusada sem mudar o watermark. Com o WAL original, `recover`
reconcilia o TXID e a posição WAL, e uma segunda chamada repete o mesmo estado.
O restore TXID 2 para arquivo novo passa `PRAGMA integrity_check` e contém as
mesmas linhas da fonte, que também são comparadas antes e depois do exercício.

O teste não usa `capture_once`: a tradução e a publicação são chamadas
diretamente, sem escritores concorrentes e sem gate real. A corrupção foi só na
cópia em memória; a interrupção foi injetada, não uma queda de processo. Não
testa rotação/checkpoint concorrente, reconciliação de WAL ausente ou de outra
geração, nem uma base Discord MCP, recuperação de host ou armazenamento remoto.
A revisão do checkout irmão observada para esta execução foi novamente
`f5c0984f069407913365136a10d5962f9a33930d`, com mudanças locais preexistentes;
dependências por caminho não oferecem uma árvore imutável reproduzível.

Dois testes adicionais usam `capture_once` em Linux x86-64 com SQLite sintético
real e uma implementação de `WriteGate` sem bloqueio: o próprio teste é o único
escritor e não escreve durante a captura. Ambos partem de snapshot TXID 1,
gravam uma linha ainda não capturada e fazem checkpoint `TRUNCATE` bem-sucedido.
No primeiro, fecham a última conexão SQLite e verificam que o arquivo WAL está
ausente. No segundo, mantêm a conexão, gravam outra linha e verificam que os
salts do novo WAL diferem dos anteriores. Em cada caso a captura reconcilia o
estado, o restore em destino novo contém as mesmas linhas da fonte e passa
`integrity_check`; uma segunda captura não publica transações adicionais.

Esses testes não simulam perda de um WAL contendo commits não checkpointados:
o checkpoint anterior concluiu com sucesso. O spool inicial contém apenas o
snapshot, não um delta parcialmente publicado; as falhas parciais são cobertas
separadamente acima e nos cenários combinados abaixo. Não há concorrência, SIGKILL, substituição de WAL por um
terceiro, gate real ou validação de dados de aplicação. A comparação verifica
as linhas observáveis e integridade da cópia restaurada, não uma recuperação de
host ou uma prova de preservação de todas as transações sob falhas arbitrárias.

Dois cenários combinados acrescentam ao mesmo exercício uma falha injetada
`AfterLtxPublish`: o delta TXID 2 já existe enquanto o watermark permanece em
TXID 1. O teste grava uma linha adicional, conclui checkpoint e então remove o
WAL pelo fechamento normal da última conexão ou cria outra geração de WAL com
salts diferentes. Em ambos `capture_once` recupera o spool e reconcilia a
imagem SQLite, resultando em watermark TXID 3. O restore em destino novo passa
`integrity_check` e suas linhas coincidem com a fonte; uma segunda captura não
adiciona transações. A falha é um retorno injetado, não morte de processo; o
checkpoint foi bem-sucedido e o gate é o fixture de escritor único, sem
concorrência. Não há teste de perda de commits não checkpointados nem garantia
para queda de energia, armazenamento remoto ou banco de aplicação.

#### Perda simulada de WAL com commit não checkpointado

Em um diretório temporário, o teste mantém uma conexão SQLite sintética aberta
com uma segunda linha confirmada apenas no WAL. Publica no spool o delta TXID 2
e injeta falha antes de avançar o watermark. Para simular uma visão sem WAL,
copia somente o arquivo principal para outro caminho novo; não remove nem altera
o WAL da fonte. A fonte enxerga duas linhas e a cópia incompleta apenas uma.

Na revisão local testada, `capture_once` **retorna sucesso** ao usar a cópia
incompleta como origem: reconcilia o spool para o estado antigo e avança o
watermark a TXID 3. O restore TXID 3 passa `integrity_check`, mas contém somente
uma linha. O LTX TXID 2 permaneceu inalterado e seu restore ainda contém duas
linhas. O teste compara também a contagem da fonte e os bytes do WAL original,
que permanecem inalterados. Portanto, essa API não detectou a perda sem evidência
externa: integridade estrutural e sucesso da captura não comprovam que os commits
foram preservados. Não usar a cópia incompleta como origem operacional; preservar
e avaliar pontos anteriores antes de qualquer reconciliação real.

O exercício é uma simulação deliberada com arquivos próprios, não uma perda
real, nem prova de comportamento sob falha física, execução concorrente ou em
outras revisões da biblioteca. O gate continua sendo um fixture sem bloqueio.

#### Evidência independente experimental de continuidade

Um teste separado obtém, antes de simular a perda, uma imagem consistente da
fonte sintética usando SQLite Backup API e registra em memória sua tupla de
page size, tamanho em páginas e checksum calculado pela biblioteca LTX. Essa
evidência é obtida da fonte completa, independentemente da reconciliação do
spool; não é um registro operacional independente do host. O teste compara a
imagem consistente de uma cópia candidata contra a tupla antes de chamar
`capture_once`. Evidência ausente ou divergente recusa a chamada; snapshot,
delta TXID 2 e watermark são comparados com seus bytes/valores anteriores, TXID
3 não aparece e o restore TXID 2 ainda contém as duas linhas. A fonte e os
bytes do seu WAL permanecem intactos. Como controle positivo, a fonte completa
corresponde à evidência e é aceita para captura no mesmo teste, sem escritores
entre a verificação e a chamada.

Essa é uma barreira **somente do teste**, não uma funcionalidade ou garantia da
biblioteca nem da aplicação. A tupla LTX não é uma assinatura criptográfica;
o teste não comprova resistência a colisões ou manipulação. Evidência velha
pode rejeitar mudanças legítimas ou não incluir commits posteriores, e não há
garantia de frescor, identidade da base, publicação independente durável ou
custódia fora do host. Uma alteração entre a cópia de verificação e a captura é
uma corrida não protegida por este fixture. Não se deve autorizar captura real
com base nesse experimento; é necessário um protocolo de evidência e coordenação
que cubra essas fronteiras, inclusive crash, falhas de persistência e escritores.

Um teste posterior reproduz essa corrida com uma segunda conexão SQLite e
sincronização por canais: a evidência e a imagem candidata coincidem; somente
depois da comparação aceita o escritor confirma uma nova linha; a captura é
chamada em seguida com o gate sem bloqueio. Uma comparação atualizada detecta
que a evidência ficou desatualizada, sem alterar snapshot ou watermark. Porém,
um chamador que confie no resultado anterior ainda chama `capture_once`, que
aceita e captura a nova linha. O restore TXID 2 contém ambas as linhas, assim
como a fonte. O resultado demonstra uma janela TOCTOU, **não perda de dados**
neste cenário. Evidência desatualizada também rejeita essa alteração legítima;
o teste não decide se ela deve ser aceita na política operacional.

O teste é limitado a uma alteração confirmada entre comparação e captura, sem
substituição do arquivo, queda, prova de identidade ou processo real da
aplicação. Para evitar a janela é necessário vincular verificação e captura a
uma exclusão que cubra todos os escritores e a mesma identidade da origem; o
fixture não oferece essa garantia e não deve ser integrado à aplicação.

#### Barreira cooperativa durante verificação e captura

Três testes do exemplo de spool usam um subprocesso que mantém um lock
consultivo exclusivo desde antes da cópia consistente e comparação da evidência
até `capture_once` retornar. Um escritor SQLite em outra thread tenta obter o
mesmo lock em modo compartilhado: o teste observa a tentativa bloqueada,
executa a captura, e só depois o escritor confirma sua transação. A cópia
restaurada do TXID capturado contém a linha anterior; a fonte contém também a
linha confirmada depois da liberação. A sincronização usa mensagens, não uma
espera de duração presumida. O gate passado à biblioteca é sem bloqueio porque
o lock externo é mantido pelo subprocesso durante toda a chamada.

Na falha sintética de diretório de staging, a captura retorna erro e o lock é
liberado; o teste confirma que snapshot e watermark não mudaram, repara somente
o diretório temporário do teste e repete a captura com sucesso. Em outro caso,
o subprocesso é morto após verificar a evidência: seu lock é liberado, uma
escrita cooperativa consegue entrar e o processo reiniciado recusa a evidência
agora desatualizada sem avançar o spool. **Não há fail-closed durante a ausência
do processo**: a escrita após a morte é uma limitação demonstrada, não garantia
de recuperação segura. A falha de staging não simula queda após publicação.

O experimento não coordena processos reais da aplicação, não autentica usuários,
não protege contra escritor não cooperativo ou troca dos arquivos de lock e
origem, nem testa rede, perda de energia, evidência forjada ou continuidade após
falha parcial de publicação. A evidência continua uma tupla não criptográfica,
de frescor e custódia não qualificados. Esses resultados não autorizam integrar
o protótipo ao gate real ou habilitar replicação operacional.

#### Autoridade reiniciada fechada com verificação e captura

Um fixture adicional do exemplo executa uma autoridade sintética em subprocesso
com socket Unix: ela inicia fechada, mantém lock exclusivo de atividade durante
a comparação da imagem com a evidência e a chamada `capture_once`, e permanece
fechada até um comando explícito de abertura após sucesso. O teste verificou
recusa com evidência inválida, lock ocupado durante a verificação, morte da
autoridade antes da captura, falha de conexão enquanto ausente, e recusa após
reinício até nova verificação, captura e abertura. Só então um escritor de
teste adquire o lock compartilhado, pede admissão e grava uma linha sintética.
O processo anterior é encerrado antes de o teste remover apenas seu socket
temporário; não é um procedimento para sockets de produção.

Este fixture não prova fail-closed para a aplicação: depende de cada escritor
adquirir o lock e consultar a autoridade sem fallback, não autentica conexões,
não acompanha efeitos externos ou a conclusão de escritores já admitidos,
e não faz gestão segura de socket, inode, substituição de arquivo ou múltiplas
autoridades. A morte foi antes de `capture_once`; perda durante publicação e
evidência independente durável não foram testadas nesse fluxo. Não cobre
perda de energia, rede, estado parcial nem identidade da fonte. O gate real
continua não implementado e não deve ser inferido desses resultados.

O fixture também executa uma publicação LTX sintética em um subprocesso de
autoridade com a barreira mantida. Como `capture_once` público não oferece um
hook de falha, esse caminho de teste chama diretamente tradução WAL e
`publish_transactions`, sinaliza no hook `AfterLtxPublish` e aguarda; o teste
mata a autoridade nesse ponto. O delta TXID 2 existe com watermark em 1. O
cliente cooperativo não consegue conectar enquanto a autoridade está ausente;
após reinício, `WRITE` e `OPEN` são recusados. Uma nova comparação de evidência
e uma chamada explícita de `capture_once` reconciliam o spool até watermark 2,
sem alterar os bytes do delta; a autoridade ainda recusa escrita até abertura
explícita. O restore em caminho novo passa `integrity_check` e contém a mesma
contagem de linhas da fonte sintética, que permanece inalterada.

Esse teste mata o processo após a publicação LTX pela API de spool, **não**
interrompe uma chamada real a `capture_once` no hook interno. Também não cobre
power loss, corrupção, checkpoint concorrente, sincronização remota, efeito
externo, identidade da fonte ou escritores que ignorem a autoridade. Os
marcadores e a evidência são locais e sintéticos; o resultado não qualifica
o protocolo para uso operacional.

Esse cenário foi ampliado após o reinício: uma evidência sintética divergente
resulta em `MISMATCH`, e um arquivo de evidência ausente retorna
`EVIDENCE_UNAVAILABLE`; em ambos `WRITE` e `OPEN` continuam recusados. Antes
de fornecer novamente a evidência do teste, são comparados snapshot e delta LTX
em bytes, watermark e WAL da fonte; nenhum TXID 3 aparece e a fonte mantém suas
linhas. A reconciliação e abertura explícitas só ocorrem depois. A ausência de
evidência inicialmente causava panic na thread do fixture e conexão sem
resposta; o teste de regressão reproduziu isso antes do tratamento explícito.

Os casos de divergência e ausência não testam autenticidade, integridade
criptográfica, frescor ou custódia independente da evidência. Restaurar a
evidência é uma ação controlada pelo teste, não procedimento seguro de
recuperação em produção; o ponto de falha continua na API de spool direta, não
dentro do hook privado de `capture_once`.

Em outro teste, o delta TXID 2 sintético é truncado após a autoridade ser morta
no hook de publicação. A autoridade reiniciada permanece fechada; a evidência
da fonte confere, mas a reconciliação com `capture_once` retorna `FAILED`, sem
avançar o watermark TXID 1, publicar TXID 3 ou admitir escrita. Os bytes do
snapshot anterior, o delta truncado, as linhas e o WAL da fonte são comparados
antes e depois. A tentativa de restaurar TXID 1 do **mesmo repositório** também
falha ao encontrar o delta posterior truncado e não publica destino. Copiar os
bytes intactos do snapshot para um repositório temporário separado permitiu
restaurar TXID 1 com uma linha e `integrity_check` bem-sucedido. Isso demonstra
integridade do snapshot isolado, não disponibilidade direta do ponto anterior
no repositório corrompido nem um procedimento aprovado de reparo. O teste não
modifica ou remove o delta corrompido original. A morte continua no hook da
API de spool direta, não no hook interno de `capture_once`.

Um teste separado cria snapshot TXID 1 e deltas TXID 2 e 3 a partir de SQLite
real com linhas sintéticas; após truncar apenas o delta 3, o restore direto de
TXID 2 do mesmo repositório falha sem criar destino. O teste copia os arquivos
conhecidos do prefixo 1–2 para um repositório temporário novo e pede à biblioteca
o restore de TXID 2: as duas linhas esperadas e `integrity_check` são verificados.
Truncar então o delta **na cópia isolada** faz o restore falhar sem publicar
destino. O teste compara snapshot, delta anterior, delta corrompido, watermark e
WAL originais antes e depois e confirma as três linhas na fonte.

A seleção do prefixo é conhecida antecipadamente pelo teste; não há descoberta
automática do último ponto íntegro, nem prova de que seja o ponto mais recente
ou autorizado. O watermark original permanece em TXID 3 e não é reparado. A
cópia e o restore isolados são experimento sintético, não runbook para spool
real, não qualificam cadeia parcial arbitrária, recursos limitados, metadados,
identidade, credenciais ou recuperação de host. Nenhum arquivo do repositório
corrompido é removido para obter esse resultado.

Uma varredura **somente no fixture de teste** investiga a seleção automática de
um prefixo: enumera snapshot TXID 1 e deltas de um único TXID, copia cada
prefixo candidato para diretório temporário externo ao spool e usa restore da
biblioteca mais `integrity_check` para validá-lo; para na primeira lacuna ou
falha. Em cadeias sintéticas de três posições, identifica 3 quando íntegra, 2
com delta 3 truncado e 1 com delta 2 truncado ou ausente. Snapshot truncado é
recusado. Cada prefixo identificado é restaurado em outro arquivo novo e suas
linhas são verificadas; os arquivos LTX de entrada e o spool/fonte originais
são comparados antes e depois.

Esse algoritmo não é ferramenta de produção nem descobre necessariamente o
último ponto recuperável de um repositório geral: aceita somente snapshot TXID 1
e arquivos com um TXID por delta, até 16 objetos e 16 MiB por arquivo. Não
interpreta compactações, múltiplas gerações ou metadados remotos, nem protege
contra alteração concorrente, TOCTOU, substituição de diretórios, armazenamento
hostil ou expansão descomprimida; limites de entrada não são limites completos
de recursos. A integridade estrutural não prova identidade, autorização, frescor
ou completude do negócio. O teste não altera o watermark nem habilita uso de um
spool real, e não estabelece um procedimento de reparo ou recuperação de host.

**Não é um gate de produção nem uma decisão final de arquitetura.** CLI, web,
stdio, servidor, métricas, auditoria, OAuth e recuperação ainda não o usam;
um processo não cooperativo pode escrever diretamente no banco. O fixture não
garante proteção contra substituição de arquivos/inodes, symlinks, permissões
incorretas, múltiplos hosts ou filesystems de rede; não qualifica semântica de
lock em outras plataformas, `fork`, perda física de energia, corrupção parcial
ou durabilidade do diretório. Também não define inicialização fail-closed,
timeouts, tratamento de falha de `fsync`, starvation, limites de espera,
reconciliação após captura ou uma liberação condicionada à publicação durável.
O encerramento do processo libera locks do sistema, mas não comprova que seu
efeito externo ou seu estado de captura esteja consistente. Antes de uma
integração operacional, é necessário especificar e testar esses casos, integrar
todos os escritores e o write-gate real da biblioteca, além de publicar e fixar
uma revisão reproduzível dela.

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
